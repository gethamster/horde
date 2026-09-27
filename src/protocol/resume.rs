//! Retry failed work and reconsider only the skipped dependency closure.
use crate::store::Store;
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::collections::BTreeSet;

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
