use crate::store::{Store, now};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde_json::{Value, json};

pub fn migrate(connection: &rusqlite::Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS decisions(
            seq INTEGER PRIMARY KEY AUTOINCREMENT,
            id TEXT NOT NULL UNIQUE,
            task TEXT NOT NULL REFERENCES tasks(id),
            step TEXT REFERENCES steps(id),
            attempt TEXT REFERENCES attempts(id),
            purpose TEXT NOT NULL,
            state TEXT NOT NULL,
            state_hash TEXT NOT NULL DEFAULT '',
            context_version INTEGER NOT NULL DEFAULT 0,
            policy TEXT NOT NULL,
            policy_hash TEXT NOT NULL DEFAULT '',
            catalog_hash TEXT NOT NULL DEFAULT '',
            candidate_hashes TEXT NOT NULL DEFAULT '{}',
            backend TEXT NOT NULL,
            model TEXT NOT NULL,
            backend_fingerprint TEXT NOT NULL DEFAULT '',
            evidence_hash TEXT NOT NULL DEFAULT '',
            request_hash TEXT NOT NULL DEFAULT '',
            cache_hash TEXT NOT NULL DEFAULT '',
            cache_source TEXT,
            artifact_hash TEXT,
            result TEXT,
            error TEXT,
            baseline TEXT,
            proposed TEXT,
            abstention INTEGER NOT NULL DEFAULT 0,
            applied INTEGER NOT NULL DEFAULT 0 CHECK(applied=0),
            queued INTEGER NOT NULL,
            started INTEGER,
            finished INTEGER,
            queue_ms INTEGER,
            provider_ms INTEGER,
            provider_attempts INTEGER NOT NULL DEFAULT 0,
            usage TEXT
        );
        CREATE INDEX IF NOT EXISTS decisions_task_seq ON decisions(task,seq);
        CREATE INDEX IF NOT EXISTS decisions_cache ON decisions(task,cache_hash,state);",
    )?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct QueuedDecision {
    pub id: String,
    pub task: String,
    pub step: Option<String>,
    pub attempt: Option<String>,
    pub purpose: String,
    pub policy: String,
    pub backend: String,
    pub model: String,
    pub baseline: Option<String>,
}

pub fn enqueue(db: &Store, row: &QueuedDecision) -> Result<()> {
    enqueue_bounded(db, row, usize::MAX).map(|_| ())
}

/// Insert a durable queue row and atomically decide whether it is within the
/// task's configured admission limit. Overflow rows remain inspectable but
/// are never eligible to call the provider.
pub fn enqueue_bounded(db: &Store, row: &QueuedDecision, maximum: usize) -> Result<bool> {
    super::ensure_label(&row.purpose, "decision purpose")?;
    let maximum = i64::try_from(maximum).unwrap_or(i64::MAX);
    db.atomic(|| {
        db.task(&row.task)?;
        let existing: i64 = db.conn.query_row(
            "SELECT COUNT(*) FROM decisions WHERE task=?",
            [&row.task],
            |result| result.get(0),
        )?;
        let admitted = existing < maximum;
        let timestamp = now();
        db.conn.execute(
            "INSERT INTO decisions(id,task,step,attempt,purpose,state,policy,backend,model,baseline,queued,error,finished)
             VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
            params![
                row.id,
                row.task,
                row.step,
                row.attempt,
                row.purpose,
                if admitted { "queued" } else { "skipped" },
                row.policy,
                row.backend,
                row.model,
                row.baseline,
                timestamp,
                (!admitted).then_some("task_decision_limit"),
                (!admitted).then_some(timestamp),
            ],
        )?;
        Ok(admitted)
    })
}

#[derive(Clone, Debug)]
pub struct PreparedDecision {
    pub state_hash: String,
    pub context_version: i64,
    pub policy_hash: String,
    pub catalog_hash: String,
    pub candidate_hashes: Value,
    pub backend_fingerprint: String,
    pub evidence_hash: String,
    pub request_hash: String,
    pub cache_hash: String,
    pub artifact_hash: Option<String>,
}

