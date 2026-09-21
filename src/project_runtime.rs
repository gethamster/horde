//! Project scheduling decisions and durable dispatch provenance.
use crate::{
    projects,
    store::{Store, now},
};
use anyhow::{Context, Result};
use rusqlite::params;
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};

pub fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS project_queue(task TEXT PRIMARY KEY REFERENCES tasks(id),reason TEXT NOT NULL,updated INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS attempt_bindings(attempt TEXT PRIMARY KEY REFERENCES attempts(id),project TEXT NOT NULL REFERENCES projects(id),runtime TEXT NOT NULL,account TEXT,profile TEXT,credential_version INTEGER,isolation TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS remote_account_leases(step TEXT PRIMARY KEY REFERENCES steps(id),request_id TEXT NOT NULL,peer TEXT NOT NULL,owner_task TEXT NOT NULL,task TEXT NOT NULL,state TEXT NOT NULL);")?;
    Ok(())
}

pub fn task_root(db: &Store, task: &str) -> Result<std::path::PathBuf> {
    projects::storage_root(db, &projects::task_project(db, task)?)
}

pub fn host_limit(db: &Store) -> Result<usize> {
    let limit = crate::management::limit(db)?;
    let reserved = crate::fleet::reserved_local_cpus(db)?;
    Ok(if reserved == 0 {
        limit
    } else {
        limit.min(
            std::thread::available_parallelism()?
                .get()
                .saturating_sub(reserved),
        )
    })
}

pub fn host_active(db: &Store) -> Result<usize> {
    Ok(db.conn.query_row(
        "SELECT COUNT(*) FROM attempts WHERE state IN ('running','uncertain')",
        [],
        |r| r.get(0),
    )?)
}

pub fn ordered_tasks(db: &Store) -> Result<Vec<Value>> {
    let rows = db.rows("SELECT t.id,t.settings,p.project FROM tasks t JOIN task_projects p ON p.task=t.id WHERE t.status='running' AND NOT EXISTS(SELECT 1 FROM remote_links WHERE task=t.id) ORDER BY t.created,t.rowid", &[])?;
    let mut queues: BTreeMap<String, VecDeque<Value>> = BTreeMap::new();
    for row in rows {
        queues
            .entry(row["project"].as_str().context("project")?.to_owned())
            .or_default()
            .push_back(row);
    }
    let mut order: Vec<String> = queues.keys().cloned().collect();
    if let Some(last) = crate::management::value(db, "scheduler.last_project")? {
        let split = order.partition_point(|project| project <= &last);
        order.rotate_left(split);
    }
    let mut result = vec![];
    loop {
        let mut found = false;
        for project in &order {
            if let Some(row) = queues.get_mut(project).and_then(VecDeque::pop_front) {
                result.push(row);
                found = true;
            }
        }
        if !found {
            break;
        }
    }
    Ok(result)
}

pub fn active(db: &Store, project: &str) -> Result<i64> {
    let local: i64 = db.conn.query_row("SELECT COUNT(*) FROM attempts a JOIN steps s ON s.id=a.step JOIN task_projects p ON p.task=s.task WHERE p.project=? AND a.state IN ('running','uncertain')", [project], |r| r.get(0))?;
    let remote: i64 = db.conn.query_row("SELECT COUNT(*) FROM project_remote_reservations WHERE project=? AND state IN ('active','revoked','uncertain')", [project], |r| r.get(0))?;
    Ok(local + remote)
}

pub fn queue(db: &Store, task: &str, reason: &str) -> Result<()> {
    let changed = db.conn.execute("INSERT INTO project_queue VALUES(?,?,?) ON CONFLICT(task) DO UPDATE SET reason=excluded.reason,updated=excluded.updated WHERE reason!=excluded.reason", params![task,reason,now()])?;
    if changed > 0 {
        db.event(task, "task.queued", json!({"reason":reason}))?;
    }
    Ok(())
}

pub fn eligible(db: &Store, task: &str) -> Result<bool> {
    let project = projects::task_project(db, task)?;
    let (limit, isolation): (i64, String) = db.conn.query_row(
        "SELECT concurrency,isolation FROM projects WHERE id=?",
        [&project],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let runtime = local_runtime(db)?;
    if !projects::runtime_allowed(db, &project, &runtime)? {
        queue(db, task, "runtime is not granted to this project")?;
        return Ok(false);
    }
    let mode = crate::management::value(db, "isolation")?.unwrap_or_else(|| "native".into());
    if isolation == "vm" && !["vm", "lima"].contains(&mode.as_str()) {
        queue(db, task, "project requires an isolated VM runtime")?;
        return Ok(false);
    }
    if active(db, &project)? >= limit {
        queue(db, task, "project concurrency limit reached")?;
        return Ok(false);
    }
    Ok(true)
}

pub fn record(
    db: &Store,
    task: &str,
    attempt: &str,
    binding: Option<&crate::accounts::Binding>,
) -> Result<()> {
    let project = projects::task_project(db, task)?;
    let runtime = local_runtime(db)?;
    let isolation = crate::management::value(db, "isolation")?.unwrap_or_else(|| "native".into());
    db.conn.execute(
        "INSERT INTO attempt_bindings VALUES(?,?,?,?,?,?,?)",
        params![
            attempt,
            project,
            runtime,
            binding.and_then(|b| b.account.as_deref()),
            binding.and_then(|b| b.profile.as_deref()),
            binding.and_then(|b| b.credential_version),
            isolation
        ],
    )?;
    db.conn
        .execute("DELETE FROM project_queue WHERE task=?", [task])?;
    crate::management::set(db, "scheduler.last_project", &project)?;
    Ok(())
}

pub fn inspection(db: &Store, task: &str) -> Result<Value> {
    let project = projects::task_project(db, task)?;
    Ok(
        json!({"project":project,"queue":db.rows("SELECT reason,updated FROM project_queue WHERE task=?",&[&task])?,"bindings":db.rows("SELECT b.* FROM attempt_bindings b JOIN attempts a ON a.id=b.attempt JOIN steps s ON s.id=a.step WHERE s.task=? ORDER BY a.started",&[&task])?}),
    )
}

fn local_runtime(db: &Store) -> Result<String> {
    projects::local_runtime(db)
}

pub fn is_remote(db: &Store, task: &str) -> Result<bool> {
    Ok(!db
        .rows("SELECT task FROM remote_origins WHERE task=?", &[&task])?
        .is_empty())
}

pub fn managed_remote(
    db: &Store,
    task: &str,
    settings: &crate::config::Settings,
    role: &str,
) -> Result<bool> {
    if !is_remote(db, task)? {
        return Ok(false);
    }
    let Some(config) = settings.executor(role) else {
        return Ok(false);
    };
    if config.kind == "simulated" {
        return Ok(false);
    }
    let project = projects::task_project(db, task)?;
    if project != projects::DEFAULT_PROJECT {
        return Ok(true);
    }
    for row in db.rows("SELECT packet FROM remote_context WHERE task=?", &[&task])? {
        let packet: Value =
            serde_json::from_str(row["packet"].as_str().context("remote context")?)?;
        if packet["managed_accounts"] == true {
            return Ok(true);
        }
    }
    Ok(db.conn.query_row("SELECT EXISTS(SELECT 1 FROM accounts a JOIN account_grants g ON g.account=a.id WHERE g.project=? AND a.provider=? AND a.auth_mode=? AND a.base_url=?)",params![project,config.kind,config.auth_mode,config.base_url],|r|r.get(0))?)
}

pub fn multiple_projects(db: &Store) -> Result<bool> {
    Ok(db.conn.query_row("SELECT COUNT(DISTINCT p.project)>1 FROM task_projects p JOIN tasks t ON t.id=p.task WHERE t.status='running'", [], |r| r.get(0))?)
}

pub fn revoked(db: &Store, step: &str) -> Result<bool> {
    for binding in db.rows("SELECT b.project,b.runtime FROM attempt_bindings b JOIN attempts a ON a.id=b.attempt WHERE a.step=? AND a.state IN ('running','uncertain')", &[&step])? {
        if !projects::runtime_allowed(db, binding["project"].as_str().context("project")?, binding["runtime"].as_str().context("runtime")?)? {
            return Ok(true);
        }
    }
    Ok(db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM account_reservations WHERE step=?1 AND state='revoked') OR EXISTS(SELECT 1 FROM attempt_bindings b JOIN attempts a ON a.id=b.attempt WHERE a.step=?1 AND a.state IN ('running','uncertain') AND b.profile IS NOT NULL AND NOT EXISTS(SELECT 1 FROM accounts c JOIN account_grants g ON g.account=c.id JOIN auth_profiles p ON p.account=c.id WHERE c.id=b.account AND c.state='active' AND g.project=b.project AND p.id=b.profile))",
        [step], |r| r.get(0),
    )?)
}

