//! Explicit operator-only bootstrap. Durable intent never authorizes delivery.
use super::*;
use crate::{
    decision::{DecisionHttpClient, store as decisions},
    store::Store,
};
use rusqlite::params;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    frozen: Value,
    case_id: String,
    task: String,
    expected_head: String,
    request_id: String,
    project: String,
}

fn current(
    db: &Store,
    args: &Arguments,
    decision: &crate::config::Decision,
    require_running: bool,
) -> Result<()> {
    crate::projects::authorize_task(db, &args.project, &args.task)?;
    let task = db.task(&args.task)?;
    ensure!(
        task["status"] == "running"
            || !require_running && matches!(task["status"].as_str(), Some("succeeded" | "failed")),
        "task is no longer reviewable"
    );
    ensure!(
        crate::decision::review::observed_head(db, &args.task).as_deref()
            == Some(&args.expected_head),
        "advisory Run head changed"
    );
    let workspace = crate::project_runtime::task_root(db, &args.task)?
        .join("workspaces")
        .join(&args.task)
        .join("integrated");
    ensure!(
        crate::git::run(
            &workspace,
            &["status", "--porcelain", "--untracked-files=all"]
        )?
        .is_empty(),
        "advisory Run workspace is dirty"
    );
    let saved: crate::config::Settings =
        serde_json::from_str(task["settings"].as_str().context("task settings")?)?;
    let operator = crate::config::Settings::load_project_user(db, &args.project)?;
    ensure!(
        !saved.automatic_delivery.enabled && !operator.automatic_delivery.enabled,
        "bootstrap observation requires automatic delivery disabled"
    );
    ensure!(
        saved.decision == *decision && operator.decision == *decision,
        "advisory decision configuration changed"
    );
    crate::upstream_maintenance::check_local(db, &args.task)?
        .context("advisory observation requires the owning maintenance budget")?;
    Ok(())
}

fn receipt(id: &str, state: &str, data: &Value) -> Value {
    json!({"request_id":id,"state":state,"advisory":true,"enrolled":false,"observation":data.get("observation")})
}

fn current_facts(db: &Store, args: &Arguments, frozen: &Frozen, expected: &Value) -> Result<()> {
    current(db, args, &frozen.manifest.decision, true)?;
    let case = frozen
        .manifest
        .cases
        .iter()
        .find(|case| case.id == args.case_id)
        .context("frozen case missing")?;
    let snapshot = evidence::snapshot_local(db, &args.task, &case.base_commit)?;
    ensure!(
        evidence::advisory_state(
            frozen,
            case,
            &snapshot,
            &args.expected_head,
            crate::store::now()
        )? == *expected,
        "advisory source facts changed"
    );
    Ok(())
}

/// The protocol checks operator and project authority before calling this entry.
/// A new request is frozen before HTTP. Exact retries only read its saved state;
/// an interrupted or uncertain send remains held without an automatic resend.
pub fn observe(db: &Store, value: &Value) -> Result<Value> {
    ensure!(
        serde_json::to_vec(value)?.len() <= 1024 * 1024,
        "observation request exceeds 1 MiB"
    );
    let args: Arguments = serde_json::from_value(value.clone())?;
    label(&args.request_id)?;
    ensure!(
        hex(&args.expected_head, 40),
        "observation requires an exact commit"
    );
    let frozen = frozen_value(args.frozen.clone())?;
    let case = frozen
        .manifest
        .cases
        .iter()
        .find(|case| case.id == args.case_id)
        .context("case is outside the frozen corpus")?;
    ensure!(
        case.candidate_task == args.task,
        "observation task differs from frozen candidate"
    );
    let decision = &frozen.manifest.decision;
    current(db, &args, decision, false)?;
    let (_, binding) = crate::upstream_maintenance::binding(db, &args.task)?
        .context("owning maintenance binding missing")?;
    ensure!(
        binding.budget.reference == frozen.manifest.spend_reference,
        "observation spend reference differs from original maintenance ledger"
    );
    let identity = json!({"request_id":args.request_id,"task":args.task,"case_id":args.case_id,"head":args.expected_head,"manifest_hash":frozen.manifest_hash,"backend_fingerprint":decision.fingerprint()?,"decision_config_hash":digest(decision)?,"call_scope":"maintenance-worker-budget:delivery-veto","budget_reference":binding.budget.reference,"invocation_id":binding.invocation_id});
    let prior = db.rows(
        "SELECT state,candidate_hashes,artifact_hash FROM decisions WHERE task=? AND purpose='delivery:veto' AND baseline='manual-qualification'",
        &[&args.task],
    )?;
    if let Some(prior) = prior.first() {
        let data: Value = serde_json::from_str(
            prior["candidate_hashes"]
                .as_str()
                .context("observation intent")?,
        )?;
        ensure!(
            data == identity,
            "observation request identity changed; original request must be reconciled"
        );
        return Ok(receipt(
            &args.request_id,
            if prior["state"] == "succeeded" {
                "succeeded"
            } else {
                "held"
            },
            &json!({"observation":if prior["state"] == "succeeded" {prior["artifact_hash"].clone()} else {Value::Null}}),
        ));
    }
    let snapshot = evidence::snapshot_local(db, &args.task, &case.base_commit)?;
    ensure!(
        snapshot["task"]["objective"] == case.objective
            && snapshot["attempts"]
                .as_array()
                .context("actual candidate attempts missing")?
                .iter()
                .filter_map(|attempt| attempt["started"].as_i64())
                .min()
                .is_some_and(|at| at > frozen.frozen_at),
        "candidate execution must match the frozen case and follow its freeze"
    );
    let facts = evidence::advisory_state(
        &frozen,
        case,
        &snapshot,
        &args.expected_head,
        crate::store::now(),
    )?;
    let request = crate::delivery_policy::delivery_veto_request(&decision.model, &facts)?;
    let credential = crate::config::credential(&decision.api_key_env)?;
    ensure!(
        !credential.is_empty() && !serde_json::to_string(&request)?.contains(&credential),
        "advisory request contains the active credential"
    );
    let id = format!("qualification-observe:{}", digest(&identity)?);
    let intent = json!({"identity":identity,"request":request,"request_sha256":digest(&crate::decision::wire_request(&request))?});
    db.atomic(|| {
        current_facts(db, &args, &frozen, &facts)?;
        // A task has one immutable observation slot. New IDs cannot bypass a hold.
        ensure!(db.rows("SELECT id FROM decisions WHERE task=? AND purpose='delivery:veto' AND baseline='manual-qualification'", &[&args.task])?.is_empty(), "observation already has durable intent");
        let intent_hash = db.artifact(&args.task, None, "qualification-observe-intent", &serde_json::to_vec(&intent)?, &identity, true)?;
        let admitted = decisions::enqueue_bounded(
            db,
            &decisions::QueuedDecision {
                id: id.clone(),
                task: args.task.clone(),
                step: None,
                attempt: None,
                purpose: "delivery:veto".into(),
                policy: decision.policy.clone(),
                backend: decision.backend.clone(),
                model: decision.model.clone(),
                baseline: Some("manual-qualification".into()),
            },
            decision.max_decisions_per_task,
        )?;
        ensure!(admitted, "task decision limit reached");
        decisions::start(
            db,
            &id,
            &decisions::PreparedDecision {
                state_hash: digest(&facts)?,
                context_version: facts["context_version"]
                    .as_i64()
                    .context("context version")?,
                policy_hash: digest(&frozen.manifest.delivery_policy)?,
                catalog_hash: crate::store::hash(b"delivery-v1"),
                candidate_hashes: identity.clone(),
                backend_fingerprint: decision.fingerprint()?,
                evidence_hash: digest(&facts)?,
                request_hash: intent["request_sha256"]
                    .as_str()
                    .context("request hash")?
                    .into(),
                cache_hash: String::new(),
                artifact_hash: Some(intent_hash),
            },
        )?;
        db.event(
            &args.task,
            "qualification.observation_intent",
            identity.clone(),
        )?;
        Ok(())
    })?;
    execute(db, &args, &frozen, &id, &intent, &request)
}

