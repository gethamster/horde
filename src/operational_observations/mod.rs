//! Controller-owned structural failure telemetry. Worker input never configures this lane.
use crate::store::{Store, now};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
mod diagnostics;
mod exporter;
mod transport;
pub use diagnostics::diagnostics;
pub use exporter::serve;
#[cfg(test)]
mod tests;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema_version: u32,
    project_id: String,
    enabled: bool,
    tenant_id: String,
    telemetry_project_id: String,
    diagnostic_thread_id: String,
    endpoint: String,
    token_file: PathBuf,
    start_cursor: i64,
    #[serde(default)]
    retry_held: bool,
}

pub fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS operational_observation_policies(project TEXT PRIMARY KEY REFERENCES projects(id),config TEXT NOT NULL,cursor INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS operational_observation_outbox(key TEXT PRIMARY KEY,project TEXT NOT NULL REFERENCES projects(id),task TEXT NOT NULL REFERENCES tasks(id),event_seq INTEGER NOT NULL,payload TEXT,state TEXT NOT NULL,attempts INTEGER NOT NULL DEFAULT 0,next_attempt INTEGER NOT NULL DEFAULT 0,error_code TEXT,UNIQUE(project,event_seq));")?;
    Ok(())
}

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}

fn validate(db: &Store, c: &Config) -> Result<()> {
    ensure!(
        c.schema_version == 1 && c.start_cursor >= 0,
        "invalid observation schema or cursor"
    );
    ensure!(
        crate::projects::resolve(db, &c.project_id)? == c.project_id,
        "canonical project required"
    );
    ensure!(
        [
            &c.tenant_id,
            &c.telemetry_project_id,
            &c.diagnostic_thread_id
        ]
        .into_iter()
        .all(|s| valid_id(s)),
        "invalid diagnostic scope"
    );
    let url = reqwest::Url::parse(&c.endpoint)?;
    let private =
        url.scheme() == "http" && url.host_str() == Some("signals") && url.port() == Some(8080);
    let secure = url.scheme() == "https"
        && url.host_str().is_some_and(|host| !host.is_empty())
        && url.port_or_known_default() == Some(443);
    ensure!(
        (private || secure)
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/v1/observations"
            && url.query().is_none()
            && url.fragment().is_none(),
        "observation endpoint must be private Signals or explicit HTTPS"
    );
    ensure!(
        c.token_file.is_absolute()
            && !c
                .token_file
                .components()
                .any(|v| matches!(v, std::path::Component::ParentDir))
            && c.token_file
                .to_str()
                .is_some_and(|s| !s.chars().any(char::is_control)),
        "invalid private credential path"
    );
    // Only the separate exporter service can read transport credentials.
    Ok(())
}

pub fn setup(db: &Store, value: &Value) -> Result<Value> {
    let c: Config =
        serde_json::from_value(value.clone()).context("invalid operational observation policy")?;
    validate(db, &c)?;
    db.atomic(|| {
        let existing:Option<String> = db.conn.query_row("SELECT config FROM operational_observation_policies WHERE project=?",[&c.project_id],|r|r.get(0)).optional()?;
        if let Some(existing)=existing {
            let old:Config=serde_json::from_str(&existing)?;
            ensure!(old.tenant_id==c.tenant_id && old.telemetry_project_id==c.telemetry_project_id && old.diagnostic_thread_id==c.diagnostic_thread_id, "diagnostic destination identity is immutable");
        }
        db.conn.execute("INSERT INTO operational_observation_policies VALUES(?,?,?) ON CONFLICT(project) DO UPDATE SET config=excluded.config",params![c.project_id,serde_json::to_string(&c)?,c.start_cursor])?;
        if c.retry_held {
            db.conn.execute("UPDATE operational_observation_outbox SET state='pending',error_code=NULL,next_attempt=0 WHERE project=? AND state='held' AND payload IS NOT NULL",[&c.project_id])?;
        }
        status(db,Some(&c.project_id))
    })
}

pub fn status(db: &Store, project: Option<&str>) -> Result<Value> {
    let values=db.rows("SELECT project,cursor,json_extract(config,'$.enabled') AS enabled FROM operational_observation_policies WHERE (? IS NULL OR project=?) ORDER BY project LIMIT 128",&[&project,&project])?;
    let counts=db.rows("SELECT state,count(*) AS count FROM operational_observation_outbox WHERE (? IS NULL OR project=?) GROUP BY state",&[&project,&project])?;
    let count = |state: &str| {
        counts
            .iter()
            .find(|r| r["state"] == state)
            .map(|r| r["count"].clone())
            .unwrap_or(json!(0))
    };
    let holds=db.rows("SELECT task,event_seq,error_code FROM operational_observation_outbox WHERE (? IS NULL OR project=?) AND state='held' ORDER BY event_seq DESC LIMIT 20",&[&project,&project])?;
    Ok(
        json!({"configured":!values.is_empty(),"projects":values,"pending":count("pending"),"delivered":count("delivered"),"held":count("held"),"holds":holds}),
    )
}

fn category(kind: &str, state: Option<&str>) -> Option<(&'static str, &'static str, &'static str)> {
    match kind {
        "step.finished" if state == Some("failed") => {
            Some(("deliver.attempt.failed.v1", "failed", "attempt_failed"))
        }
        "run.preview_held" | "run.preview_interrupted" => {
            Some(("deliver.preview.held.v1", "held", "preview_held"))
        }
        "run.preview_phase" if state == Some("held") => {
            Some(("deliver.preview.held.v1", "held", "preview_held"))
        }
        "attempt.interrupted" => Some((
            "deliver.worker.interrupted.v1",
            "interrupted",
            "worker_interrupted",
        )),
        _ => None,
    }
}

