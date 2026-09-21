//! Bounded, durable shadow routing launched after conventional dispatch.
use super::{
    Answer, ChoiceQuestion, DecisionHttpClient, DecisionRequest, NoulQuestion, Question,
    ScoreQuestion,
    store::{self, PreparedDecision, QueuedDecision},
};
use crate::{
    config::{Decision, DecisionMode, Settings},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    path::{Path, PathBuf},
    time::Instant,
};
use tokio::task::JoinHandle;

const CONCURRENCY: usize = 2;
const MAX_PENDING: usize = 128;

#[derive(Clone, Debug, Serialize)]
pub struct Candidate {
    pub id: String,
    pub runtime: String,
    pub capability: String,
    pub description: String,
    pub configuration_hash: String,
}

fn guidance(settings: &Settings, runtime: &Value, capability: &str) -> Option<String> {
    settings
        .decision
        .capability_guidance
        .iter()
        .find(|item| {
            item.capability == capability
                && (item.runtime == runtime["runtime"]
                    || (item.runtime == "local" && runtime["local"] == true))
        })
        .map(|item| item.description.clone())
}

fn eligible_runtime(runtime: &Value) -> bool {
    runtime["fresh"] == true
        && runtime["ready"] != false
        && runtime["capacity"]["draining"] != true
        && runtime["capacity"]["available"]
            .as_u64()
            .is_some_and(|slots| slots > 0)
}

/// Return only policy-pinned bindings, or the local selected role and its
/// explicit fallback chain. Candidates without current operator guidance are
/// excluded so the evaluator never invents model quality.
pub fn candidates(
    inventory: &Value,
    policy: Option<&Value>,
    settings: &Settings,
    conventional_role: &str,
) -> Result<Vec<Candidate>> {
    let runtimes = inventory["runtimes"]
        .as_array()
        .context("decision inventory unavailable")?;
    let permitted: BTreeSet<(String, String, Option<String>)> = if let Some(policy) = policy {
        policy["bindings"]
            .as_array()
            .context("decision execution bindings unavailable")?
            .iter()
            .filter_map(|binding| {
                Some((
                    binding["runtime"].as_str()?.to_owned(),
                    binding["capability"].as_str()?.to_owned(),
                    binding["configuration_hash"].as_str().map(str::to_owned),
                ))
            })
            .collect()
    } else {
        let local = runtimes
            .iter()
            .find(|runtime| runtime["local"] == true)
            .context("local runtime unavailable")?;
        let runtime = local["runtime"]
            .as_str()
            .context("local runtime identity")?
            .to_owned();
        let mut roles = vec![conventional_role.to_owned()];
        let mut role = conventional_role;
        let mut seen = BTreeSet::new();
        while let Some(next) = settings.fallbacks.get(role) {
            ensure!(seen.insert(role), "executor fallback cycle");
            roles.push(next.clone());
            role = next;
        }
        roles
            .into_iter()
            .map(|role| (runtime.clone(), role, None))
            .collect()
    };
    let mut result = vec![];
    for (runtime_id, capability, pinned_hash) in permitted {
        let Some(runtime) = runtimes
            .iter()
            .find(|runtime| runtime["runtime"] == runtime_id)
        else {
            continue;
        };
        if !eligible_runtime(runtime) {
            continue;
        }
        let Some(current) = runtime["capabilities"]
            .as_array()
            .and_then(|values| values.iter().find(|value| value["id"] == capability))
        else {
            continue;
        };
        let Some(configuration_hash) = current["configuration_hash"].as_str() else {
            continue;
        };
        if current["available"] == false
            || pinned_hash
                .as_deref()
                .is_some_and(|hash| configuration_hash != hash)
        {
            continue;
        }
        let Some(description) = guidance(settings, runtime, &capability) else {
            continue;
        };
        result.push(Candidate {
            id: format!("{runtime_id}/{capability}"),
            runtime: runtime_id,
            capability,
            description,
            configuration_hash: configuration_hash.to_owned(),
        });
    }
    result.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(result)
}

