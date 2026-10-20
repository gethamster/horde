//! Operator-owned, revocable authority for nightly upstream maintenance.
//! This does not authorize human acceptance or bypass delivery qualification.
use crate::{
    management,
    store::{Store, now},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
mod authority;
mod usage;
pub use usage::{binding, check_local, consume_output, inspection, pin, reserve_model};
#[cfg(test)]
mod tests;
const PREFIX: &str = "upstream-maintenance.policy:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub reference: String,
    pub max_model_calls: u64,
    pub max_input_bytes: u64,
    pub max_output_bytes: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub schema_version: u32,
    pub policy_id: String,
    pub policy_revision: u64,
    pub project_id: String,
    pub tenant_id: String,
    pub repo: PathBuf,
    pub repository: String,
    pub base: String,
    pub automation_grant_id: String,
    pub routine_id: String,
    pub expires_at: i64,
    pub budget: Budget,
    pub authority_url: String,
    pub authority_token_env: String,
    #[serde(default)]
    pub allow_private_http: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub schema_version: u32,
    pub policy_id: String,
    pub policy_revision: u64,
    pub automation_grant_id: String,
    pub routine_id: String,
    pub occurrence_id: String,
    pub invocation_id: String,
    pub expires_at: i64,
    pub upstreams: BTreeMap<String, String>,
    pub budget: Budget,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registered {
    policy: Policy,
    revoked: bool,
}
fn label(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 256
            && value.trim() == value
            && !value.chars().any(char::is_control),
        "invalid maintenance identifier"
    );
    Ok(())
}
fn validate_budget(budget: &Budget) -> Result<()> {
    label(&budget.reference)?;
    ensure!(
        budget.max_model_calls > 0 && budget.max_input_bytes > 0 && budget.max_output_bytes > 0,
        "maintenance budget must be positive"
    );
    Ok(())
}
fn load(db: &Store, id: &str) -> Result<Registered> {
    label(id)?;
    serde_json::from_str(
        &management::value(db, &format!("{PREFIX}{id}"))?
            .context("maintenance policy is not registered")?,
    )
    .context("invalid registered maintenance policy")
}
pub fn register(db: &Store, policy: Policy) -> Result<Value> {
    for value in [
        &policy.policy_id,
        &policy.project_id,
        &policy.tenant_id,
        &policy.repository,
        &policy.base,
        &policy.automation_grant_id,
        &policy.routine_id,
    ] {
        label(value)?;
    }
    ensure!(
        policy.schema_version == 1 && policy.policy_revision > 0,
        "unsupported maintenance policy version"
    );
    validate_budget(&policy.budget)?;
    ensure!(policy.expires_at > now(), "maintenance policy has expired");
    ensure!(
        crate::projects::resolve(db, &policy.project_id)? == policy.project_id
            && crate::projects::tenant(db, &policy.project_id)? == policy.tenant_id,
        "maintenance project/tenant mismatch"
    );
    ensure!(
        policy.repo.is_absolute() && policy.repo.canonicalize()? == policy.repo,
        "maintenance repository must be canonical"
    );
    ensure!(
        policy.repository.split('/').count() == 2
            && policy
                .repository
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b)),
        "invalid maintenance repository identity"
    );
    crate::git::run(
        &policy.repo,
        &["check-ref-format", "--branch", &policy.base],
    )?;
    authority::validate_url(&policy)?;
    let key = format!("{PREFIX}{}", policy.policy_id);
    db.atomic(|| {
        if let Some(existing)=management::value(db,&key)? {
            let old:Registered=serde_json::from_str(&existing)?;
            if old.policy==policy {return get(db,&policy.policy_id,&policy.project_id);}
            ensure!(policy.policy_revision>old.policy.policy_revision,"maintenance policy revision must advance");
        }
        management::set(db,&key,&serde_json::to_string(&Registered {policy:policy.clone(),revoked:false})?)?;
        management::event(db,"maintenance.policy_registered",json!({"policy_id":policy.policy_id,"policy_revision":policy.policy_revision,"project_id":policy.project_id}))?;
        get(db,&policy.policy_id,&policy.project_id)
    })
}
pub fn get(db: &Store, id: &str, project: &str) -> Result<Value> {
    let registered = load(db, id)?;
    ensure!(
        registered.policy.project_id == project,
        "maintenance policy project scope violation"
    );
    Ok(
        json!({"active":!registered.revoked && registered.policy.expires_at>now(),"policy":registered.policy,"revoked":registered.revoked}),
    )
}
pub fn revoke(db: &Store, id: &str, revision: u64, project: &str) -> Result<Value> {
    db.atomic(|| {
        let old=load(db,id)?;
        ensure!(old.policy.project_id==project && old.policy.policy_revision==revision,"maintenance policy scope/revision mismatch");
        management::set(db,&format!("{PREFIX}{id}"),&serde_json::to_string(&Registered {policy:old.policy,revoked:true})?)?;
        let roots=db.rows("SELECT task FROM external_ops WHERE name='upstream-maintenance.binding' AND json_extract(data,'$.policy_id')=? AND json_extract(data,'$.policy_revision')=?",&[&id,&revision])?;
        for root in roots {
            let root=root["task"].as_str().context("maintenance task")?;
            crate::protocol::dispatch(db,"cancel",json!({"task":root,"project":project}),None)?;
        }
        management::event(db,"maintenance.policy_revoked",json!({"policy_id":id,"policy_revision":revision,"project_id":project}))?;
        get(db,id,project)
    })
}
pub fn validate_binding(
    db: &Store,
    binding: &Binding,
    project: &str,
    repo: &Path,
) -> Result<Policy> {
    for v in [
        &binding.policy_id,
        &binding.automation_grant_id,
        &binding.routine_id,
        &binding.occurrence_id,
        &binding.invocation_id,
    ] {
        label(v)?;
    }
    validate_budget(&binding.budget)?;
    ensure!(
        binding.schema_version == 1,
        "unsupported maintenance binding version"
    );
    let registered = load(db, &binding.policy_id)?;
    let policy = registered.policy;
    ensure!(
        !registered.revoked
            && binding.expires_at > now()
            && binding.expires_at <= policy.expires_at,
        "maintenance authority is revoked or expired"
    );
    ensure!(
        binding.policy_revision == policy.policy_revision
            && binding.automation_grant_id == policy.automation_grant_id
            && binding.routine_id == policy.routine_id,
        "maintenance authority identity changed"
    );
    ensure!(
        project == policy.project_id
            && crate::projects::tenant(db, project)? == policy.tenant_id
            && repo.canonicalize()? == policy.repo,
        "maintenance repository/project scope mismatch"
    );
    ensure!(
        binding.budget.reference == policy.budget.reference
            && binding.budget.max_model_calls <= policy.budget.max_model_calls
            && binding.budget.max_input_bytes <= policy.budget.max_input_bytes
            && binding.budget.max_output_bytes <= policy.budget.max_output_bytes,
        "maintenance worker budget exceeds policy"
    );
    ensure!(
        binding.upstreams.len() == 3
            && ["vercel/ai", "vercel/chat", "vercel/eve"]
                .iter()
                .all(|repo| binding.upstreams.get(*repo).is_some_and(
                    |sha| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit())
                )),
        "maintenance upstream snapshot must pin all three approved repositories"
    );
    Ok(policy)
}
/// Policy checks at synchronous RPC boundaries run HTTP on a separate runtime so
/// the caller never nests a Tokio runtime. No caller-controlled URL is used.
pub fn authorize_submission(db: &Store, args: &Value, repo: &Path) -> Result<Option<Binding>> {
    let Some(value) = args.get("upstream_maintenance") else {
        return Ok(None);
    };
    let binding: Binding = serde_json::from_value(value.clone())?;
    let project = args["project"]
        .as_str()
        .context("maintenance project required")?;
    let policy = validate_binding(db, &binding, project, repo)?;
    ensure!(
        args["on"].is_null() || args["on"] == "local",
        "remote maintenance execution is not budget-qualified"
    );
    authority::online_sync(&policy, &binding)?;
    validate_binding(db, &binding, project, repo)?;
    Ok(Some(binding))
}
pub fn check_online(
    db: &Store,
    task: &str,
) -> impl std::future::Future<Output = Result<()>> + Send + use<> {
    let admitted = (|| -> Result<Option<(Policy, Binding)>> {
        let Some((_, binding)) = binding(db, task)? else {
            return Ok(None);
        };
        let policy = check_local(db, task)?.context("maintenance binding")?;
        ensure!(binding.expires_at > now(), "maintenance invocation expired");
        Ok(Some((policy, binding)))
    })();
    let root = db.root.clone();
    let task = task.to_owned();
    async move {
        if let Some((policy, binding)) = admitted? {
            authority::online(&policy, &binding).await?;
            // Local revocation may have committed while the HTTP reply was in flight.
            check_local(&Store::open(&root)?, &task)?;
        }
        Ok(())
    }
}
pub fn check_online_sync(db: &Store, task: &str) -> Result<()> {
    if let Some(policy) = check_local(db, task)? {
        let (_, binding) = binding(db, task)?.context("maintenance binding")?;
        authority::online_sync(&policy, &binding)?;
        check_local(db, task)?;
    }
    Ok(())
}
pub fn dispatch(db: &Store, name: &str, args: &Value) -> Result<Option<Value>> {
    let result = match name {
        "maintenance_policy_register" => {
            register(db, serde_json::from_value(args["policy"].clone())?)?
        }
        "maintenance_policy_get" => get(
            db,
            args["policy_id"].as_str().context("policy_id required")?,
            args["project"].as_str().context("project required")?,
        )?,
        "maintenance_policy_revoke" => revoke(
            db,
            args["policy_id"].as_str().context("policy_id required")?,
            args["policy_revision"]
                .as_u64()
                .context("policy_revision required")?,
            args["project"].as_str().context("project required")?,
        )?,
        _ => return Ok(None),
    };
    Ok(Some(result))
}
/// Enforce the same policy while a provider call or command is in flight. Dropping
/// the work uses the existing process-group cleanup and uncertain-effect records.
pub async fn supervise<F>(db: &Store, task: &str, work: F) -> Result<Value>
where
    F: std::future::Future<Output = Result<Value>>,
{
    if binding(db, task)?.is_none() {
        return work.await;
    }
    tokio::pin!(work);
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result=&mut work=>return result,
            _=tick.tick()=>check_online(db,task).await?,
        }
    }
}
pub fn validate_settings(
    db: &Store,
    binding: &Binding,
    settings: &crate::config::Settings,
) -> Result<()> {
    let policy = load(db, &binding.policy_id)?.policy;
    if settings.delivery.enabled {
        ensure!(
            settings.delivery.repository == policy.repository
                && settings.delivery.base == policy.base,
            "maintenance delivery repository/base mismatch"
        );
        ensure!(
            !settings.delivery.merge || settings.automatic_delivery.enabled,
            "maintenance automatic merge requires qualified operator-owned delivery policy"
        );
    }
    Ok(())
}