pub fn start(db: &Store, id: &str, row: &PreparedDecision) -> Result<()> {
    let started = now();
    ensure!(
        db.conn.execute(
            "UPDATE decisions SET state='running',state_hash=?,context_version=?,policy_hash=?,catalog_hash=?,candidate_hashes=?,backend_fingerprint=?,evidence_hash=?,request_hash=?,cache_hash=?,artifact_hash=?,started=?,queue_ms=(?-queued)*1000 WHERE id=? AND state='queued'",
            params![row.state_hash,row.context_version,row.policy_hash,row.catalog_hash,row.candidate_hashes.to_string(),row.backend_fingerprint,row.evidence_hash,row.request_hash,row.cache_hash,row.artifact_hash,started,started,id],
        )? == 1,
        "decision is no longer queued"
    );
    Ok(())
}

pub struct CompletedDecision<'a> {
    pub result: &'a Value,
    pub proposed: Option<&'a str>,
    pub abstention: bool,
    pub provider_ms: u64,
    pub attempts: usize,
    pub usage: &'a Value,
}

pub fn complete(db: &Store, id: &str, completed: &CompletedDecision<'_>) -> Result<()> {
    ensure!(
        db.conn.execute(
            "UPDATE decisions SET state='succeeded',result=?,proposed=?,abstention=?,finished=?,provider_ms=?,provider_attempts=?,usage=? WHERE id=? AND state='running'",
            params![completed.result.to_string(),completed.proposed,completed.abstention,now(),completed.provider_ms as i64,completed.attempts as i64,completed.usage.to_string(),id],
        )? == 1,
        "decision is no longer running"
    );
    Ok(())
}

pub fn attempt_started(db: &Store, id: &str) -> Result<()> {
    ensure!(
        db.conn.execute(
            "UPDATE decisions SET provider_attempts=provider_attempts+1 WHERE id=? AND state='running'",
            [id],
        )? == 1,
        "decision is no longer running"
    );
    Ok(())
}

pub fn complete_cached(db: &Store, id: &str, source: &Value) -> Result<()> {
    ensure!(
        db.conn.execute(
            "UPDATE decisions SET state='cached',cache_source=?,result=?,proposed=?,abstention=?,artifact_hash=?,finished=?,provider_ms=0,provider_attempts=0,usage='{}' WHERE id=? AND state='running'",
            params![source["id"].as_str(),source["result"].to_string(),source["proposed"].as_str(),source["abstention"].as_bool().unwrap_or(false),source["artifact_hash"].as_str(),now(),id],
        )? == 1,
        "decision cache record changed concurrently"
    );
    Ok(())
}

pub fn finish_state(db: &Store, id: &str, state: &str, code: &str, attempts: usize) -> Result<()> {
    ensure!(
        ["failed", "skipped", "cancelled", "interrupted"].contains(&state),
        "invalid terminal decision state"
    );
    ensure!(
        code.len() <= 128 && !code.chars().any(char::is_control),
        "invalid decision error code"
    );
    ensure!(
        db.conn.execute(
            "UPDATE decisions SET state=?,error=?,finished=?,provider_attempts=MAX(provider_attempts,?) WHERE id=? AND state IN ('queued','running')",
            params![state,code,now(),attempts as i64,id],
        )? == 1,
        "decision is already terminal"
    );
    Ok(())
}

pub fn interrupt_running(connection: &rusqlite::Connection) -> Result<usize> {
    Ok(connection.execute(
        "UPDATE decisions SET state='interrupted',error='daemon_restart',finished=? WHERE state IN ('queued','running')",
        [now()],
    )?)
}

pub fn cached(db: &Store, task: &str, cache_hash: &str) -> Result<Option<Value>> {
    db.rows("SELECT * FROM decisions WHERE task=? AND cache_hash=? AND state='succeeded' ORDER BY seq DESC LIMIT 1", &[&task, &cache_hash])?
        .into_iter().next().map(parse).transpose()
}