fn baseline(policy: Option<&Value>, inventory: &Value, role: &str) -> Result<String> {
    if let Some(selected) = policy
        .map(|value| &value["selected"])
        .filter(|value| !value.is_null())
    {
        return Ok(format!(
            "{}/{}",
            selected["runtime"].as_str().context("selected runtime")?,
            selected["capability"]
                .as_str()
                .context("selected capability")?
        ));
    }
    let local = inventory["runtimes"]
        .as_array()
        .and_then(|values| values.iter().find(|runtime| runtime["local"] == true))
        .context("local runtime unavailable")?;
    Ok(format!(
        "{}/{}",
        local["runtime"]
            .as_str()
            .context("local runtime identity")?,
        role
    ))
}

fn proposal(response: &super::DecisionResponse) -> Option<String> {
    response.answers.iter().find_map(|answer| match answer {
        Answer::Choice { id, answer, .. } if id == "route" => Some(answer.clone()),
        _ => None,
    })
}

fn authorized(snapshot: &Settings, current: &Settings) -> bool {
    snapshot.decision.mode == DecisionMode::Shadow && snapshot.decision == current.decision
}

#[derive(Clone, Debug)]
pub struct Job {
    pub id: String,
    pub root: PathBuf,
    pub task: String,
    pub row: Value,
    pub attempt: Option<String>,
    pub role: String,
    pub decision: Decision,
}

impl Job {
    pub fn new(
        root: PathBuf,
        task: String,
        row: Value,
        attempt: Option<String>,
        role: String,
        decision: Decision,
    ) -> Self {
        Self {
            id: crate::store::id(),
            root,
            task,
            row,
            attempt,
            role,
            decision,
        }
    }
}

#[derive(Default)]
pub struct Queue {
    pending: VecDeque<Job>,
    running: HashMap<String, (String, JoinHandle<()>)>,
}

impl Queue {
    /// Durably records a decision before it can acquire one of the two
    /// daemon-owned worker slots.
    pub fn enqueue(&mut self, db: &Store, job: Job) -> Result<()> {
        let admitted = store::enqueue_bounded(
            db,
            &QueuedDecision {
                id: job.id.clone(),
                task: job.task.clone(),
                step: job.row["id"].as_str().map(str::to_owned),
                attempt: job.attempt.clone(),
                purpose: "routing".into(),
                policy: job.decision.policy.clone(),
                backend: job.decision.backend.clone(),
                model: job.decision.model.clone(),
                baseline: None,
            },
            job.decision.max_decisions_per_task,
        )?;
        if !admitted {
            event(
                db,
                &job.task,
                &job.id,
                "skipped",
                Some("task_decision_limit"),
            );
        } else if self.pending.len() >= MAX_PENDING {
            store::finish_state(db, &job.id, "skipped", "queue_full", 0)?;
            event(db, &job.task, &job.id, "skipped", Some("queue_full"));
        } else {
            self.pending.push_back(job);
        }
        Ok(())
    }

    pub async fn tick(&mut self, db: &Store) -> Result<()> {
        let finished = self
            .running
            .iter()
            .filter(|(_, (_, handle))| handle.is_finished())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in finished {
            if let Some((_, handle)) = self.running.remove(&id) {
                let _ = handle.await;
            }
        }
        let cancelled = self
            .running
            .iter()
            .filter_map(|(id, (task, _))| {
                db.task(task)
                    .ok()
                    .filter(|row| row["status"] == "cancelled")
                    .map(|_| id.clone())
            })
            .collect::<Vec<_>>();
        for id in cancelled {
            if let Some((task, handle)) = self.running.remove(&id) {
                handle.abort();
                let _ = handle.await;
                let _ = store::finish_state(db, &id, "cancelled", "task_cancelled", 0);
                event(db, &task, &id, "cancelled", Some("task_cancelled"));
            }
        }
        let mut kept = VecDeque::new();
        while let Some(job) = self.pending.pop_front() {
            if db.task(&job.task)?["status"] == "cancelled" {
                store::finish_state(db, &job.id, "cancelled", "task_cancelled", 0)?;
                event(db, &job.task, &job.id, "cancelled", Some("task_cancelled"));
            } else {
                kept.push_back(job);
            }
        }
        self.pending = kept;
        while self.running.len() < CONCURRENCY {
            let Some(job) = self.pending.pop_front() else {
                break;
            };
            let id = job.id.clone();
            let task = job.task.clone();
            let handle = tokio::task::spawn_local(async move {
                run(job).await;
            });
            self.running.insert(id, (task, handle));
        }
        Ok(())
    }

