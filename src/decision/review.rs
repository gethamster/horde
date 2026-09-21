//! Opt-in, advisory review of durable workflow checkpoints. Reviews never
//! authorize delivery or substitute for a generative reviewer step.
use super::{ChoiceQuestion, DecisionRequest, NoulQuestion, Question, store, typesafe::TypeSafe};
use crate::{
    config::{Decision, DecisionMode, Settings},
    store::{Store, id},
};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    time::Instant,
};
use tokio::task::JoinHandle;
mod manifest;
pub(crate) use manifest::observed_head;
use manifest::{git_text, manifest, workspace};

/// Reuse the bounded, coverage-aware diff manifest for delivery checks.
pub(crate) fn delivery_manifest(repo: &std::path::Path, base: &str, head: &str) -> Result<Value> {
    manifest(repo, base, head)
}

const MAX_PENDING: usize = 32;
const CONCURRENCY: usize = 2;

pub fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS review_checkpoints(
        id TEXT PRIMARY KEY REFERENCES decisions(id),
        event_seq INTEGER NOT NULL UNIQUE REFERENCES events(seq),
        task TEXT NOT NULL REFERENCES tasks(id),
        kind TEXT NOT NULL,
        revision INTEGER,
        context_version INTEGER,
        base TEXT,
        head TEXT,
        external_hash TEXT,
        manifest_hash TEXT,
        evidence_hash TEXT,
        coverage TEXT,
        generative_review_attempts TEXT,
        created INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS review_checkpoints_task_event ON review_checkpoints(task,event_seq);
    CREATE TABLE IF NOT EXISTS review_scan_cursor(id INTEGER PRIMARY KEY CHECK(id=1), seq INTEGER NOT NULL);
    INSERT OR IGNORE INTO review_scan_cursor(id,seq) SELECT 1,COALESCE(MAX(seq),0) FROM events;")?;
    Ok(())
}

#[derive(Clone)]
struct Job {
    id: String,
    root: PathBuf,
    task: String,
    event_seq: i64,
    kind: String,
    event: Value,
    decision: Decision,
}

/// This catalog is deliberately separate from routing-v1. One request asks
/// all questions against the same bounded work-product evidence.
fn questions() -> Vec<Question> {
    [
        ("requirements", "Does the work product appear to miss an acceptance requirement?"),
        ("correctness", "Is there a likely correctness defect in the supplied work product?"),
        ("security", "Is there a likely security or privacy defect?"),
        ("tests", "Are important tests or verification evidence missing?"),
        ("integration", "Is there an integration, compatibility, or deployment concern?"),
        ("insufficient_evidence", "Is the supplied evidence insufficient to make a reliable review judgment?"),
    ].into_iter().map(|(id, question)| Question::Noul(NoulQuestion { id: id.into(), question: question.into() }))
        .chain(std::iter::once(Question::Choice(ChoiceQuestion {
            id: "specialist".into(),
            question: "Which specialist should inspect this work product next? Choose abstain when evidence is insufficient.".into(),
            options: ["none", "correctness", "security", "testing", "operations", "abstain"].map(str::to_owned).to_vec(),
        }))).collect()
}