/// Persist correlation before contacting the authority. A lost reply retries the same lease.
pub async fn acquire_remote(db: &Store, row: &mut Value) -> Result<bool> {
    let task = row["task"].as_str().context("task")?.to_owned();
    let step = row["id"].as_str().context("step")?.to_owned();
    let origins = db.rows(
        "SELECT owner_peer,owner_task FROM remote_origins WHERE task=?",
        &[&task],
    )?;
    let Some(origin) = origins.first() else {
        return Ok(true);
    };
    let peer = origin["owner_peer"].as_str().context("owner peer")?;
    let owner = origin["owner_task"].as_str().context("owner task")?;
    let project = projects::task_project(db, &task)?;
    let releasing:bool=db.conn.query_row("SELECT EXISTS(SELECT 1 FROM remote_account_leases WHERE step=? AND state='release_pending')",[&step],|r|r.get(0))?;
    if releasing {
        queue(
            db,
            &task,
            "previous remote reservation release pending reconciliation",
        )?;
        return Ok(false);
    }
    db.conn.execute("INSERT INTO remote_account_leases VALUES(?,?,?,?,?,'pending') ON CONFLICT(step) DO UPDATE SET request_id=excluded.request_id,state='pending' WHERE state='released'",params![step,crate::store::id(),peer,owner,task])?;
    let request: String = db.conn.query_row(
        "SELECT request_id FROM remote_account_leases WHERE step=?",
        [&step],
        |r| r.get(0),
    )?;
    let config = crate::federation::config(db)?;
    let admitted = match crate::federation::call(
        &config,
        peer,
        "project_acquire",
        json!({"project":project,"task":owner,"remote_task":task,"request_id":request}),
    )
    .await
    {
        Ok(response) => response["granted"] == true,
        Err(_) => {
            queue(
                db,
                &task,
                "controller project reservation pending reconciliation or reconnect",
            )?;
            release_remote(db, &step).await?;
            return Ok(false);
        }
    };
    if !admitted {
        queue(db, &task, "project concurrency limit reached across fleet")?;
        return Ok(false);
    }
    if row["managed_remote_account"] != true {
        db.conn.execute(
            "UPDATE remote_account_leases SET state='active' WHERE step=?",
            [step],
        )?;
        return Ok(true);
    }
    let response=match crate::federation::call(&config,peer,"account_acquire",json!({"project":project,"task":owner,"remote_task":task,"role":row["dispatch_role"],"attempt":request,"request_id":request})).await {
        Ok(response)=>response,
        Err(_)=>{queue(db,&task,"controller account reservation pending reconciliation or reconnect")?;release_remote(db,&step).await?;return Ok(false)}
    };
    if response["binding"].is_null() {
        queue(
            db,
            &task,
            "no eligible authorized account capacity at controller",
        )?;
        release_remote(db, &step).await?;
        return Ok(false);
    }
    if !response["provision"].is_null() {
        let received = serde_json::from_value(response["provision"].clone())
            .map_err(anyhow::Error::from)
            .and_then(|provision| crate::accounts::receive(db, &provision));
        if received.is_err() {
            queue(
                db,
                &task,
                "controller credential delivery rejected; reservation cleanup pending",
            )?;
            release_remote(db, &step).await?;
            return Ok(false);
        }
    }
    row["account_binding"] = response["binding"].clone();
    row["dispatch_role"] = response["binding"]["role"].clone();
    db.conn.execute(
        "UPDATE remote_account_leases SET state='active' WHERE step=?",
        [step],
    )?;
    Ok(true)
}