    pub async fn shutdown(&mut self, db: &Store) {
        while let Some(job) = self.pending.pop_front() {
            let _ = store::finish_state(db, &job.id, "cancelled", "daemon_shutdown", 0);
            event(db, &job.task, &job.id, "cancelled", Some("daemon_shutdown"));
        }
        for (id, (task, handle)) in self.running.drain() {
            handle.abort();
            let _ = handle.await;
            let _ = store::finish_state(db, &id, "cancelled", "daemon_shutdown", 0);
            event(db, &task, &id, "cancelled", Some("daemon_shutdown"));
        }
    }

    pub fn is_idle(&self) -> bool {
        self.pending.is_empty() && self.running.is_empty()
    }
}

fn event(db: &Store, task: &str, id: &str, state: &str, code: Option<&str>) {
    let kind = match state {
        "skipped" => "decision.skipped",
        "cancelled" => "decision.cancelled",
        _ => "decision.finished",
    };
    let _ = db.event(task, kind, json!({"decision":id,"state":state,"code":code}));
}

enum Prepared {
    Call {
        request: DecisionRequest,
        config: Box<Decision>,
    },
    Done,
}

fn hash_json(value: &Value) -> Result<String> {
    Ok(crate::store::hash(&serde_json::to_vec(value)?))
}

/// Reconstruct the capacity snapshot immediately before this job's own local
/// attempt began. Identity, freshness, configuration hashes, and all other
/// inventory evidence remain current.
fn credit_own_attempt(db: &Store, job: &Job, inventory: Value) -> Result<Value> {
    let (Some(attempt), Some(step)) = (job.attempt.as_deref(), job.row["id"].as_str()) else {
        return Ok(inventory);
    };
    let matched: i64 = db.conn.query_row(
        "SELECT COUNT(*) FROM attempts a JOIN steps s ON s.id=a.step
         WHERE a.id=? AND a.step=? AND s.task=? AND a.state='running'",
        rusqlite::params![attempt, step, job.task],
        |row| row.get(0),
    )?;
    if matched != 1 {
        return Ok(inventory);
    }
    let Some(runtimes) = inventory["runtimes"].as_array() else {
        return Ok(inventory);
    };
    let credited = runtimes
        .iter()
        .map(|runtime| {
            if runtime["local"] != true || runtime["capacity"]["draining"] == true {
                return runtime.clone();
            }
            let Some(active) = runtime["capacity"]["active"].as_u64() else {
                return runtime.clone();
            };
            let Some(concurrency) = runtime["capacity"]["concurrency"].as_u64() else {
                return runtime.clone();
            };
            if active == 0 {
                return runtime.clone();
            }
            let mut credited = runtime.clone();
            credited["capacity"]["active"] = json!(active - 1);
            credited["capacity"]["available"] = json!(concurrency - (active - 1).min(concurrency));
            credited
        })
        .collect::<Vec<_>>();
    let mut result = inventory;
    result["runtimes"] = Value::Array(credited);
    Ok(result)
}