fn execute(
    db: &Store,
    args: &Arguments,
    frozen: &Frozen,
    id: &str,
    intent: &Value,
    request: &crate::decision::DecisionRequest,
) -> Result<Value> {
    let root = db.root.clone();
    let task = args.task.clone();
    let config = frozen.manifest.decision.clone();
    let dispatched = request.clone();
    let decision_id = id.to_owned();
    let check = args.clone();
    let pinned = frozen.clone();
    let start = std::time::Instant::now();
    let outcome = std::thread::spawn(move || -> Result<_> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let client = DecisionHttpClient::new_project(config.clone(), &root, &task)?;
            let guard = || current_facts(&Store::open(&root)?, &check, &pinned, &dispatched.state);
            Ok(client
                .observe_counted_with(&dispatched, &guard, |_| {
                    guard()?;
                    decisions::attempt_started(&Store::open(&root)?, &decision_id)
                })
                .await)
        })
    })
    .join();
    let outcome = match outcome {
        Ok(Ok(value)) => value,
        _ => Err((anyhow::anyhow!("advisory execution unavailable"), 0)),
    };
    let (response, raw, attempts) = match outcome {
        Ok(value)
            if crate::upstream_maintenance::check_online_sync(db, &args.task).is_ok()
                && current_facts(db, args, frozen, &request.state).is_ok() =>
        {
            value
        }
        _ => {
            // Intent stays held even when the process dies before this update.
            let _ = decisions::finish_state(
                db,
                id,
                "interrupted",
                "observation_requires_reconciliation",
                0,
            );
            return Ok(receipt(&args.request_id, "held", intent));
        }
    };
    let transport = json!({"endpoint":crate::decision::typesafe::endpoint(&frozen.manifest.decision.base_url)?.as_str(),"http_status":200,"request_sha256":intent["request_sha256"],"response_sha256":digest(&raw)?});
    db.atomic(|| {
        current_facts(db, args, frozen, &request.state)?;
        let transport_hash = db.artifact(&args.task,None,"qualification-decision-transport",&serde_json::to_vec(&transport)?,&intent["identity"],true)?;
        let observation = json!({"backend_fingerprint":frozen.manifest.decision.fingerprint()?,"delivery_policy_hash":digest(&frozen.manifest.delivery_policy)?,"purpose":"delivery:veto","catalog":"delivery-v1","transport_sha256":transport_hash,"observed_at":crate::store::now(),"request":request,"response":raw});
        let observation_hash = db.artifact(&args.task,None,"qualification-decision",&serde_json::to_vec(&observation)?,&intent["identity"],true)?;
        decisions::complete(db,id,&decisions::CompletedDecision {result:&serde_json::to_value(&response)?,proposed:None,abstention:false,provider_ms:start.elapsed().as_millis() as u64,attempts,usage:&serde_json::to_value(&response.usage)?})?;
        let data = json!({"identity":intent["identity"],"observation":observation_hash});
        db.conn.execute("UPDATE decisions SET artifact_hash=? WHERE id=? AND state='succeeded'", params![observation_hash,id])?;
        db.event(&args.task,"qualification.observation_finished",json!({"request_id":args.request_id,"head":args.expected_head,"observation":observation_hash}))?;
        Ok(receipt(&args.request_id,"succeeded",&data))
    })
}