pub async fn release_remote(db: &Store, step: &str) -> Result<()> {
    let live: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts WHERE step=? AND state IN ('running','uncertain'))",
        [step],
        |r| r.get(0),
    )?;
    if live {
        return Ok(());
    }
    for lease in db.rows(
        "SELECT * FROM remote_account_leases WHERE step=? AND state!='released'",
        &[&step],
    )? {
        let task = lease["task"].as_str().context("task")?;
        let project = projects::task_project(db, task)?;
        let config = crate::federation::config(db)?;
        let peer = lease["peer"].as_str().context("peer")?;
        let payload = json!({"project":project,"task":lease["owner_task"],"remote_task":task,"request_id":lease["request_id"]});
        let result = match crate::federation::call(
            &config,
            peer,
            "account_release",
            payload.clone(),
        )
        .await
        {
            Ok(_) => crate::federation::call(&config, peer, "project_release", payload).await,
            Err(error) => Err(error),
        };
        if result.is_ok() {
            db.conn.execute(
                "UPDATE remote_account_leases SET state='released' WHERE step=?",
                [step],
            )?;
        } else {
            db.conn.execute(
                "UPDATE remote_account_leases SET state='release_pending' WHERE step=?",
                [step],
            )?;
        }
    }
    Ok(())
}