fn prepare(job: &Job) -> Result<Prepared> {
    let db = Store::open(&job.root)?;
    let task_row = db.task(&job.task)?;
    let snapshot: Settings =
        serde_json::from_str(task_row["settings"].as_str().context("task settings")?)?;
    ensure!(
        snapshot.decision == job.decision,
        "task decision pin changed"
    );
    let project = crate::projects::task_project(&db, &job.task)?;
    let current = Settings::load_project_user(&db, &project)?;
    ensure!(
        authorized(&snapshot, &current),
        "operator decision pin changed"
    );
    let mut redactions =
        crate::secrets::values(&db, &job.task).context("decision secret sources unavailable")?;
    let credential = crate::config::credential(&snapshot.decision.api_key_env)
        .context("decision credential unavailable")?;
    ensure!(!credential.is_empty(), "decision credential unavailable");
    redactions.insert("__decision_credential".into(), credential);

    let inventory = credit_own_attempt(
        &db,
        job,
        crate::capabilities::inventory_project(&db, &project)?,
    )?;
    let policy = crate::execution_selection::policy(&db, &job.task)?;
    let baseline = baseline(policy.as_ref(), &inventory, &job.role)?;
    let catalog = candidates(&inventory, policy.as_ref(), &snapshot, &job.role)?;
    let expected = if let Some(policy) = &policy {
        policy["bindings"].as_array().map_or(0, Vec::len)
    } else {
        let mut total = 1;
        let mut role = job.role.as_str();
        while let Some(next) = snapshot.fallbacks.get(role) {
            total += 1;
            role = next;
        }
        total
    };
    let step = Store::step(&job.row)?;
    let context = crate::delegation::mandatory(&db, &job.task)?;
    let raw_evidence = json!({
        "objective":task_row["objective"],
        "mandatory_context":context["records"],
        "step":{"id":step.id,"instructions":step.instructions,"acceptance":step.acceptance},
        "baseline":baseline,
        "candidates":catalog,
    });
    let evidence = crate::secrets::redact_json(&raw_evidence, &redactions);
    let evidence_bytes = serde_json::to_vec(&evidence)?;
    let evidence_hash = crate::store::hash(&evidence_bytes);
    let catalog_value = crate::secrets::redact_json(&serde_json::to_value(&catalog)?, &redactions);
    let catalog_hash = hash_json(&catalog_value)?;
    let candidate_hashes = crate::secrets::redact_json(
        &Value::Object(
            catalog
                .iter()
                .map(|candidate| {
                    (
                        candidate.id.clone(),
                        Value::String(candidate.configuration_hash.clone()),
                    )
                })
                .collect(),
        ),
        &redactions,
    );
    let context_version = context["version"].as_i64().context("context version")?;
    let options = catalog
        .iter()
        .map(|candidate| candidate.id.clone())
        .chain(std::iter::once("abstain".into()))
        .collect::<Vec<_>>();
    let request = DecisionRequest {
        model: snapshot.decision.model.clone(),
        state: evidence,
        questions: vec![
            Question::Choice(ChoiceQuestion { id:"route".into(), question:"Which eligible capability should run this step? Choose abstain when the evidence does not support a choice.".into(), options }),
            Question::Score(ScoreQuestion { id:"difficulty".into(), question:"How difficult is this step?".into(), legend:vec!["routine".into(),"moderate".into(),"complex".into()] }),
            Question::Noul(NoulQuestion { id:"security".into(), question:"Does this step touch a security-sensitive or consequential boundary?".into() }),
            Question::Noul(NoulQuestion { id:"insufficient_evidence".into(), question:"Is the supplied evidence insufficient to choose among the eligible capabilities?".into() }),
        ],
    };
    let wire = super::wire_request(&request);
    let request_hash = hash_json(&wire)?;
    let policy_value = crate::secrets::redact_json(&policy.unwrap_or(Value::Null), &redactions);
    let policy_hash = hash_json(&policy_value)?;
    let backend_fingerprint = snapshot.decision.fingerprint()?;
    let cache_hash = hash_json(&json!({
        "state":hash_json(&request.state)?,"context_version":context_version,
        "policy":snapshot.decision.policy,"policy_hash":policy_hash,
        "catalog_hash":catalog_hash,"candidate_hashes":candidate_hashes,
        "backend_fingerprint":backend_fingerprint,"evidence_hash":evidence_hash,
        "request_hash":request_hash,
    }))?;
    let artifact_hash = db.artifact(
        &job.task,
        job.row["id"].as_str(),
        &format!("decision-evidence-{}", job.id),
        &evidence_bytes,
        &json!({"purpose":"routing","evidence_hash":evidence_hash}),
        false,
    )?;
    store::start(
        &db,
        &job.id,
        &PreparedDecision {
            state_hash: hash_json(&request.state)?,
            context_version,
            policy_hash,
            catalog_hash,
            candidate_hashes,
            backend_fingerprint,
            evidence_hash,
            request_hash,
            cache_hash: cache_hash.clone(),
            artifact_hash: Some(artifact_hash),
        },
    )?;
    db.conn.execute(
        "UPDATE decisions SET baseline=? WHERE id=?",
        rusqlite::params![baseline, job.id],
    )?;
    if let Some(cached) = store::cached(&db, &job.task, &cache_hash)? {
        store::complete_cached(&db, &job.id, &cached)?;
        event(&db, &job.task, &job.id, "cached", None);
        return Ok(Prepared::Done);
    }
    if catalog.len() < expected || request.validate().is_err() {
        let reason = if catalog.len() < expected {
            "candidate_guidance_incomplete"
        } else {
            "request_limit"
        };
        let result = json!({"abstained":true,"code":reason});
        store::complete(
            &db,
            &job.id,
            &store::CompletedDecision {
                result: &result,
                proposed: Some("abstain"),
                abstention: true,
                provider_ms: 0,
                attempts: 0,
                usage: &json!({}),
            },
        )?;
        event(&db, &job.task, &job.id, "succeeded", Some(reason));
        return Ok(Prepared::Done);
    }
    Ok(Prepared::Call {
        request,
        config: Box::new(snapshot.decision),
    })
}