fn payload(db: &Store, c: &Config, event: &Value) -> Result<Option<Value>> {
    let Some((kind, status, reason)) = category(
        event["kind"].as_str().context("event kind")?,
        event["state"].as_str(),
    ) else {
        return Ok(None);
    };
    let reason = if kind == "deliver.attempt.failed.v1"
        && event["failure_code"] == "credential_refresh_required"
    {
        "credential_refresh_required"
    } else {
        reason
    };
    let task = event["task"].as_str().context("event task")?;
    let context = crate::run::run_context(db, task)?;
    ensure!(
        context["project"] == c.project_id
            && context["tenant_id"] == crate::projects::tenant(db, &c.project_id)?,
        "Run ownership mismatch"
    );
    ensure!(
        valid_id(task)
            && [
                &context["project"],
                &context["tenant_id"],
                &context["thread_id"],
                &context["brief_id"]
            ]
            .iter()
            .all(|v| v.is_null() || v.as_str().is_some_and(valid_id)),
        "invalid structural correlation"
    );
    let seq = event["seq"].as_i64().context("event sequence")?;
    let message = json!({"schema_version":1,"event_seq":seq,"task_id":task,"tenant_id":context["tenant_id"],"project_id":context["project"],"thread_id":context["thread_id"],"brief_id":context["brief_id"],"run_id":task,"status":status,"reason":reason});
    Ok(Some(
        json!({"project_id":c.telemetry_project_id,"thread_id":c.diagnostic_thread_id,"run_id":task,"kind":kind,"severity":"high","message":serde_json::to_string(&message)?,"source_ref":format!("deliver:run:{task}:event:{seq}")}),
    ))
}

fn capture(db: &Store) -> Result<()> {
    let policies=db.rows("SELECT config,cursor FROM operational_observation_policies WHERE json_extract(config,'$.enabled')=1 ORDER BY project LIMIT 128",&[])?;
    for policy in policies {
        let c: Config = serde_json::from_str(policy["config"].as_str().context("policy")?)?;
        let count:i64=db.conn.query_row("SELECT count(*) FROM operational_observation_outbox WHERE project=? AND state<>'delivered'",[&c.project_id],|r|r.get(0))?;
        if count >= 1000 {
            continue;
        }
        db.atomic(|| {
            let limit=100.min(1000-count);
            let events=db.rows("SELECT e.seq,e.task,e.kind,COALESCE(json_extract(e.data,'$.state'),json_extract(e.data,'$.phase')) AS state,json_extract(e.data,'$.failure_code') AS failure_code FROM events e JOIN task_projects tp ON tp.task=e.task WHERE tp.project=? AND e.seq>? ORDER BY e.seq LIMIT ?",&[&c.project_id,&policy["cursor"].as_i64(),&limit])?;
            for event in events {
                let seq=event["seq"].as_i64().context("event sequence")?;
                match payload(db,&c,&event) {
                    Ok(None)=>(),
                    result=>{
                        let key=format!("deliver:{}",crate::store::hash(format!("{}:{}:{seq}",c.project_id,event["task"].as_str().context("task")?).as_bytes()));
                        let (payload,state,error)=match result { Ok(Some(v))=>(Some(v.to_string()),"pending",None), _=>(None,"held",Some("invalid_correlation")) };
                        db.conn.execute("INSERT OR IGNORE INTO operational_observation_outbox(key,project,task,event_seq,payload,state,error_code) VALUES(?,?,?,?,?,?,?)",params![key,c.project_id,event["task"].as_str(),seq,payload,state,error])?;
                    }
                }
                db.conn.execute("UPDATE operational_observation_policies SET cursor=? WHERE project=?",params![seq,c.project_id])?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

pub async fn tick(db: &Store) -> Result<()> {
    capture(db)
}

async fn export_tick(db: &Store, trusted: &exporter::Trusted) -> Result<()> {
    let rows=db.rows("SELECT o.key,o.payload,p.config FROM operational_observation_outbox o JOIN operational_observation_policies p ON p.project=o.project WHERE o.state='pending' AND o.next_attempt<=? AND json_extract(p.config,'$.enabled')=1 ORDER BY o.event_seq LIMIT 1",&[&now()])?;
    let Some(row) = rows.first() else {
        return Ok(());
    };
    let key = row["key"].as_str().context("observation key")?;
    let c: Config = serde_json::from_str(row["config"].as_str().context("policy")?)?;
    let payload = row["payload"].as_str().context("observation payload")?;
    if trusted.authorize(&c, key, payload).is_err() {
        return finish(
            db,
            key,
            transport::Outcome::Held("transport_policy_mismatch"),
        );
    }
    // This intent commits before HTTP; restart replays the same frozen payload/key.
    db.conn.execute(
        "UPDATE operational_observation_outbox SET attempts=attempts+1,next_attempt=? WHERE key=?",
        params![now() + 5, key],
    )?;
    let outcome = transport::send(&c, key, payload).await;
    finish(db, key, outcome)
}

fn finish(db: &Store, key: &str, outcome: transport::Outcome) -> Result<()> {
    let (state, error) = match outcome {
        transport::Outcome::Delivered => ("delivered", None),
        transport::Outcome::Retry(code) => ("pending", Some(code)),
        transport::Outcome::Held(code) => ("held", Some(code)),
    };
    db.conn.execute(
        "UPDATE operational_observation_outbox SET state=?,error_code=? WHERE key=?",
        params![state, error, key],
    )?;
    Ok(())
}