pub async fn reconcile_releases(db: &Store) -> Result<()> {
    // A reply can be lost before begin() creates an attempt. Canceled tasks and
    // terminal steps must still release that durable controller reservation.
    // Running/uncertain attempts retain ownership until positive reconciliation.
    db.conn.execute("UPDATE remote_account_leases SET state='release_pending' WHERE state IN ('pending','active') AND NOT EXISTS(SELECT 1 FROM attempts a WHERE a.step=remote_account_leases.step AND a.state IN ('running','uncertain')) AND (EXISTS(SELECT 1 FROM tasks t WHERE t.id=remote_account_leases.task AND t.status!='running') OR EXISTS(SELECT 1 FROM steps s WHERE s.id=remote_account_leases.step AND s.state!='pending'))", [])?;
    for row in db.rows(
        "SELECT step FROM remote_account_leases WHERE state='release_pending'",
        &[],
    )? {
        release_remote(db, row["step"].as_str().context("step")?).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interleaves_project_tasks_and_rotates_after_last_dispatch() {
        let root = tempfile::tempdir().unwrap();
        let db = Store::open(root.path()).unwrap();
        for project in ["aaa", "bbb"] {
            db.conn
                .execute(
                    "INSERT INTO projects VALUES(?,?,?,4,'native',0)",
                    params![project, project, project],
                )
                .unwrap();
            for i in 0..2 {
                let task = format!("{project}{i}");
                db.conn
                    .execute(
                        "INSERT INTO tasks VALUES(?,'work','repo','running','{}','{}',0)",
                        [&task],
                    )
                    .unwrap();
                db.conn
                    .execute(
                        "INSERT INTO task_projects VALUES(?,?,NULL)",
                        params![task, project],
                    )
                    .unwrap();
            }
        }
        crate::management::set(&db, "scheduler.last_project", "aaa").unwrap();
        let order = ordered_tasks(&db).unwrap();
        assert_eq!(
            order
                .iter()
                .map(|r| r["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["bbb0", "aaa0", "bbb1", "aaa1"]
        );
    }
}