async fn run(job: Job) {
    let root = job.root.clone();
    let task = job.task.clone();
    let id = job.id.clone();
    let prepared = tokio::task::spawn_blocking(move || prepare(&job)).await;
    let prepared = match prepared {
        Ok(Ok(value)) => value,
        _ => {
            if let Ok(db) = Store::open(&root) {
                let _ = store::finish_state(&db, &id, "skipped", "preparation_unavailable", 0);
                event(&db, &task, &id, "skipped", Some("preparation_unavailable"));
            }
            return;
        }
    };
    let Prepared::Call { request, config } = prepared else {
        return;
    };
    let started = Instant::now();
    let outcome = match DecisionHttpClient::new_project(*config, &root, &task) {
        Ok(backend) => backend
            .decide_counted_with(&request, |_| {
                if let Ok(db) = Store::open(&root) {
                    let _ = store::attempt_started(&db, &id);
                }
            })
            .await
            .map_err(|(error, attempts)| super::typesafe::DecisionFailure { error, attempts }),
        Err(error) => Err(super::typesafe::DecisionFailure { error, attempts: 0 }),
    };
    let elapsed = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let Ok(db) = Store::open(&root) else {
        return;
    };
    if db.task(&task).is_ok_and(|row| row["status"] == "cancelled") {
        let _ = store::finish_state(&db, &id, "cancelled", "task_cancelled", 0);
        event(&db, &task, &id, "cancelled", Some("task_cancelled"));
        return;
    }
    match outcome {
        Ok((response, attempts)) => {
            let proposed = proposal(&response);
            let abstention = proposed.as_deref() == Some("abstain");
            let result = serde_json::to_value(&response).unwrap_or_else(|_| json!({}));
            let usage = serde_json::to_value(&response.usage).unwrap_or_else(|_| json!({}));
            if store::complete(
                &db,
                &id,
                &store::CompletedDecision {
                    result: &result,
                    proposed: proposed.as_deref(),
                    abstention,
                    provider_ms: elapsed,
                    attempts,
                    usage: &usage,
                },
            )
            .is_ok()
            {
                event(&db, &task, &id, "succeeded", None);
            }
        }
        Err(failure) => {
            let code = if failure.attempts == 0 {
                "authorization_changed"
            } else {
                "provider_unavailable"
            };
            if store::finish_state(&db, &id, "failed", code, failure.attempts).is_ok() {
                event(&db, &task, &id, "failed", Some(code));
            }
        }
    }
}

pub fn recover(db: &Store) -> Result<usize> {
    store::interrupt_running(&db.conn)
}

pub fn recover_path(root: &Path) -> Result<usize> {
    recover(&Store::open(root)?)
}