fn parse(mut row: Value) -> Result<Value> {
    for key in ["result", "usage", "candidate_hashes"] {
        if let Some(raw) = row[key].as_str() {
            row[key] = serde_json::from_str(raw)
                .with_context(|| format!("invalid stored decision {key}"))?;
        }
    }
    Ok(row)
}

pub fn list(db: &Store, task: &str, after: i64, limit: i64) -> Result<Vec<Value>> {
    db.task(task)?;
    ensure!(after >= 0, "decision cursor must be nonnegative");
    ensure!(
        (1..=200).contains(&limit),
        "decision limit must be between 1 and 200"
    );
    db.rows(
        "SELECT * FROM decisions WHERE task=? AND seq>? ORDER BY seq LIMIT ?",
        &[&task, &after, &limit],
    )?
    .into_iter()
    .map(parse)
    .collect()
}

pub fn metrics(db: &Store, task: &str) -> Result<Value> {
    let rows = db.rows("SELECT state,baseline,proposed,abstention,provider_attempts,queue_ms,provider_ms,usage FROM decisions WHERE task=?", &[&task])?;
    let (
        mut requests,
        mut retries,
        mut agreement,
        mut disagreement,
        mut abstentions,
        mut cache_hits,
    ) = (0_i64, 0_i64, 0_i64, 0_i64, 0_i64, 0_i64);
    let (mut input, mut output) = (0_u64, 0_u64);
    let (mut queue, mut provider) = (vec![], vec![]);
    let mut states = std::collections::BTreeMap::<String, i64>::from([
        ("queued".into(), 0),
        ("running".into(), 0),
        ("succeeded".into(), 0),
        ("cached".into(), 0),
        ("failed".into(), 0),
        ("skipped".into(), 0),
        ("cancelled".into(), 0),
        ("interrupted".into(), 0),
    ]);
    for row in &rows {
        if let Some(state) = row["state"].as_str()
            && let Some(count) = states.get_mut(state)
        {
            *count += 1;
        }
        let attempts = row["provider_attempts"].as_i64().unwrap_or(0);
        requests += attempts;
        retries += (attempts - 1).max(0);
        if let Some(ms) = row["queue_ms"].as_i64() {
            queue.push(ms);
        }
        if attempts > 0
            && let Some(ms) = row["provider_ms"].as_i64()
        {
            provider.push(ms);
        }
        let usage: Value = row["usage"]
            .as_str()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or(Value::Null);
        input += usage["input_tokens"].as_u64().unwrap_or(0);
        output += usage["output_tokens"].as_u64().unwrap_or(0);
        if row["state"] == "cached" {
            cache_hits += 1;
        }
        if row["abstention"] == 1 {
            abstentions += 1;
        } else if !row["proposed"].is_null() && row["proposed"] == row["baseline"] {
            agreement += 1;
        } else if !row["proposed"].is_null() {
            disagreement += 1;
        }
    }
    let average = |values: &[i64]| {
        if values.is_empty() {
            Value::Null
        } else {
            json!(values.iter().sum::<i64>() as f64 / values.len() as f64)
        }
    };
    let completed = states["succeeded"] + states["cached"];
    Ok(json!({
        "count":rows.len(),"cache_hits":cache_hits,"provider_requests":requests,"retries":retries,
        "queued":states["queued"],"running":states["running"],"completed":completed,
        "failed":states["failed"],"skipped":states["skipped"],
        "cancelled":states["cancelled"],"interrupted":states["interrupted"],"states":states,
        "reported_input_tokens":input,"reported_output_tokens":output,"reported_api_cost_usd":Value::Null,
        "baseline_agreements":agreement,"baseline_disagreements":disagreement,"abstentions":abstentions,
        "queue_latency_ms":{"samples":queue,"average":average(&queue)},
        "provider_latency_ms":{"samples":provider,"average":average(&provider)}
    }))
}
