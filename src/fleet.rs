//! Provider-owned resources and durable administrative operations.
use crate::{
    management,
    store::{Store, now},
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf};
mod project_host;
mod providers;
use project_host::{
    admit_local_vm, bootstrap_path, persist_bootstrap, runtime_visible, send_host_operation,
};
pub use project_host::{remote_command, reserved_local_cpus};
pub use providers::kubernetes_manifest;
use providers::{lifecycle, provision};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
    pub provider: String,
    pub project: String,
    pub host: Option<String>,
    pub lima_user: String,
    pub lima_home: PathBuf,
    pub lima_guard: PathBuf,
    pub lima_image_digest: String,
    pub lima_horde_binary: PathBuf,
    pub lima_egress: Vec<String>,
    pub endpoint: String,
    pub api_key_env: String,
    pub image: String,
    pub context: String,
    pub namespace: String,
    pub concurrency: usize,
    pub cpus: u32,
    pub memory_mb: u32,
    pub disk_gb: u32,
    pub lifetime_seconds: u32,
    pub peer: Option<String>,
    pub executor_roles: Vec<String>,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            provider: "docker".into(),
            project: "default".into(),
            host: None,
            lima_user: String::new(),
            lima_home: PathBuf::new(),
            lima_guard: "/usr/local/libexec/horde-lima-guard".into(),
            lima_image_digest: String::new(),
            lima_horde_binary: PathBuf::new(),
            lima_egress: vec![],
            endpoint: String::new(),
            api_key_env: String::new(),
            image: String::new(),
            context: "default".into(),
            namespace: "default".into(),
            concurrency: 4,
            cpus: 2,
            memory_mb: 2048,
            disk_gb: 10,
            lifetime_seconds: 3600,
            peer: None,
            executor_roles: vec![],
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub profiles: BTreeMap<String, Profile>,
    pub capacity_policy: CapacityPolicy,
    pub budgets: Vec<Budget>,
    pub capacity_commands: Vec<Vec<String>>,
    pub issuer_key: Option<PathBuf>,
    pub controller_address: Option<std::net::SocketAddr>,
    pub controller_tls_name: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CapacityPolicy {
    pub warn_percent: f64,
    pub switch_percent: f64,
    pub stale_seconds: i64,
}
impl Default for CapacityPolicy {
    fn default() -> Self {
        Self {
            warn_percent: 80.0,
            switch_percent: 90.0,
            stale_seconds: 300,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub account: String,
    pub since: i64,
    pub reset_at: i64,
    pub tokens: Option<u64>,
    pub usd: Option<f64>,
}
pub fn load() -> Result<Config> {
    let path = crate::config::Settings::user_path().with_file_name("runtimes.toml");
    if !path.exists() {
        return Ok(Config::default());
    }
    let config: Config = toml::from_str(&std::fs::read_to_string(path)?)?;
    let p = &config.capacity_policy;
    ensure!(
        p.warn_percent.is_finite()
            && p.switch_percent.is_finite()
            && p.warn_percent > 0.0
            && p.warn_percent < p.switch_percent
            && p.switch_percent <= 100.0
            && p.stale_seconds > 0,
        "invalid capacity thresholds"
    );
    for b in &config.budgets {
        ensure!(
            b.since >= 0
                && b.reset_at > b.since
                && b.tokens.is_none_or(|v| v > 0)
                && b.usd.is_none_or(|v| v.is_finite() && v > 0.0),
            "invalid account budget"
        );
    }
    Ok(config)
}
impl Profile {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            [
                "docker",
                "kubernetes",
                "e2b",
                "daytona",
                "tailscale",
                "lima"
            ]
            .contains(&self.provider.as_str()),
            "unsupported runtime provider"
        );
        ensure!(
            (1..=64).contains(&self.concurrency)
                && self.cpus > 0
                && self.memory_mb >= 64
                && self.disk_gb > 0
                && self.lifetime_seconds > 0,
            "invalid runtime limits"
        );
        ensure!(
            self.provider == "tailscale" || !self.image.is_empty(),
            "runtime image or template required"
        );
        if ["docker", "kubernetes"].contains(&self.provider.as_str()) {
            ensure!(
                self.image.contains("@sha256:"),
                "container image must be pinned by digest"
            );
        }
        if self.provider == "lima" {
            crate::lima::validate(self)?;
        }
        if !self.endpoint.is_empty() {
            let u = reqwest::Url::parse(&self.endpoint)?;
            ensure!(
                u.scheme() == "https"
                    || (u.scheme() == "http"
                        && [Some("127.0.0.1"), Some("localhost")].contains(&u.host_str())),
                "provider endpoint requires HTTPS"
            );
            ensure!(
                u.username().is_empty() && u.password().is_none(),
                "credentials belong in api_key_env"
            );
        }
        Ok(())
    }
}
fn identifier(s: &str) -> Result<()> {
    ensure!(
        s != "local"
            && !s.is_empty()
            && s.len() <= 48
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !s.starts_with('-'),
        "runtime ID must be 1-48 lowercase letters, digits or hyphens; local is reserved"
    );
    Ok(())
}
pub fn dispatch(db: &Store, name: &str, args: &Value) -> Result<Option<Value>> {
    if name == "runtime_list" {
        let rows = db.rows("SELECT r.id,r.profile,r.resource,r.state,r.version,r.created,r.error,p.observed AS last_seen,p.status AS runtime_status FROM managed_runtimes r LEFT JOIN runtime_presence p ON p.runtime=r.id WHERE r.state!='removed'
UNION ALL SELECT m.runtime,'fleet:' || k.name,NULL,CASE WHEN m.state='revoked' THEN 'revoked' WHEN c.expires<=? THEN 'expired' WHEN p.observed>? THEN 'ready' ELSE 'offline' END,json_extract(p.status,'$.version'),m.created,NULL,p.observed,p.status
FROM fleet_enrollment_members m JOIN fleet_enrollment_keys k ON k.id=m.key_id LEFT JOIN fleet_enrollment_certificates c ON c.fingerprint=m.current_fingerprint LEFT JOIN runtime_presence p ON p.runtime=m.runtime WHERE NOT EXISTS(SELECT 1 FROM runtime_settings s WHERE s.key='runtime_removed:' || m.runtime AND s.value='true') ORDER BY created",&[&now(),&(now()-30)])?;
        let project = args["project"]
            .as_str()
            .map(|p| crate::projects::resolve(db, p))
            .transpose()?;
        let rows = rows
            .into_iter()
            .filter_map(|row| {
                if args["all_projects"] == true {
                    return Some(Ok(row));
                }
                let Some(project) = project.as_deref() else {
                    return Some(Ok(row));
                };
                let Some(id) = row["id"].as_str() else {
                    return Some(Err(anyhow::anyhow!("runtime id")));
                };
                match runtime_visible(db, project, id) {
                    Ok(true) => Some(Ok(row)),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .collect::<Result<Vec<_>>>()?;
        let rows = rows
            .into_iter()
            .map(|row| {
                let id = row["id"].as_str().context("runtime id")?;
                let name = crate::runtime_directory::display_name(db, id)?;
                let mut fields = row.as_object().context("runtime row")?.clone();
                fields.insert("name".into(), json!(name));
                Ok(Value::Object(fields))
            })
            .collect::<Result<Vec<_>>>()?;
        return Ok(Some(json!(rows)));
    }
    if name == "runtime_inspect" {
        let id = args["id"].as_str().context("id required")?;
        if let Some(project) = args["project"].as_str() {
            ensure!(
                runtime_visible(db, &crate::projects::resolve(db, project)?, id)?,
                "runtime is not granted to this project"
            );
        }
        return Ok(Some(
            json!({"name":crate::runtime_directory::display_name(db,id)?,"runtime":db.rows("SELECT * FROM managed_runtimes WHERE id=?",&[&id])?,"fleet_membership":db.rows("SELECT m.runtime,m.key_id,k.name AS fleet,m.state,m.created,c.expires,c.renew_after FROM fleet_enrollment_members m JOIN fleet_enrollment_keys k ON k.id=m.key_id LEFT JOIN fleet_enrollment_certificates c ON c.fingerprint=m.current_fingerprint WHERE m.runtime=?",&[&id])?,"operations":db.rows("SELECT * FROM runtime_operations WHERE runtime=? ORDER BY created",&[&id])?.iter().map(management::operation_receipt).collect::<Result<Vec<_>>>()?}),
        ));
    }
    if ![
        "runtime_create",
        "runtime_destroy",
        "runtime_restart",
        "runtime_update",
        "runtime_skills_update",
        "runtime_stop",
        "runtime_start",
        "runtime_reconcile",
    ]
    .contains(&name)
    {
        return Ok(None);
    }
    db.atomic(|| enqueue_operation(db, name, args))
}

fn enqueue_operation(db: &Store, name: &str, args: &Value) -> Result<Option<Value>> {
    let selector = args["id"].as_str().context("id required")?;
    let operation = args["request_id"]
        .as_str()
        .context("request_id required for retry-safe management")?;
    ensure!(
        !operation.is_empty() && operation.len() <= 256,
        "invalid request_id"
    );
    ensure!(
        args.get("packet").is_none(),
        "skill packets are captured from the controller catalog"
    );
    let old = db.rows("SELECT * FROM runtime_operations WHERE id=?", &[&operation])?;
    if let Some(old) = old.first() {
        let stored: Value = serde_json::from_str(old["args"].as_str().context("operation args")?)?;
        let intent = if old["action"] == "runtime_skills_update" {
            let mut intent = stored.as_object().context("operation args")?.clone();
            intent.remove("packet");
            Value::Object(intent)
        } else {
            stored
        };
        ensure!(
            old["action"] == name && intent == *args,
            "request ID reused with different operation"
        );
        return Ok(Some(management::operation_receipt(old)?));
    }
    let id = if name == "runtime_create" {
        identifier(selector)?;
        selector.to_owned()
    } else if remote_management(name) {
        crate::runtime_directory::resolve(db, selector)?
    } else {
        crate::runtime_directory::resolve_known(db, selector)?
    };
    if name != "runtime_create"
        && let Some(row) = db
            .rows("SELECT spec FROM managed_runtimes WHERE id=?", &[&id])?
            .first()
    {
        let spec: Profile = serde_json::from_str(row["spec"].as_str().context("runtime spec")?)?;
        if let Some(requested) = args["project"].as_str() {
            ensure!(
                crate::projects::resolve(db, requested)?
                    == crate::projects::resolve(db, &spec.project)?,
                "runtime belongs to another project"
            );
        }
    }
    let body = if name == "runtime_skills_update" {
        let mut payload = args.as_object().context("operation args")?.clone();
        payload.insert(
            "packet".into(),
            serde_json::to_value(crate::skill_catalog::load_for(&db.root)?)?,
        );
        Value::Object(payload).to_string()
    } else {
        args.to_string()
    };
    if name == "runtime_update" {
        crate::update::validate_version(args["version"].as_str().context("version required")?)?;
    }
    db.atomic(|| {
        if name == "runtime_create" {
            let profile = args["profile"].as_str().context("profile required")?;
            let config = load()?;
            let configured = config.profiles.get(profile).context("unknown runtime profile")?;
            let project = crate::projects::resolve(db, &configured.project)?;
            let requested = crate::projects::resolve(db, args["project"].as_str().unwrap_or("default"))?;
            ensure!(project == requested, "runtime profile belongs to another project");
            let spec = Profile { project: project.clone(), ..configured.clone() };
            spec.validate()?;
            if spec.provider == "lima" && spec.host.is_none() {
                admit_local_vm(db, &spec)?;
            }
            if let Some(host) = spec.host.as_deref() {
                ensure!(host != selector && crate::projects::runtime_allowed(db, &project, host)?, "host is not granted to project");
            }
            crate::projects::bind_runtime(db, &project, &id)?;
            ensure!(spec.provider != "tailscale", "use horde network add to enroll Tailscale hosts");
            db.conn.execute("INSERT INTO managed_runtimes(id,profile,spec,state,created) VALUES(?,?,?,'requested',?)",params![id,profile,serde_json::to_string(&spec)?,now()])?;
        } else if db.rows("SELECT id FROM managed_runtimes WHERE id=?", &[&id])?.is_empty() {
            ensure!(remote_management(name), "provider lifecycle is unavailable for independently enrolled workers");
            ensure!(crate::fleet_enrollment::authority::is_active(db, &id)?, "runtime must have an active enrollment for remote management");
        }
        db.conn.execute("INSERT INTO runtime_operations(id,runtime,action,args,state,created) VALUES(?,?,?,?,'pending',?)",params![operation,id,name,body,now()])?;
        management::event(db,"runtime.operation",json!({"id":operation,"runtime":id,"action":name}))?;
        Ok(())
    })?;
    Ok(Some(json!({"request_id":operation,"state":"pending"})))
}
fn remote_management(action: &str) -> bool {
    ["runtime_update", "runtime_restart", "runtime_skills_update"].contains(&action)
}

async fn send_management(db: &Store, op: &Value, peer: &str) -> Result<Value> {
    let request = op["id"].as_str().context("operation")?;
    let action = op["action"].as_str().context("action")?;
    let args: Value = serde_json::from_str(op["args"].as_str().context("args")?)?;
    let payload = if action == "runtime_skills_update" {
        let inventory = crate::capabilities::inventory(db)?;
        let supported = inventory["runtimes"].as_array().is_some_and(|runtimes| {
            runtimes.iter().any(|runtime| {
                runtime["runtime"] == peer
                    && runtime["protocol"]["features"]
                        .as_array()
                        .is_some_and(|features| {
                            features
                                .iter()
                                .any(|feature| feature == "runtime_skills_update")
                        })
            })
        });
        if !supported {
            return Ok(
                json!({"state":"blocked","reason":"worker has not advertised skill updates; update its Horde binary first and wait for a capability heartbeat","request_id":request}),
            );
        }
        json!({"request_id":request,"action":action,"packet":args["packet"]})
    } else {
        json!({"request_id":request,"action":action,"version":args["version"]})
    };
    let config = crate::federation::config(db)?;
    match crate::federation::call(&config, peer, "manage", payload).await {
        Ok(result) => Ok(result),
        Err(error) => Ok(
            json!({"state":if now()>op["created"].as_i64().unwrap_or(0)+1800{"blocked"}else{"waiting"},"reason":error.to_string(),"request_id":request}),
        ),
    }
}

async fn command(argv: &[String], input: Option<&Value>) -> Result<Value> {
    use tokio::io::AsyncWriteExt;
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn()?;
    if let Some(input) = input {
        child
            .stdin
            .take()
            .context("stdin")?
            .write_all(input.to_string().as_bytes())
            .await?;
    } else {
        drop(child.stdin.take());
    }
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        child.wait_with_output(),
    )
    .await
    .context("provider command timed out")??;
    ensure!(
        output.status.success(),
        "provider command failed ({})",
        output.status
    );
    ensure!(
        output.stdout.len() <= 4 * 1024 * 1024,
        "provider output too large"
    );
    if output.stdout.is_empty() {
        Ok(json!({}))
    } else {
        Ok(serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| json!({"output":String::from_utf8_lossy(&output.stdout).trim()})))
    }
}
fn docker(p: &Profile, rest: Vec<String>) -> Vec<String> {
    let mut a = vec!["docker".into(), "--context".into(), p.context.clone()];
    a.extend(rest);
    a
}
fn kube(p: &Profile, rest: Vec<String>) -> Vec<String> {
    let mut a = vec![
        "kubectl".into(),
        "--context".into(),
        p.context.clone(),
        "--namespace".into(),
        p.namespace.clone(),
    ];
    a.extend(rest);
    a
}
pub fn recover_operations(db: &Store) -> Result<()> {
    db.atomic(|| {
        let interrupted_update: bool = db.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM runtime_operations WHERE state='running' AND runtime!='local' AND action='runtime_update')", [], |r| r.get(0))?;
        if interrupted_update {
            management::set(db, "fleet_updates_paused", "true")?;
        }
        db.conn.execute("UPDATE runtime_operations SET state='waiting' WHERE state='running' AND runtime!='local' AND action='runtime_skills_update'", [])?;
        db.conn.execute("UPDATE runtime_operations SET state='uncertain' WHERE state='running' AND runtime!='local'", [])?;
        Ok(())
    })
}

pub async fn tick(db: &Store) -> Result<()> {
    db.conn.execute("UPDATE managed_runtimes SET error='bootstrap enrollment expired before activation; inspect retained guest and recreate with a new runtime identity' WHERE state NOT IN ('removed','ready') AND error IS NULL AND EXISTS(SELECT 1 FROM runtime_enrollments e WHERE e.runtime=managed_runtimes.id AND e.state='pending' AND e.expires<=?)", [now()])?;
    let paused =
        i64::from(management::value(db, "fleet_updates_paused")?.as_deref() == Some("true"));
    let operations = db.rows(
        "SELECT * FROM runtime_operations WHERE state IN ('pending','waiting') AND runtime!='local' AND (?=0 OR action!='runtime_update') ORDER BY created,rowid LIMIT 1",
        &[&paused],
    )?;
    let Some(op) = operations.first() else {
        return Ok(());
    };
    let request = op["id"].as_str().context("operation")?;
    let id = op["runtime"].as_str().context("runtime")?;
    if db.conn.execute(
        "UPDATE runtime_operations SET state='running' WHERE id=? AND state IN ('pending','waiting')",
        [request],
    )? != 1
    {
        return Ok(());
    }
    let action = op["action"].as_str().context("action")?;
    let operation=async{
        let managed = db.rows("SELECT * FROM managed_runtimes WHERE id=?", &[&id])?;
        let Some(row) = managed.first() else {
            ensure!(remote_management(action), "provider lifecycle requires a managed runtime");
            ensure!(crate::fleet_enrollment::authority::is_active(db, id)?, "runtime must have an active enrollment for remote management");
            return send_management(db, op, id).await;
        };
        ensure!(row["state"] != "removed", "runtime has been removed");
        let p:Profile=serde_json::from_str(row["spec"].as_str().context("spec")?)?;p.validate()?;
        if let Some(host) = p.host.as_deref() {
            return send_host_operation(db, op, &p, host).await;
        }
        if action=="runtime_create"{
            let incoming = bootstrap_path(db,&p.project,id)?;
            let bootstrap = if incoming.exists() {
                Some(serde_json::from_slice(&std::fs::read(incoming)?)?)
            } else {
                let packet = crate::enrollment::issue(db,id,&p)?;
                if let Some(packet) = &packet { persist_bootstrap(db,&p.project,id,packet)?; }
                packet
            };
            let resource=provision(db,&p,id,bootstrap.as_ref()).await?;
            db.conn.execute("UPDATE managed_runtimes SET resource=?,state=CASE WHEN state='ready' THEN state ELSE 'provisioned' END WHERE id=?",params![resource,id])?;
            Ok(json!({"resource":resource,"state":"provisioned","enrollment_required":p.peer.is_none()}))
        }else if remote_management(action){
            let enrollments = db.rows("SELECT state FROM runtime_enrollments WHERE runtime=?", &[&id])?;
            if !enrollments.is_empty() {
                ensure!(crate::fleet_enrollment::authority::is_active(db,id)?, "runtime must have an active enrollment for remote management");
            }
            let peer = p.peer.clone().or_else(|| (!enrollments.is_empty()).then(|| id.to_owned())).context("runtime must be enrolled and have a peer mapping before remote management")?;
            if action=="runtime_update"&&["docker","kubernetes"].contains(&p.provider.as_str()) {
                let config=crate::federation::config(db)?;
                let args:Value=serde_json::from_str(op["args"].as_str().context("args")?)?;
                container_update(db,&p,id,row,op,&config,&peer,&args).await
            } else { send_management(db,op,&peer).await }

        }else{
            let input:Value=serde_json::from_str(op["args"].as_str().context("operation args")?)?;
            let recovery_resource = if action == "runtime_destroy" && p.provider == "lima" && row["resource"].is_null() {
                Some(crate::lima::owned_resource(&db.root, &p, id)?)
            } else { None };
            let resource=if action=="runtime_reconcile"{input["resource"].as_str().context("resource required for reconciliation")?}else{row["resource"].as_str().or(recovery_resource.as_deref()).context("resource ID unavailable; reconcile provisioning first")?};
            let result=if p.provider=="tailscale" {
                ensure!(action=="runtime_destroy", "Tailscale hosts support update, restart, and destroy (unenroll); host power and provisioning remain user-owned");
                json!({"unenrolled":true,"host_retained":true})
            }else{lifecycle(db,&p,id,resource,action).await?};
            if action=="runtime_reconcile" {db.conn.execute("UPDATE managed_runtimes SET resource=?,error=NULL WHERE id=?",params![resource,id])?;}
            if action=="runtime_destroy" {db.conn.execute("UPDATE runtime_enrollments SET state='revoked',token_hash='' WHERE runtime=?",[id])?;}
            db.conn.execute("UPDATE managed_runtimes SET state=? WHERE id=?",params![match action{"runtime_destroy"=>"removed","runtime_stop"=>"stopped","runtime_reconcile" if p.provider == "lima" => result["state"].as_str().unwrap_or("uncertain"),_=>"provisioned"},id])?;Ok(result)
        }
    }.await;
    match operation {
        Ok(result) => {
            let state = if remote_management(action) || result["host_managed"] == true {
                match result["state"].as_str() {
                    Some("succeeded") => "succeeded",
                    Some("failed" | "blocked" | "uncertain") => "failed",
                    _ => "waiting",
                }
            } else {
                "succeeded"
            };
            if action == "runtime_update" && state == "failed" {
                management::set(db, "fleet_updates_paused", "true")?;
            }
            db.conn.execute(
                "UPDATE runtime_operations SET state=?,result=? WHERE id=?",
                params![state, result.to_string(), request],
            )?;
            management::event(
                db,
                "runtime.operation_progress",
                json!({"request_id":request,"runtime":id,"state":state}),
            )?;
        }
        Err(error) => {
            if action == "runtime_update" {
                management::set(db, "fleet_updates_paused", "true")?;
            }
            let message = error.to_string();
            db.conn.execute(
                "UPDATE runtime_operations SET state='uncertain',result=? WHERE id=?",
                params![json!({"error":message}).to_string(), request],
            )?;
            db.conn.execute(
                "UPDATE managed_runtimes SET error=? WHERE id=?",
                params![message, id],
            )?;
            management::event(
                db,
                "runtime.operation_uncertain",
                json!({"request_id":request,"runtime":id,"error":message}),
            )?;
        }
    }
    Ok(())
}
pub async fn collect_capacity(db: &Store) -> Result<()> {
    crate::capacity::budgets(db)?;
    crate::capacity::subscriptions(db).await?;
    for argv in load()?.capacity_commands {
        ensure!(!argv.is_empty(), "capacity command cannot be empty");
        match command(&argv, None).await {
            Ok(v) => {
                for s in serde_json::from_value::<Vec<crate::capacity::Snapshot>>(v)? {
                    crate::capacity::observe(db, &s)?;
                }
            }
            Err(_) => {
                management::event(db, "account.refresh_failed", json!({"capacity":"unknown"}))?;
            }
        }
    }
    Ok(())
}
pub fn config_path() -> PathBuf {
    crate::config::Settings::user_path().with_file_name("runtimes.toml")
}

fn bootstrap_env(p: &Profile, bootstrap: Option<&Value>) -> Value {
    let mut env = json!({"HORDE_CONCURRENCY":p.concurrency.to_string()});
    if let Some(packet) = bootstrap {
        env["HORDE_BOOTSTRAP_JSON"] = json!(packet.to_string());
    }
    env
}

async fn remote_status(
    config: &crate::network::NetworkConfig,
    peer: &str,
    action: &str,
) -> Result<Value> {
    crate::federation::call(config, peer, "manage", json!({"action":action})).await
}
async fn replace_container(db: &Store, p: &Profile, id: &str, resource: &str) -> Result<()> {
    if p.provider == "docker" {
        lifecycle(db, p, id, resource, "runtime_destroy").await?;
        provision(db, p, id, None).await?;
    } else {
        let object = command(
            &kube(
                p,
                vec![
                    "get".into(),
                    "statefulset".into(),
                    resource.into(),
                    "-o".into(),
                    "json".into(),
                ],
            ),
            None,
        )
        .await?;
        ensure!(
            object["metadata"]["labels"]["task-runtime"] == id,
            "resource ownership mismatch"
        );
        command(
            &kube(
                p,
                vec![
                    "set".into(),
                    "image".into(),
                    format!("statefulset/{resource}"),
                    format!("task={}", p.image),
                ],
            ),
            None,
        )
        .await?;
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
async fn container_update(
    db: &Store,
    p: &Profile,
    id: &str,
    row: &Value,
    op: &Value,
    config: &crate::network::NetworkConfig,
    peer: &str,
    args: &Value,
) -> Result<Value> {
    let version = args["version"].as_str().context("version required")?;
    let request = op["id"].as_str().context("operation")?;
    let mut progress: Value = op["result"]
        .as_str()
        .and_then(|v| serde_json::from_str(v).ok())
        .unwrap_or(json!({}));
    if progress["image"].is_null() {
        let manifest = crate::update::release(version).await?;
        let rollback_safe = manifest.schema_max <= 2;
        let image = manifest.image.context("release has no container image")?;
        let digest = image
            .strip_prefix("ghcr.io/asomervell/horde@sha256:")
            .or_else(|| image.strip_prefix("ghcr.io/asomervell/task@sha256:"))
            .context("untrusted release image")?;
        ensure!(
            digest.len() == 64 && digest.bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid image digest"
        );
        progress = json!({"state":"waiting","phase":"draining","image":image,"rollback_safe":rollback_safe});
        db.conn.execute(
            "UPDATE runtime_operations SET result=? WHERE id=?",
            params![progress.to_string(), request],
        )?;
    }
    let resource = row["resource"].as_str().context("resource ID missing")?;
    if progress["phase"] == "draining" {
        if now() > op["created"].as_i64().unwrap_or(0) + 1800 {
            return Ok(
                json!({"state":"blocked","reason":"drain timed out; runtime remains draining"}),
            );
        }
        match remote_status(config, peer, "runtime_drain").await {
            Ok(status) if status["drained"] == true => {}
            Ok(_) => return Ok(progress),
            Err(error) => {
                progress["last_error"] = json!(error.to_string());
                return Ok(progress);
            }
        }
        let mut next = p.clone();
        next.image = progress["image"].as_str().context("image")?.into();
        progress["phase"] = json!("replacing");
        db.conn.execute(
            "UPDATE runtime_operations SET result=? WHERE id=?",
            params![progress.to_string(), request],
        )?;
        replace_container(db, &next, id, resource).await?;
        progress["phase"] = json!("health");
        progress["replaced_at"] = json!(now());
        return Ok(progress);
    }
    ensure!(
        progress["phase"] == "health",
        "container replacement is uncertain; inspect provider state before reconciliation"
    );
    if let Ok(status) = remote_status(config, peer, "runtime_status").await
        && status["version"] == version
    {
        remote_status(config, peer, "runtime_resume").await?;
        let mut next = p.clone();
        next.image = progress["image"].as_str().context("image")?.into();
        db.conn.execute(
            "UPDATE managed_runtimes SET spec=?,version=? WHERE id=?",
            params![serde_json::to_string(&next)?, version, id],
        )?;
        return Ok(json!({"state":"succeeded","version":version}));
    }
    if now() > progress["replaced_at"].as_i64().unwrap_or(0) + 120 {
        if progress["rollback_safe"] != true {
            return Ok(
                json!({"state":"blocked","reason":"new version failed health check; schema compatibility does not permit automatic rollback; runtime remains held for inspection"}),
            );
        }
        replace_container(db, p, id, resource).await?;
        return Ok(
            json!({"state":"failed","reason":"new version failed health check; previous image restored; runtime remains draining for inspection"}),
        );
    }
    Ok(progress)
}

#[cfg(test)]
mod tests;
