//! Retry failed work and reconsider only the skipped dependency closure.
use crate::store::Store;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Resume and its optional caller receipt commit together. Receipt replay must
/// precede live checks: later attempts and failures belong to a newer operation.
pub(super) fn resume(db: &Store, task: &str, args: &Value) -> Result<Value> {
    let request = args
        .get("request_id")
        .map(|value| {
            let id = value.as_str().context("request_id must be a string")?;
            ensure!(
                !id.trim().is_empty() && id.len() <= 256 && !id.chars().any(char::is_control),
                "request_id must contain 1..256 bytes without control characters"
            );
            Ok::<_, anyhow::Error>(id)
        })
        .transpose()?;
    let operation =
        request.map(|id| format!("resume-request:{}", crate::store::hash(id.as_bytes())));
    let request_hash = crate::store::hash(&serde_json::to_vec(args)?);
    db.atomic(|| {
        if let Some(operation) = &operation
            && let Some(row) = db.rows("SELECT data FROM external_ops WHERE task=? AND name=?", &[&task, operation])?.first()
        {
            let receipt: Value = serde_json::from_str(row["data"].as_str().context("resume receipt")?)?;
            ensure!(receipt["request_hash"] == request_hash, "resume request_id reused with different request");
            return Ok(receipt["response"].clone());
        }
        ensure!(db.rows("SELECT task FROM remote_links WHERE task=?", &[&task])?.is_empty(),
            "this task runs on a remote runtime and cannot resume locally; submit a new task with --on targeting that runtime");
        ensure!(!(db.task(task)?["status"] == "blocked" && db.steps(task)?.iter().all(|t| t["state"] == "succeeded" || t["state"] == "skipped")),
            "completed result needs revalidation; add a verification step before resuming");
        let active: i64 = db.conn.query_row("SELECT COUNT(*) FROM attempts a JOIN steps t ON t.id=a.step WHERE t.task=? AND a.state IN ('uncertain','running')", [task], |r| r.get(0))?;
        ensure!(active == 0, "reconcile interrupted workers or wait for running attempts before resuming");
        let questions: i64 = db.conn.query_row("SELECT COUNT(*) FROM questions WHERE task=? AND answer IS NULL", [task], |r| r.get(0))?;
        ensure!(questions == 0, "answer pending questions before resuming");
        let retry = steps(db, task)?;
        reset(db, task, &retry)?;
        db.conn.execute("UPDATE tasks SET status='running' WHERE id=?", [task])?;
        db.event(task, "task.resumed", json!({"steps":retry.iter().map(|step| &step["name"]).collect::<Vec<_>>(),"request_id":request}))?;
        let response = json!({"resumed":true});
        if let Some(operation) = &operation {
            db.conn.execute("INSERT INTO external_ops(task,name,state,data) VALUES(?,?,'succeeded',?)",
                rusqlite::params![task, operation, json!({"request_hash":request_hash,"response":response}).to_string()])?;
        }
        Ok(response)
    })
}

pub(super) fn steps(db: &Store, task: &str) -> Result<Vec<Value>> {
    let steps = db.steps(task)?;
    let mut retry: BTreeSet<String> = steps
        .iter()
        .filter(|step| {
            step["state"]
                .as_str()
                .is_some_and(|state| ["failed", "cancelled", "uncertain"].contains(&state))
        })
        .map(|step| Ok(step["name"].as_str().context("step name")?.to_owned()))
        .collect::<Result<_>>()?;
    loop {
        let descendants = steps
            .iter()
            .filter(|step| step["state"] == "skipped")
            .filter_map(|step| {
                let spec = Store::step(step);
                match spec {
                    Ok(spec) if spec.needs.iter().any(|name| retry.contains(name)) => {
                        Some(Ok(spec.id))
                    }
                    Ok(_) => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .collect::<Result<BTreeSet<_>>>()?;
        let next = retry.union(&descendants).cloned().collect();
        if next == retry {
            break;
        }
        retry = next;
    }
    Ok(steps
        .into_iter()
        .filter(|step| {
            step["name"]
                .as_str()
                .is_some_and(|name| retry.contains(name))
        })
        .collect())
}

pub(super) fn reset(db: &Store, task: &str, retry: &[Value]) -> Result<()> {
    for step in retry {
        let changed = db.conn.execute(
            "UPDATE steps SET state='pending' WHERE task=? AND id=? AND state=?",
            rusqlite::params![
                task,
                step["id"].as_str().context("step id")?,
                step["state"].as_str().context("step state")?
            ],
        )?;
        ensure!(
            changed == 1,
            "step changed during resume; inspect the Run before retrying"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Settings, template};
    use std::collections::BTreeMap;

    #[test]
    fn stale_retry_selection_preserves_started_work_and_rolls_back() {
        let temporary = tempfile::tempdir().unwrap();
        let db = Store::open(&temporary.path().join("data")).unwrap();
        let plan = template::compile(
            "simulated",
            &template::load_templates(temporary.path()).unwrap(),
            BTreeMap::from([("task".into(), "test".into())]),
        )
        .unwrap();
        let task = db
            .submit("test", temporary.path(), &Settings::default(), &plan)
            .unwrap();
        db.conn
            .execute("UPDATE steps SET state='failed' WHERE task=?", [&task])
            .unwrap();
        let selected = steps(&db, &task).unwrap();
        assert!(
            selected.len() > 1,
            "rollback must preserve an earlier reset"
        );
        let started = selected.last().unwrap()["id"].as_str().unwrap();
        db.conn
            .execute("UPDATE steps SET state='running' WHERE id=?", [started])
            .unwrap();
        assert!(db.atomic(|| reset(&db, &task, &selected)).is_err());
        assert_eq!(
            db.steps(&task)
                .unwrap()
                .iter()
                .find(|step| step["id"] == started)
                .unwrap()["state"],
            "running"
        );
        for step in db
            .steps(&task)
            .unwrap()
            .iter()
            .filter(|step| step["id"] != started)
        {
            assert_eq!(step["state"], "failed");
        }
    }
}