fn classify(event: &Value, step: Option<&Value>) -> Option<&'static str> {
    match event["kind"].as_str()? {
        "task.submitted" | "workflow.revised" | "workflow.proposed" => Some("plan"),
        "integration.conflict" | "integration.validation_failed" => Some("integration_failure"),
        "integration.succeeded" => Some("integration"),
        "task.finished" => Some("final_evidence"),
        "step.finished" => {
            let state = event["data"]["state"].as_str()?;
            let step = step?;
            let spec = Store::step(step).ok()?;
            if state == "failed" {
                Some("failed_verification")
            } else if state == "succeeded" {
                if spec.role == "planner" || spec.id.contains("plan") {
                    Some("plan")
                } else if spec.kind == "agent" {
                    Some("patch")
                } else if spec.kind == "delivery" {
                    Some("delivery_evidence")
                } else if ["command", "environment"].contains(&spec.kind.as_str()) {
                    Some("verification")
                } else {
                    None
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

#[derive(Default)]
pub struct Queue {
    pending: VecDeque<Job>,
    running: HashMap<String, (String, JoinHandle<()>)>,
}

impl Queue {
    fn hydrate(&mut self, db: &Store) -> Result<()> {
        let rows = db.rows("SELECT d.id,r.task,r.event_seq,r.kind,e.kind AS event_kind,e.data,t.settings FROM decisions d JOIN review_checkpoints r ON r.id=d.id JOIN events e ON e.seq=r.event_seq JOIN tasks t ON t.id=r.task WHERE d.state='queued' ORDER BY r.event_seq LIMIT 64", &[])?;
        for row in rows {
            if self.pending.len() >= MAX_PENDING {
                break;
            }
            let id = row["id"].as_str().context("review id")?;
            if self.pending.iter().any(|job| job.id == id) || self.running.contains_key(id) {
                continue;
            }
            let settings: Settings =
                serde_json::from_str(row["settings"].as_str().context("task settings")?)?;
            let event_data: Value =
                serde_json::from_str(row["data"].as_str().context("review event data")?)?;
            self.pending.push_back(Job {
                id: id.to_owned(),
                root: db.root.clone(),
                task: row["task"].as_str().context("review task")?.to_owned(),
                event_seq: row["event_seq"].as_i64().context("review event seq")?,
                kind: row["kind"].as_str().context("review kind")?.to_owned(),
                event: json!({"kind":row["event_kind"],"data":event_data}),
                decision: settings.decision,
            });
        }
        Ok(())
    }

    /// Advance a durable event cursor. Each admitted event has one checkpoint
    /// and one decision row, both committed before any provider call.
    pub fn scan(&mut self, db: &Store) -> Result<()> {
        let cursor: i64 =
            db.conn
                .query_row("SELECT seq FROM review_scan_cursor WHERE id=1", [], |row| {
                    row.get(0)
                })?;
        let events = db.rows(
            "SELECT seq,task,kind,data FROM events WHERE seq>? ORDER BY seq LIMIT 100",
            &[&cursor],
        )?;
        for mut event in events {
            let seq = event["seq"].as_i64().context("event sequence")?;
            let Some(task) = event["task"].as_str().map(str::to_owned) else {
                db.conn
                    .execute("UPDATE review_scan_cursor SET seq=? WHERE id=1", [seq])?;
                continue;
            };
            let parsed = event["data"]
                .as_str()
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
            let candidate = (|| -> Result<Option<(String, Decision, Value)>> {
                let settings: Settings = serde_json::from_str(
                    db.task(&task)?["settings"]
                        .as_str()
                        .context("task settings")?,
                )?;
                if settings.decision.mode != DecisionMode::Shadow
                    || !settings.decision.review_enabled
                {
                    return Ok(None);
                }
                let Some(data) = parsed else { return Ok(None) };
                event["data"] = data;
                let step = event["data"]["step"]
                    .as_str()
                    .map(|step| {
                        db.rows("SELECT * FROM steps WHERE id=? AND task=?", &[&step, &task])
                    })
                    .transpose()?
                    .and_then(|rows| rows.into_iter().next());
                Ok(classify(&event, step.as_ref())
                    .map(|kind| (kind.to_owned(), settings.decision, event)))
            })();
            if let Some((kind, decision, event)) = candidate? {
                let job = Job {
                    id: id(),
                    root: db.root.clone(),
                    task: task.clone(),
                    event_seq: seq,
                    kind,
                    event,
                    decision: decision.clone(),
                };
                db.atomic(|| {
                    let exists: i64 = db.conn.query_row("SELECT COUNT(*) FROM review_checkpoints WHERE event_seq=?", [seq], |row| row.get(0))?;
                    if exists != 0 { return Ok(()) }
                    let admitted = store::enqueue_bounded(db, &store::QueuedDecision {
                        id: job.id.clone(), task: task.clone(), step: job.event["data"]["step"].as_str().map(str::to_owned),
                        attempt: job.event["data"]["attempt"].as_str().map(str::to_owned), purpose: format!("review:{}", job.kind),
                        policy: "review-v1".into(), backend: decision.backend.clone(), model: decision.model.clone(), baseline: None,
                    }, decision.max_decisions_per_task)?;
                    db.conn.execute("INSERT INTO review_checkpoints(id,event_seq,task,kind,created) VALUES(?,?,?,?,?)", params![job.id,seq,task,job.kind,crate::store::now()])?;
                    if admitted && self.pending.len() < MAX_PENDING { self.pending.push_back(job.clone()); }
                    else if admitted { store::finish_state(db, &job.id, "skipped", "queue_full", 0)?; }
                    Ok(())
                })?;
            }
            db.conn
                .execute("UPDATE review_scan_cursor SET seq=? WHERE id=1", [seq])?;
        }
        Ok(())
    }

    pub async fn tick(&mut self, db: &Store) -> Result<()> {
        let finished = self
            .running
            .iter()
            .filter(|(_, (_, h))| h.is_finished())
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
            if let Some((_, handle)) = self.running.remove(&id) {
                handle.abort();
                let _ = handle.await;
                let _ = store::finish_state(db, &id, "cancelled", "task_cancelled", 0);
            }
        }
        self.hydrate(db)?;
        while self.running.len() < CONCURRENCY {
            let Some(job) = self.pending.pop_front() else {
                break;
            };
            if db.task(&job.task)?["status"] == "cancelled" {
                store::finish_state(db, &job.id, "cancelled", "task_cancelled", 0)?;
                continue;
            }
            let id = job.id.clone();
            let task = job.task.clone();
            self.running
                .insert(id, (task, tokio::task::spawn_local(run(job))));
        }
        Ok(())
    }

    pub async fn shutdown(&mut self, db: &Store) {
        for job in self.pending.drain(..) {
            let _ = store::finish_state(db, &job.id, "cancelled", "daemon_shutdown", 0);
        }
        for (id, (_, handle)) in self.running.drain() {
            handle.abort();
            let _ = handle.await;
            let _ = store::finish_state(db, &id, "cancelled", "daemon_shutdown", 0);
        }
    }
}

/// Only rows that never left the durable queue can safely resume after an
/// interrupted daemon. A running row stays held even if its attempt counter
/// is zero, because that counter write could have failed before a POST.
pub fn resume_never_sent(db: &Store) -> Result<usize> {
    Ok(db.conn.execute("UPDATE decisions SET state='queued',error=NULL,finished=NULL,queue_ms=NULL WHERE state='interrupted' AND started IS NULL AND provider_attempts=0 AND id IN (SELECT id FROM review_checkpoints)", [])?)
}

fn hash_json(value: &Value) -> Result<String> {
    Ok(crate::store::hash(&serde_json::to_vec(value)?))
}

pub(crate) fn current_revision(db: &Store, task: &str) -> Result<i64> {
    db.conn
        .query_row(
            "SELECT MAX(revision) FROM revisions WHERE task=?",
            [task],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn redact_evidence(
    value: &Value,
    secrets: &std::collections::BTreeMap<String, String>,
) -> Result<Value> {
    Ok(match value {
        Value::String(text) => Value::String(crate::secrets::redact_values(text, secrets)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_evidence(item, secrets))
                .collect::<Result<Vec<_>>>()?,
        ),
        Value::Object(fields) => {
            let mut result = serde_json::Map::new();
            for (key, value) in fields {
                let key = crate::secrets::redact_values(key, secrets);
                ensure!(
                    result
                        .insert(key, redact_evidence(value, secrets)?)
                        .is_none(),
                    "redacted evidence keys collide"
                );
            }
            Value::Object(result)
        }
        other => other.clone(),
    })
}

struct Prepared {
    request: Option<DecisionRequest>,
    config: Decision,
}

fn prepare(job: &Job) -> Result<Prepared> {
    let db = Store::open(&job.root)?;
    let task = db.task(&job.task)?;
    let settings: Settings =
        serde_json::from_str(task["settings"].as_str().context("task settings")?)?;
    let project = crate::projects::task_project(&db, &job.task)?;
    let current = Settings::load_project_user(&db, &project)?;
    ensure!(
        settings.decision == job.decision
            && current.decision == job.decision
            && job.decision.mode == DecisionMode::Shadow
            && job.decision.review_enabled,
        "review authorization changed"
    );
    let mut redactions = crate::secrets::values(&db, &job.task)?;
    let credential = crate::config::credential(&job.decision.api_key_env)?;
    ensure!(!credential.is_empty(), "review credential unavailable");
    redactions.insert("__review_credential".into(), credential);
    let escaped = redactions
        .values()
        .filter_map(|secret| {
            let encoded = serde_json::to_string(secret).ok()?;
            let inner = encoded.get(1..encoded.len().saturating_sub(1))?;
            (inner != secret).then(|| inner.to_owned())
        })
        .collect::<Vec<_>>();
    for (index, value) in escaped.into_iter().enumerate() {
        redactions.insert(format!("__review_escaped_{index}"), value);
    }
    let revision = current_revision(&db, &job.task)?;
    let context = crate::delegation::mandatory(&db, &job.task)?;
    let context_version = context["version"].as_i64().context("context version")?;
    let base = db
        .rows(
            "SELECT start FROM workspace_bases WHERE task=?",
            &[&job.task],
        )?
        .into_iter()
        .next()
        .and_then(|row| row["start"].as_str().map(str::to_owned));
    let path = workspace(&db, &job.task)?;
    let head = if path.exists() {
        Some(git_text(&path, &["rev-parse", "HEAD"], 128)?)
    } else {
        None
    };
    let mut coverage = json!({"complete":true,"reason":null,"catalog":"review-v1"});
    let clean = !path.exists()
        || git_text(
            &path,
            &["status", "--porcelain", "--untracked-files=all"],
            16 * 1024,
        )
        .is_ok_and(|value| value.is_empty());
    if !clean {
        coverage = json!({"complete":false,"reason":"workspace_dirty_or_unavailable","catalog":"review-v1"});
    }
    if job.kind != "plan" && (base.is_none() || head.is_none()) {
        coverage = json!({"complete":false,"reason":"workspace_unavailable","catalog":"review-v1"});
    }
    let expected_head = job.event["data"]["result"]["integration"]["integrated_head"]
        .as_str()
        .or_else(|| job.event["data"]["integrated_head"].as_str())
        .or_else(|| {
            (job.event["kind"] == "integration.conflict")
                .then(|| job.event["data"]["before"].as_str())
                .flatten()
        });
    if let Some(expected) = expected_head
        && head.is_some()
        && head.as_deref() != Some(expected)
    {
        coverage =
            json!({"complete":false,"reason":"checkpoint_head_changed","catalog":"review-v1"});
    }
    if job.event["data"].get("integrated_head").is_some()
        && job.event["data"]["integrated_head"].as_str() != head.as_deref()
    {
        coverage =
            json!({"complete":false,"reason":"checkpoint_head_changed","catalog":"review-v1"});
    }
    let expected_revision = job.event["data"]["revision"].as_i64();
    if expected_revision.is_none() {
        coverage =
            json!({"complete":false,"reason":"checkpoint_revision_unbound","catalog":"review-v1"});
    } else if expected_revision != Some(revision) {
        coverage =
            json!({"complete":false,"reason":"checkpoint_revision_changed","catalog":"review-v1"});
    }
    if expected_revision == Some(revision) {
        let expected_context_version = job.event["data"]["context_version"].as_i64();
        if expected_context_version.is_none() {
            coverage = json!({"complete":false,"reason":"checkpoint_context_unbound","catalog":"review-v1"});
        } else if expected_context_version != Some(context_version) {
            coverage = json!({"complete":false,"reason":"checkpoint_context_changed","catalog":"review-v1"});
        }
    }
    let manifest = if let (Some(base), Some(head)) = (&base, &head) {
        match manifest(&path, base, head) {
            Ok(value) => {
                if value["complete"] != true {
                    let uncovered = value["paths"].as_array().map(|paths| paths.iter().filter(|row| row["covered"] != true).map(|row| json!({"path":row["path"],"reason":row["uncovered_reason"]})).collect::<Vec<_>>()).unwrap_or_default();
                    coverage = json!({"complete":false,"reason":"diff_content_incomplete","catalog":"review-v1","uncovered_paths":uncovered});
                }
                value
            }
            Err(_) => {
                coverage = json!({"complete":false,"reason":"manifest_unavailable_or_oversize","catalog":"review-v1"});
                Value::Null
            }
        }
    } else {
        Value::Null
    };
    let mut external = db.rows(
        "SELECT name,state,data FROM external_ops WHERE task=? ORDER BY name",
        &[&job.task],
    )?;
    let external_hash = hash_json(&json!(external))?;
    for row in &mut external {
        if let Some(raw) = row["data"].as_str() {
            row["data"] = serde_json::from_str(raw).context("invalid external operation JSON")?;
        }
    }
    let attempts = db.rows("SELECT a.id,a.state,a.started,a.finished,s.name,s.spec FROM attempts a JOIN steps s ON s.id=a.step WHERE s.task=? ORDER BY a.started", &[&job.task])?
        .into_iter().filter_map(|row| {
            let spec: crate::template::Step = serde_json::from_str(row["spec"].as_str()?).ok()?;
            (spec.kind == "agent" && (spec.role == "reviewer" || spec.id.contains("review"))).then(|| json!({"attempt":row["id"],"step":spec.id,"state":row["state"],"started":row["started"],"finished":row["finished"]}))
        }).collect::<Vec<_>>();
    let work_products = if job.kind == "final_evidence" {
        let mut rows = db.rows(
            "SELECT id,name,state,result FROM steps WHERE task=? ORDER BY rowid",
            &[&job.task],
        )?;
        for row in &mut rows {
            if let Some(raw) = row["result"].as_str() {
                row["result"] = serde_json::from_str(raw).context("invalid step result JSON")?;
            }
        }
        if serde_json::to_vec(&rows)?.len() > 12 * 1024 {
            coverage =
                json!({"complete":false,"reason":"step_evidence_oversize","catalog":"review-v1"});
            json!({"count":rows.len(),"results_included":false})
        } else {
            json!({"count":rows.len(),"results_included":true,"steps":rows})
        }
    } else {
        Value::Null
    };
    let plan: Value = serde_json::from_str(task["plan"].as_str().context("task plan")?)
        .context("invalid task plan JSON")?;
    let mut evidence = redact_evidence(
        &json!({
            "catalog":"review-v1","checkpoint":{"event_seq":job.event_seq,"kind":job.kind,"event":job.event["data"]},
            "objective":task["objective"],"plan":plan,"revision":revision,"context_version":context_version,
            "mandatory_context":context["records"],"manifest":manifest,"external_operations":external,
            "generative_review_attempts":attempts,"work_products":work_products,"coverage":coverage,
        }),
        &redactions,
    )?;
    let mut request = DecisionRequest {
        model: job.decision.model.clone(),
        state: evidence.clone(),
        questions: questions(),
    };
    if coverage["complete"] == true && request.validate().is_err() {
        coverage = json!({"complete":false,"reason":"request_limit","catalog":"review-v1"});
        evidence["coverage"] = coverage.clone();
        request.state = evidence.clone();
    }
    let evidence_bytes = serde_json::to_vec(&evidence)?;
    let evidence_hash = crate::store::hash(&evidence_bytes);
    let manifest_hash = hash_json(&evidence["manifest"])?;
    db.conn.execute("UPDATE review_checkpoints SET revision=?,context_version=?,base=?,head=?,external_hash=?,manifest_hash=?,evidence_hash=?,coverage=?,generative_review_attempts=? WHERE id=?",
        params![revision,context_version,base,head,external_hash,manifest_hash,evidence_hash,coverage.to_string(),json!(attempts).to_string(),job.id])?;
    let artifact_hash = db.artifact(
        &job.task,
        job.event["data"]["step"].as_str(),
        &format!("review-evidence-{}", job.id),
        &evidence_bytes,
        &json!({"purpose":"review","catalog":"review-v1","evidence_hash":evidence_hash}),
        false,
    )?;
    let request_hash = hash_json(&super::wire_request(&request))?;
    let backend_fingerprint = hash_json(
        &json!({"backend":job.decision.backend,"base_url":job.decision.base_url,"api_key_env":job.decision.api_key_env,"model":job.decision.model}),
    )?;
    store::start(
        &db,
        &job.id,
        &store::PreparedDecision {
            state_hash: hash_json(&request.state)?,
            context_version,
            policy_hash: hash_json(&json!("review-v1"))?,
            catalog_hash: hash_json(&json!(questions()))?,
            candidate_hashes: json!({}),
            backend_fingerprint,
            evidence_hash,
            request_hash,
            cache_hash: hash_json(
                &json!({"event_seq":job.event_seq,"evidence_hash":hash_json(&request.state)?}),
            )?,
            artifact_hash: Some(artifact_hash),
        },
    )?;
    if coverage["complete"] != true || request.validate().is_err() {
        let reason = coverage["reason"].as_str().unwrap_or("request_limit");
        let result = json!({"abstained":true,"code":reason,"catalog":"review-v1"});
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
        db.event(
            &job.task,
            "review.finished",
            json!({"review":job.id,"state":"abstained","code":reason}),
        )?;
        return Ok(Prepared {
            request: None,
            config: job.decision.clone(),
        });
    }
    Ok(Prepared {
        request: Some(request),
        config: job.decision.clone(),
    })
}

async fn run(job: Job) {
    let root = job.root.clone();
    let id = job.id.clone();
    let task = job.task.clone();
    let prepared = tokio::task::spawn_blocking(move || prepare(&job)).await;
    let prepared = match prepared {
        Ok(Ok(value)) => value,
        _ => {
            if let Ok(db) = Store::open(&root) {
                let _ = store::finish_state(&db, &id, "skipped", "preparation_unavailable", 0);
            }
            return;
        }
    };
    let Some(request) = prepared.request else {
        return;
    };
    let start = Instant::now();
    let outcome = match TypeSafe::new_project(prepared.config, &root, &task) {
        Ok(backend) => {
            backend
                .decide_counted_with(&request, |_| {
                    if let Ok(db) = Store::open(&root) {
                        let _ = store::attempt_started(&db, &id);
                    }
                })
                .await
        }
        Err(error) => Err((error, 0)),
    };
    let Ok(db) = Store::open(&root) else { return };
    if db.task(&task).is_ok_and(|row| row["status"] == "cancelled") {
        let _ = store::finish_state(&db, &id, "cancelled", "task_cancelled", 0);
        return;
    }
    match outcome {
        Ok((response, attempts)) => {
            let result = serde_json::to_value(&response).unwrap_or_else(|_| json!({}));
            let usage = serde_json::to_value(&response.usage).unwrap_or_else(|_| json!({}));
            let specialist = response.answers.iter().find_map(|answer| match answer {
                super::Answer::Choice { id, answer, .. } if id == "specialist" => {
                    Some(answer.as_str())
                }
                _ => None,
            });
            if store::complete(
                &db,
                &id,
                &store::CompletedDecision {
                    result: &result,
                    proposed: specialist,
                    abstention: specialist == Some("abstain"),
                    provider_ms: start.elapsed().as_millis() as u64,
                    attempts,
                    usage: &usage,
                },
            )
            .is_ok()
            {
                let _ = db.event(
                    &task,
                    "review.finished",
                    json!({"review":id,"state":"succeeded","specialist":specialist}),
                );
            }
        }
        Err((_, attempts)) => {
            let code = if attempts == 0 {
                "authorization_changed"
            } else {
                "provider_unavailable"
            };
            if store::finish_state(&db, &id, "failed", code, attempts).is_ok() {
                let _ = db.event(
                    &task,
                    "review.finished",
                    json!({"review":id,"state":"failed","code":code}),
                );
            }
        }
    }
}

/// Review freshness is derived at read time. A provider result never mutates
/// the authoritative checkpoint or masks a changed worktree.
pub fn list(db: &Store, task: &str, after: i64, limit: i64) -> Result<Vec<Value>> {
    db.task(task)?;
    ensure!(
        after >= 0 && (1..=200).contains(&limit),
        "invalid review pagination"
    );
    let mut rows = db.rows("SELECT r.*,d.state,d.result,d.proposed,d.abstention,d.error,d.queued,d.started,d.finished,d.provider_attempts,d.usage FROM review_checkpoints r JOIN decisions d ON d.id=r.id WHERE r.task=? AND r.event_seq>? ORDER BY r.event_seq LIMIT ?", &[&task,&after,&limit])?;
    let revision: i64 = db.conn.query_row(
        "SELECT MAX(revision) FROM revisions WHERE task=?",
        [task],
        |row| row.get(0),
    )?;
    let context_version = crate::delegation::mandatory(db, task)?["version"]
        .as_i64()
        .context("context version")?;
    let path = workspace(db, task)?;
    let head = if path.exists() {
        git_text(&path, &["rev-parse", "HEAD"], 128).ok()
    } else {
        None
    };
    let clean = !path.exists()
        || git_text(
            &path,
            &["status", "--porcelain", "--untracked-files=all"],
            16 * 1024,
        )
        .is_ok_and(|value| value.is_empty());
    let external = db.rows(
        "SELECT name,state,data FROM external_ops WHERE task=? ORDER BY name",
        &[&task],
    )?;
    let external_hash = hash_json(&json!(external))?;
    for row in &mut rows {
        for key in ["result", "usage", "coverage", "generative_review_attempts"] {
            if let Some(raw) = row[key].as_str() {
                row[key] = serde_json::from_str(raw).context("invalid stored review JSON")?;
            }
        }
        let fresh = row["coverage"]["complete"] == true
            && clean
            && row["revision"].as_i64() == Some(revision)
            && row["context_version"].as_i64() == Some(context_version)
            && row["head"].as_str() == head.as_deref()
            && row["external_hash"].as_str() == Some(external_hash.as_str());
        row["fresh"] = json!(fresh);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_classifier_separates_work_products_and_failures() {
        let row = |spec: &str| json!({"spec":spec});
        let agent = row(r#"{"id":"implement","kind":"agent"}"#);
        let command = row(r#"{"id":"test","kind":"command"}"#);
        let planner = row(r#"{"id":"plan","kind":"agent","role":"planner"}"#);
        assert_eq!(
            classify(&json!({"kind":"task.submitted"}), None),
            Some("plan")
        );
        assert_eq!(
            classify(
                &json!({"kind":"step.finished","data":{"state":"succeeded"}}),
                Some(&agent)
            ),
            Some("patch")
        );
        assert_eq!(
            classify(
                &json!({"kind":"step.finished","data":{"state":"succeeded"}}),
                Some(&command)
            ),
            Some("verification")
        );
        assert_eq!(
            classify(
                &json!({"kind":"step.finished","data":{"state":"succeeded"}}),
                Some(&planner)
            ),
            Some("plan")
        );
        assert_eq!(
            classify(
                &json!({"kind":"step.finished","data":{"state":"failed"}}),
                Some(&command)
            ),
            Some("failed_verification")
        );
        assert_eq!(
            classify(&json!({"kind":"integration.conflict"}), None),
            Some("integration_failure")
        );
        assert_eq!(
            classify(&json!({"kind":"integration.succeeded"}), None),
            Some("integration")
        );
        assert_eq!(
            classify(&json!({"kind":"task.finished"}), None),
            Some("final_evidence")
        );
    }
}
