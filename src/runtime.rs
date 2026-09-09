use crate::{
    config::Settings,
    executor::{Invocation, execute},
    store::{Store, id, now},
    template,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixListener,
    task::JoinHandle,
};

pub fn recover(db: &Store) -> Result<usize> {
    let interrupted=db.rows("SELECT a.id,a.worker,a.step,t.task FROM attempts a JOIN steps t ON t.id=a.step WHERE a.state='running'",&[])?;
    db.atomic(|| {
        for a in &interrupted {
            db.conn.execute(
                "UPDATE attempts SET state='uncertain' WHERE id=?",
                [a["id"].as_str()],
            )?;
            db.conn.execute(
                "UPDATE steps SET state='uncertain' WHERE id=? AND state!='cancelled'",
                [a["step"].as_str()],
            )?;
            db.conn.execute(
                "UPDATE tasks SET status='blocked' WHERE id=? AND status!='cancelled'",
                [a["task"].as_str()],
            )?;
            db.conn.execute(
                "UPDATE workers SET status='unresponsive' WHERE id=?",
                [a["worker"].as_str()],
            )?;
            db.event(
                a["task"].as_str().context("task")?,
                "attempt.interrupted",
                a.clone(),
            )?;
        }
        Ok(())
    })?;
    Ok(interrupted.len())
}
pub fn ready(db: &Store, oid: &str) -> Result<Vec<Value>> {
    if !db
        .rows("SELECT task FROM remote_links WHERE task=?", &[&oid])?
        .is_empty()
    {
        return Ok(vec![]);
    }
    let steps = db.steps(oid)?;
    let states: BTreeMap<_, _> = steps
        .iter()
        .map(|t| {
            (
                t["name"].as_str().unwrap_or(""),
                t["state"].as_str().unwrap_or(""),
            )
        })
        .collect();
    let mut result = vec![];
    for step in &steps {
        if step["state"] != "pending" {
            continue;
        }
        let s = Store::step(step)?;
        if !s.needs.iter().all(|n| {
            states
                .get(n.as_str())
                .is_some_and(|s| ["succeeded", "failed", "skipped", "cancelled"].contains(s))
        }) {
            continue;
        }
        if let Some(c) = &s.when
            && states.get(c.step.as_str()) != Some(&c.status.as_str())
        {
            db.conn.execute(
                "UPDATE steps SET state='skipped' WHERE id=?",
                [step["id"].as_str()],
            )?;
            continue;
        }
        let bad = s.needs.iter().any(|n| {
            states.get(n.as_str()) != Some(&"succeeded")
                && !s.when.as_ref().is_some_and(|c| c.step == *n)
        });
        if bad {
            db.conn.execute(
                "UPDATE steps SET state='skipped' WHERE id=?",
                [step["id"].as_str()],
            )?;
            continue;
        }
        result.push(step.clone());
    }
    Ok(result)
}
fn begin(db: &Store, step: &Value) -> Result<(String, String, String)> {
    let tid = step["id"].as_str().context("step")?;
    let oid = step["task"].as_str().context("task")?;
    let previous = db.rows(
        "SELECT id FROM workers WHERE step=? ORDER BY updated DESC LIMIT 1",
        &[&tid],
    )?;
    let (wid, token) = if let Some(w) = previous.first() {
        let wid = w["id"].as_str().context("worker")?.to_owned();
        let token = format!("{}{}", id(), id());
        db.conn.execute(
            "UPDATE workers SET token_hash=? WHERE id=?",
            rusqlite::params![crate::store::hash(token.as_bytes()), wid],
        )?;
        (wid, token)
    } else {
        let w = db.register(oid, Some(tid))?;
        (
            w["id"].as_str().context("worker")?.to_owned(),
            w["token"].as_str().context("token")?.to_owned(),
        )
    };
    let attempt = id();
    db.atomic(|| {
        if db.conn.execute(
            "UPDATE steps SET state='running' WHERE id=? AND state='pending'",
            [tid],
        )? != 1
        {
            bail!("step already dispatched");
        }
        db.conn.execute(
            "INSERT INTO attempts(id,step,worker,state,started) VALUES(?,?,?,'running',?)",
            rusqlite::params![attempt, tid, wid, now()],
        )?;
        db.conn.execute(
            "UPDATE workers SET status='working',updated=? WHERE id=?",
            rusqlite::params![now(), wid],
        )?;
        db.conn.execute("UPDATE notifications SET dispatched_seq=COALESCE((SELECT MAX(m.seq) FROM messages m JOIN receipts r ON r.message=m.id WHERE r.worker=?),0) WHERE worker=?",rusqlite::params![wid,wid])?;
        crate::delegation::pin(db,oid,&attempt)?;
        db.event(
            oid,
            "step.started",
            json!({"step":tid,"worker":wid,"attempt":attempt}),
        )?;
        Ok(())
    })?;
    Ok((attempt, wid, token))
}
async fn run_step(
    root: PathBuf,
    row: Value,
    attempt: String,
    wid: String,
    token: String,
) -> Result<()> {
    let db = Store::open(&root)?;
    let tid = row["id"].as_str().context("step")?;
    let oid = row["task"].as_str().context("task")?;
    let settings: Settings =
        serde_json::from_str(db.task(oid)?["settings"].as_str().context("settings")?)?;
    let step = Store::step(&row)?;
    let role = row["dispatch_role"].as_str().unwrap_or(&step.role);
    let seconds =
        (!step.step_budget_exempt).then(|| crate::budget::seconds(&settings, &step, role));
    let result = crate::budget::supervise(
        &db,
        oid,
        tid,
        &attempt,
        &wid,
        seconds,
        execute_step(&db, &row, &attempt, &wid, &token),
    )
    .await;
    db.finish(tid, &attempt, &wid, result)
}
async fn execute_step(
    db: &Store,
    row: &Value,
    attempt: &str,
    wid: &str,
    token: &str,
) -> Result<Value> {
    let tid = row["id"].as_str().context("step")?;
    let oid = row["task"].as_str().context("task")?;
    crate::federation::refresh_origin(db, oid).await?;
    db.conn.execute("UPDATE context_attempts SET version=(SELECT version FROM task_tree WHERE task=?) WHERE attempt=?",rusqlite::params![oid,attempt])?;
    let o = db.task(oid)?;
    let settings: Settings = serde_json::from_str(o["settings"].as_str().context("settings")?)?;
    let mut step = Store::step(row)?;
    let settings = if step.kind == "agent" {
        crate::execution_selection::apply(db, oid, &settings)?
    } else {
        settings
    };
    let failures: i64 = db.conn.query_row(
        "SELECT COUNT(*) FROM attempts WHERE step=? AND state='failed'",
        [tid],
        |r| r.get(0),
    )?;
    let original_role = step.role.clone();
    for _ in 0..failures {
        if let Some(role) = settings.fallbacks.get(&step.role) {
            step.role = role.clone();
        } else {
            break;
        }
    }
    if step.kind == "agent" {
        step.role = if let Some(role) = row["dispatch_role"].as_str() {
            role.to_owned()
        } else {
            crate::capacity::select(db, &settings, &step.role)?
                .context("account capacity unavailable; work held")?
        };
        if let Some(config) = settings.executor(&step.role) {
            db.conn.execute(
                "INSERT OR REPLACE INTO attempt_accounts VALUES(?,?,?)",
                rusqlite::params![attempt, crate::capacity::account(&config), step.role],
            )?;
        }
    }
    if step.role != original_role {
        db.event(
            oid,
            "executor.escalated",
            json!({"step":tid,"from":original_role,"to":step.role,"failures":failures}),
        )?;
    }
    let steps = db.steps(oid)?;
    let mut outputs = BTreeMap::new();
    let mut dependencies = vec![];
    for t in steps
        .iter()
        .filter(|t| step.needs.iter().any(|n| t["name"] == *n))
    {
        if let Some(raw) = t["result"].as_str() {
            let v: Value = serde_json::from_str(raw)?;
            if let Some(map) = v.as_object() {
                for (k, v) in map {
                    outputs.insert(
                        format!("{}.{}", t["name"].as_str().context("name")?, k),
                        v.clone(),
                    );
                }
            }
            dependencies.push(json!({"step":t["id"],"name":t["name"],"result":v}));
        }
    }
    step.instructions = template::resolve_refs(&step.instructions, &outputs)?;
    for arg in &mut step.command {
        *arg = template::resolve_refs(arg, &outputs)?;
    }
    let simulated = step.kind == "simulated"
        || settings
            .executor(&step.role)
            .is_some_and(|e| e.kind == "simulated");
    let workspace = if simulated {
        PathBuf::from(o["repo"].as_str().context("repo")?)
    } else if ["command", "delivery", "environment"].contains(&step.kind.as_str()) {
        let root = db.root.clone();
        let task = oid.to_owned();
        crate::budget::blocking(move || crate::git::task_workspace(&Store::open(&root)?, &task))
            .await?
    } else {
        let w = db.worker(wid)?;
        if let Some(path) = w["workspace"].as_str() {
            PathBuf::from(path)
        } else {
            let root = db.root.clone();
            let task = oid.to_owned();
            let worker = wid.to_owned();
            crate::budget::blocking(move || {
                crate::git::allocate(&Store::open(&root)?, &task, &worker)
            })
            .await?
        }
    };
    if !simulated && step.kind == "agent" {
        db.claim(oid, wid, &step.scope)?;
    }
    let context = json!({
        "objective":o["objective"],
        "inherited_contract":crate::delegation::mandatory(db,oid)?,
        "answers":answered_questions(db,oid)?,
        "dependencies":dependencies,
        "children":crate::delegation::dispatch(db,oid,"list_children",&json!({}))?,
        "pending_questions":crate::delegation::dispatch(db,oid,"pending_questions",&json!({}))?,
        "remote_caller_context":db.rows("SELECT packet FROM remote_context WHERE task=?",&[&oid])?,
        "knowledge":db.rows("SELECT * FROM knowledge WHERE task=? ORDER BY verified DESC LIMIT 100",&[&oid])?,
        "messages":db.messages(wid,0,100)?,
        "previous_attempts":db.rows("SELECT result FROM attempts WHERE step=? AND state!='running' ORDER BY started DESC LIMIT 3",&[&tid])?
    });
    let invocation = Invocation {
        db,
        task: oid,
        step: tid,
        attempt,
        worker: wid,
        token,
        workspace: &workspace,
        spec: &step,
        settings: &settings,
        context,
    };
    let mut result = execute(&invocation).await?;
    let pending:i64=db.conn.query_row("SELECT COUNT(*) FROM questions q JOIN question_context c ON c.question=q.id WHERE c.worker=? AND q.answer IS NULL",[wid],|r|r.get(0))?;
    if pending > 0 {
        bail!("awaiting required information; answer the pending question to resume");
    }
    crate::federation::refresh_origin(db, oid).await?;
    crate::delegation::check_pin(db, oid, attempt)?;
    template::validate_result(&step, &result)?;
    if !simulated && step.kind == "agent" {
        let root = db.root.clone();
        let task = oid.to_owned();
        let worker = wid.to_owned();
        result["integration"] = crate::budget::blocking(move || {
            let db = Store::open(&root)?;
            crate::git::validate_scope(&db, &worker)?;
            crate::git::integrate(&db, &task, &worker, &[])
        })
        .await?;
        let _ = db.send(
            oid,
            wid,
            &id(),
            "task",
            &format!(
                "Step {} integrated; inspect its result before dependent edits.",
                step.id
            ),
            &json!({"step":tid,"integration":result["integration"]}),
            false,
        );
    }
    for name in &step.artifacts {
        let relative = crate::store::scope(name)?;
        let path = workspace.join(relative).canonicalize()?;
        if !path.starts_with(workspace.canonicalize()?) {
            bail!("artifact escapes workspace");
        }
        let bytes = std::fs::read(path)?;
        db.artifact(
            oid,
            Some(tid),
            name,
            &bytes,
            &json!({"attempt":attempt}),
            false,
        )?;
    }
    Ok(result)
}
fn answered_questions(db: &Store, oid: &str) -> Result<Vec<Value>> {
    db.rows("SELECT question,answer FROM questions q WHERE task=? AND answer IS NOT NULL AND NOT EXISTS(SELECT 1 FROM context_records c WHERE c.id='answer:'||q.id AND c.mandatory=1)", &[&oid])
}
fn settle(db: &Store, oid: &str) -> Result<()> {
    let steps = db.steps(oid)?;
    let mut retrying = false;
    for t in steps.iter().filter(|t| t["state"] == "failed") {
        let s = Store::step(t)?;
        let count: i64 = db.conn.query_row(
            "SELECT COUNT(*) FROM attempts WHERE step=?",
            [t["id"].as_str()],
            |r| r.get(0),
        )?;
        if count < i64::from(s.attempts) {
            db.conn.execute(
                "UPDATE steps SET state='pending' WHERE id=?",
                [t["id"].as_str()],
            )?;
            db.event(oid, "step.retry", json!({"step":t["id"],"attempts":count}))?;
            retrying = true;
        }
    }
    if retrying {
        return Ok(());
    }
    if !crate::delegation::child_completion(db, oid)? {
        return Ok(());
    }
    if steps.iter().all(|t| {
        ["succeeded", "failed", "skipped", "cancelled"].contains(&t["state"].as_str().unwrap_or(""))
    }) {
        // A failed step is recovered only by a successful explicit failure branch.
        let failed = steps.iter().any(|t| {
            t["state"] == "failed"
                && !steps.iter().any(|r| {
                    r["state"] == "succeeded"
                        && Store::step(r).ok().and_then(|s| s.when).is_some_and(|c| {
                            c.step == t["name"].as_str().unwrap_or("") && c.status == "failed"
                        })
                })
        });
        let status = if failed { "failed" } else { "succeeded" };
        if !failed {
            let o = db.task(oid)?;
            let plan: template::Plan = serde_json::from_str(o["plan"].as_str().context("plan")?)?;
            let mut values = serde_json::Map::new();
            for (name, reference) in plan.outputs {
                if let Some((step, field)) = reference.rsplit_once('.')
                    && let Some(t) = steps
                        .iter()
                        .find(|t| t["name"] == step && t["state"] == "succeeded")
                {
                    let result: Value =
                        serde_json::from_str(t["result"].as_str().unwrap_or("null"))?;
                    values.insert(name, result[field].clone());
                }
            }
            db.conn.execute(
                "INSERT OR REPLACE INTO workflow_outputs VALUES(?,?)",
                rusqlite::params![oid, Value::Object(values).to_string()],
            )?;
        }
        if db.conn.execute(
            "UPDATE tasks SET status=? WHERE id=? AND status='running'",
            rusqlite::params![status, oid],
        )? > 0
        {
            db.event(oid, "task.finished", json!({"status":status}))?;
        }
    }
    Ok(())
}
// A receipt and the actionable mailbox message commit together. A working caller
// observes the message at its next boundary; an idle caller resumes durably.
fn notify_completed_children(db: &Store) -> Result<()> {
    let mut children = db.rows("SELECT ot.*,o.status,COALESCE((SELECT MAX(revision) FROM revisions WHERE task=ot.task),0) AS revision FROM task_tree ot JOIN tasks o ON o.id=ot.task WHERE ot.parent IS NOT NULL AND ot.caller_worker IS NOT NULL", &[])?;
    for row in db.rows("SELECT task,packet FROM remote_context", &[])? {
        let packet: Value =
            serde_json::from_str(row["packet"].as_str().context("remote context")?)?;
        for child in packet["children"].as_array().into_iter().flatten() {
            let child_id = child["task"].as_str().context("remote child")?;
            let name = format!("delegation.caller:{child_id}");
            let mapping = db.rows(
                "SELECT data FROM external_ops WHERE task=? AND name=?",
                &[&row["task"].as_str(), &name],
            )?;
            if let Some(mapping) = mapping.first() {
                let data: Value =
                    serde_json::from_str(mapping["data"].as_str().context("caller mapping")?)?;
                let mut child = child.clone();
                child["parent"] = row["task"].clone();
                child["caller_worker"] = data["worker"].clone();
                children.push(child);
            }
        }
    }
    for child in children {
        if !["succeeded", "failed", "cancelled"].contains(&child["status"].as_str().unwrap_or("")) {
            continue;
        }
        let Some(worker) = child["caller_worker"].as_str() else {
            continue;
        };
        let parent = child["parent"].as_str().context("parent")?;
        let name = format!(
            "delegation.terminal:{}:{}:{}:{}",
            child["task"].as_str().context("child")?,
            child["version"],
            child["revision"],
            child["status"]
        );
        db.atomic(|| {
            if db.conn.execute("INSERT OR IGNORE INTO external_ops VALUES(?,?,'notified',?)", rusqlite::params![parent,name,child.to_string()])? == 0 { return Ok(()); }
            db.send(parent,worker,&id(),worker,"Delegated child reached a terminal state. Inspect its result; integrate and validate successful changes, or handle its failure.",&json!({"child":child}),true)?;
            Ok(())
        })?;
    }
    Ok(())
}
fn wake_notified(db: &Store) -> Result<()> {
    let workers=db.rows("SELECT w.* FROM workers w JOIN tasks o ON o.id=w.task WHERE w.status='notified' AND o.status IN ('running','succeeded') AND EXISTS(SELECT 1 FROM attempts a WHERE a.worker=w.id)",&[])?;
    for w in workers {
        let oid = w["task"].as_str().context("task")?;
        let old = db.rows(
            "SELECT * FROM steps WHERE id=? AND state='succeeded'",
            &[&w["step"].as_str()],
        )?;
        let Some(old) = old.first() else {
            continue;
        };
        let mut step = Store::step(old)?;
        // Application lifecycles are not conversational workers: a child notification
        // must not replay a completed test or its external effects.
        if step.kind == "environment" {
            continue;
        }
        step.needs = vec![step.id.clone()];
        step.id = format!("{}.followup-{}", step.id, &id()[..8]);
        step.instructions="Read pending_questions and child results as well as unread coordination messages, acknowledge messages, and act on actionable requests within the assigned scope. Verify and commit any changes. Escalate unresolved disagreements in the task channel.".into();
        step.when = None;
        step.output_types.clear();
        step.artifacts.clear();
        crate::protocol::dispatch(db, "add_steps", json!({"task":oid,"steps":[step]}), None)?;
        db.conn.execute("UPDATE workers SET step=(SELECT id FROM steps WHERE task=? AND name=?),status='idle' WHERE id=?",rusqlite::params![oid,step.id,w["id"].as_str()])?;
    }
    Ok(())
}
async fn handle(stream: tokio::net::UnixStream, root: PathBuf) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    // Each connection carries one bounded request; clients can reconnect without affecting execution.
    use tokio::io::AsyncReadExt;
    let n = tokio::time::timeout(
        Duration::from_secs(15),
        (&mut reader).take(1024 * 1024 + 1).read_line(&mut line),
    )
    .await??;
    if n > 1024 * 1024 {
        bail!("request exceeds 1 MiB");
    }
    let result = match serde_json::from_str::<Value>(&line) {
        Ok(v) => {
            let root = root.clone();
            crate::budget::blocking(move || {
                let db = Store::open(&root)?;
                crate::protocol::dispatch(
                    &db,
                    v["method"].as_str().unwrap_or(""),
                    v.get("args").cloned().unwrap_or(json!({})),
                    v["token"].as_str(),
                )
            })
            .await
        }
        Err(e) => Err(e.into()),
    };
    let response = match result {
        Ok(v) => json!({"result":v}),
        Err(e) => json!({"error":format!("{e:#}")}),
    };
    write.write_all(format!("{response}\n").as_bytes()).await?;
    Ok(())
}
type ActiveAttempt = (String, String, String, JoinHandle<Result<()>>);

struct Scheduler {
    running: HashMap<String, ActiveAttempt>,
    limit: usize,
}
impl Scheduler {
    async fn tick(&mut self, db: &Store, remote_ready: bool) -> Result<()> {
        let finished: Vec<_> = self
            .running
            .iter()
            .filter(|(_, (_, _, _, h))| h.is_finished())
            .map(|(k, _)| k.clone())
            .collect();
        for tid in finished {
            if let Some((_, attempt, wid, h)) = self.running.remove(&tid) {
                match h.await {
                    Ok(Ok(())) => {}
                    other => {
                        db.finish(
                            &tid,
                            &attempt,
                            &wid,
                            Err(anyhow::anyhow!("worker execution failed: {other:?}")),
                        )?;
                    }
                }
            }
        }
        let cancelled: Vec<_> = self
            .running
            .iter()
            .filter_map(|(tid, (oid, _, _, _))| {
                db.task(oid)
                    .ok()
                    .filter(|o| o["status"] == "cancelled")
                    .map(|_| tid.clone())
            })
            .collect();
        for tid in cancelled {
            if let Some((_, attempt, wid, h)) = self.running.remove(&tid) {
                h.abort();
                let _ = h.await;
                db.finish(
                    &tid,
                    &attempt,
                    &wid,
                    Err(anyhow::anyhow!("cancelled; worker process group stopped")),
                )?;
            }
        }
        self.limit = crate::management::limit(db)?;
        if crate::management::draining(db)? {
            return Ok(());
        }
        notify_completed_children(db)?;
        wake_notified(db)?;
        for o in db.rows(
            "SELECT id,settings FROM tasks WHERE status='running' AND NOT EXISTS(SELECT 1 FROM remote_links WHERE task=tasks.id) ORDER BY created",
            &[],
        )? {
            let oid = o["id"].as_str().context("task")?;
            if !remote_ready
                && !db
                    .rows("SELECT task FROM remote_origins WHERE task=?", &[&oid])?
                    .is_empty()
            {
                continue;
            }
            let settings: Settings =
                serde_json::from_str(o["settings"].as_str().context("settings")?)?;
            let settings = if db.steps(oid)?.iter().any(|step| Store::step(step).is_ok_and(|step| step.kind == "agent")) {
                match crate::execution_selection::apply(db, oid, &settings) {
                    Ok(settings) => settings,
                    Err(error) => {
                        db.conn.execute("UPDATE tasks SET status='blocked' WHERE id=?", [oid])?;
                        db.event(oid, "execution.selection_blocked", json!({"reason":error.to_string()}))?;
                        continue;
                    }
                }
            } else { settings };
            settle(db, oid)?;
            let mut active = self
                .running
                .values()
                .filter(|(o, _, _, _)| o == oid)
                .count();
            for mut row in ready(db, oid)? {
                if !crate::delegation::capacity(db, oid)? {
                    break;
                }
                if active >= settings.concurrency || self.running.len() >= self.limit {
                    break;
                }
                let step = Store::step(&row)?;
                if step.kind == "agent" {
                    let Some(role) = crate::capacity::select_for_step(
                        db,
                        &settings,
                        &step.role,
                        row["id"].as_str().context("step")?,
                    )?
                    else {
                        continue;
                    };
                    row["dispatch_role"] = json!(role);
                }
                if step.environment.is_some() && !crate::environment::available(db, oid)? {
                    continue;
                }
                let claims=db.rows("SELECT c.path,w.step,w.status,t.state AS step_state FROM claims c JOIN workers w ON c.worker=w.id LEFT JOIN steps t ON t.id=w.step WHERE c.task=?",&[&oid])?;
                let conflict = claims.iter().find(|c| {
                    c["step"] != row["id"]
                        && step.scope.iter().any(|p| {
                            crate::store::scope(p).is_ok_and(|p| {
                                crate::store::overlaps(&p, c["path"].as_str().unwrap_or("."))
                            })
                        })
                });
                if let Some(owner) = conflict {
                    if active == 0
                        && owner["step_state"] != "pending"
                        && ["failed", "unresponsive", "stopped"]
                            .contains(&owner["status"].as_str().unwrap_or(""))
                    {
                        db.conn
                            .execute("UPDATE tasks SET status='blocked' WHERE id=?", [oid])?;
                        db.event(oid,"task.blocked",json!({"reason":"write claim requires reconciliation","step":row["id"],"claim":owner}))?;
                    }
                    continue;
                }
                let exclusive_active = db
                    .rows(
                        "SELECT spec FROM steps WHERE task=? AND state='running'",
                        &[&oid],
                    )?
                    .iter()
                    .any(|t| {
                        Store::step(t).is_ok_and(|s| {
                            ["command", "delivery", "environment"].contains(&s.kind.as_str())
                        })
                    });
                if exclusive_active {
                    continue;
                }
                let exclusive =
                    ["command", "delivery", "environment"].contains(&step.kind.as_str());
                if exclusive && active > 0 {
                    continue;
                }
                let tid = row["id"].as_str().context("step")?.to_owned();
                let (attempt, wid, token) = begin(db, &row)?;
                let handle = tokio::task::spawn_local(run_step(
                    db.root.clone(),
                    row,
                    attempt.clone(),
                    wid.clone(),
                    token,
                ));
                self.running
                    .insert(tid, (oid.to_owned(), attempt, wid, handle));
                active += 1;
                if exclusive {
                    break;
                }
            }
        }
        Ok(())
    }
}

pub async fn daemon(root: &Path) -> Result<()> {
    use fs2::FileExt;
    anyhow::ensure!(
        crate::branding::var_os("HORDE_BOOTSTRAP_JSON").is_none()
            || (crate::branding::var_os("HORDE_ENROLLMENT_FILE").is_none()
                && crate::branding::var_os("HORDE_ENROLLMENT_JSON").is_none()
                && !root.join("fleet-worker.json").exists()
                && !root.join("fleet-worker-pending.json").exists()
                && !root.join("fleet-worker.key").exists()),
        "legacy bootstrap and fleet enrollment cannot be combined"
    );
    let db = Store::open(root)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("daemon.lock"))?;
    lock.try_lock_exclusive()
        .context("another daemon is already running")?;
    crate::enrollment::bootstrap(root)?;
    crate::fleet_enrollment::worker::bootstrap(root).await?;
    let shutdown = root.join("shutdown.request");
    if shutdown.exists() {
        std::fs::remove_file(&shutdown)?;
    }
    let socket = root.join("daemon.sock");
    if socket.exists() {
        std::fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let recovered = recover(&db)?;
    crate::environment::reconcile(&db).await?;
    crate::federation::clear_remote_secrets(&db)?;
    eprintln!(
        "horde daemon listening on {} ({recovered} interrupted attempts held for reconciliation)",
        socket.display()
    );
    let mut scheduler = Scheduler {
        running: HashMap::new(),
        limit: crate::management::limit(&db)?,
    };
    let remote_ready = std::rc::Rc::new(std::cell::Cell::new(false));
    let maintenance_ready = remote_ready.clone();
    let maintenance_root = root.to_owned();
    let maintenance = tokio::task::spawn_local(async move {
        loop {
            maintenance_ready.set(false);
            let result = async {
                let db = Store::open(&maintenance_root)?;
                crate::federation::tick(&db).await?;
                Ok::<(), anyhow::Error>(())
            }
            .await;
            maintenance_ready.set(result.is_ok());
            if let Err(error) = result {
                eprintln!("Federation maintenance: {error:#}");
            }
            let cleanup = async {
                let db = Store::open(&maintenance_root)?;
                crate::environment::cleanup_pending(&db).await
            }
            .await;
            if let Err(error) = cleanup {
                eprintln!("Environment maintenance: {error:#}");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    db.conn.execute("UPDATE runtime_operations SET state='succeeded' WHERE runtime='local' AND action='runtime_restart' AND state='restarting'",[])?;
    let fleet_root = root.to_owned();
    crate::fleet::recover_operations(&db)?;
    let fleet = tokio::task::spawn_local(async move {
        let mut last = std::time::Instant::now() - Duration::from_secs(60);
        loop {
            let result = async {
                let db = Store::open(&fleet_root)?;
                crate::fleet::tick(&db).await?;
                crate::management::local_commands(&db)?;
                if last.elapsed() >= Duration::from_secs(60) {
                    crate::fleet::collect_capacity(&db).await?;
                    last = std::time::Instant::now();
                }
                Ok::<_, anyhow::Error>(())
            }
            .await;
            if let Err(e) = result {
                eprintln!("Runtime management: {e:#}");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
    let managed_config = root.join("managed-network.toml");
    let network_settings = crate::network::NetworkConfig::load(if managed_config.exists() {
        Some(&managed_config)
    } else {
        None
    })?;
    let admission = tokio::task::spawn_local(crate::fleet_enrollment::service::supervise(
        root.to_owned(),
        network_settings.clone(),
    ));
    let renewal_root = root.to_owned();
    let renewal = tokio::task::spawn_local(async move {
        loop {
            if let Err(error) = crate::fleet_enrollment::worker::renew_if_due(&renewal_root).await {
                eprintln!("Worker certificate renewal: {error:#}");
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
    let reverse = if network_settings.controller_peer.is_some() {
        let root = root.to_owned();
        let config = network_settings.clone();
        crate::federation::configure(&root, &config)?;
        Some(tokio::task::spawn_local(async move {
            loop {
                let current = if root.join("fleet-worker.json").exists() {
                    crate::network::NetworkConfig::load(Some(&root.join("managed-network.toml")))
                } else {
                    Ok(config.clone())
                };
                let result = match current {
                    Ok(current) => crate::control::connect(root.clone(), current).await,
                    Err(error) => Err(error),
                };
                if let Err(e) = result {
                    eprintln!("Control connection: {e:#}");
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }))
    } else {
        None
    };
    let network = if network_settings.provider != crate::network::Provider::Disabled
        && network_settings.controller_peer.is_none()
    {
        let listener = crate::network::bind_listener(&network_settings).await?;
        crate::federation::configure(root, &network_settings)?;
        let network_root = root.to_owned();
        Some(tokio::task::spawn_local(async move {
            crate::network::serve_runtime(
                &network_settings,
                listener,
                std::future::pending::<()>(),
                network_root,
            )
            .await
        }))
    } else {
        None
    };
    let mut interval = tokio::time::interval(Duration::from_millis(200));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    crate::update::complete_handoff(&db)?;
    // Older updaters replace only the executable. Fetch its signed default pack
    // independently while keeping management and already-pinned work available.
    let skills_root = root.to_owned();
    let skills_bootstrap = tokio::task::spawn_local(async move {
        loop {
            match crate::update::ensure_default_skills(&skills_root).await {
                Ok(()) => break,
                Err(error) => eprintln!("Default skill installation: {error:#}"),
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
    loop {
        tokio::select! {
            connection=listener.accept()=>{
                let(stream,_)=connection?;let root=root.to_owned();
                tokio::task::spawn_local(async move{if let Err(e)=handle(stream,root).await{eprintln!("RPC connection: {e:#}");}});
            },
            _=interval.tick()=>{if shutdown.exists(){std::fs::remove_file(&shutdown)?;break;}scheduler.tick(&db,remote_ready.get()).await?;},
            _=tokio::signal::ctrl_c()=>{break;},
            _=terminate.recv()=>{break;},
        }
    }
    skills_bootstrap.abort();
    let _ = skills_bootstrap.await;
    if let Some(reverse) = reverse {
        reverse.abort();
        let _ = reverse.await;
    }
    admission.abort();
    let _ = admission.await;
    renewal.abort();
    let _ = renewal.await;
    fleet.abort();
    let _ = fleet.await;
    if let Some(network) = network {
        network.abort();
        let _ = network.await;
    }
    maintenance.abort();
    let _ = maintenance.await;
    for (tid, (_, attempt, wid, h)) in scheduler.running {
        h.abort();
        let _ = h.await;
        db.finish(
            &tid,
            &attempt,
            &wid,
            Err(anyhow::anyhow!("daemon stopped; process group stopped")),
        )?;
    }
    std::fs::remove_file(socket)?;
    if crate::management::value(&db, "restart_requested")?.as_deref() == Some("true") {
        crate::management::set(&db, "restart_requested", "false")?;
        bail!("restart requested; supervisor will launch the selected version");
    }
    Ok(())
}

#[cfg(test)]
mod coordination_regressions {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Store, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let db = Store::open(dir.path()).unwrap();
        let plan = template::compile(
            "simulated",
            &template::load_templates(dir.path()).unwrap(),
            BTreeMap::from([("task".into(), "parent".into())]),
        )
        .unwrap();
        let parent = db
            .submit("parent", dir.path(), &Settings::default(), &plan)
            .unwrap();
        let child = db
            .submit("child", dir.path(), &Settings::default(), &plan)
            .unwrap();
        (dir, db, parent, child)
    }
    #[test]
    fn terminal_child_wakes_caller_once_across_reopen() {
        let (dir, db, parent, child) = fixture();
        let step = db.steps(&parent).unwrap()[0].clone();
        let (attempt, worker, _) = begin(&db, &step).unwrap();
        db.finish(
            step["id"].as_str().unwrap(),
            &attempt,
            &worker,
            Ok(json!({"accepted":true})),
        )
        .unwrap();
        db.conn
            .execute(
                "UPDATE task_tree SET parent=?,caller_worker=? WHERE task=?",
                rusqlite::params![parent, worker, child],
            )
            .unwrap();
        db.conn
            .execute("UPDATE tasks SET status='succeeded' WHERE id=?", [&child])
            .unwrap();
        notify_completed_children(&db).unwrap();
        assert_eq!(db.worker(&worker).unwrap()["status"], "notified");
        let messages = db.messages(&worker, 0, 100).unwrap();
        assert_eq!(messages.as_array().unwrap().len(), 1);
        drop(db);
        let db = Store::open(dir.path()).unwrap();
        notify_completed_children(&db).unwrap();
        assert_eq!(db.messages(&worker, 0, 100).unwrap(), messages);
        wake_notified(&db).unwrap();
        assert_ne!(db.worker(&worker).unwrap()["step"], step["id"]);
    }
    #[test]
    fn remote_terminal_snapshot_notifies_original_local_worker() {
        let (_dir, db, parent, child) = fixture();
        let worker = db.register(&parent, None).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        db.conn
            .execute(
                "INSERT INTO external_ops VALUES(?,?,'done',?)",
                rusqlite::params![
                    parent,
                    format!("delegation.caller:{child}"),
                    json!({"worker":worker}).to_string()
                ],
            )
            .unwrap();
        db.conn.execute("INSERT INTO remote_context VALUES(?,?)",rusqlite::params![parent,json!({"children":[{"task":child,"status":"failed","version":1,"caller_worker":null}]}).to_string()]).unwrap();
        notify_completed_children(&db).unwrap();
        notify_completed_children(&db).unwrap();
        assert_eq!(
            db.messages(&worker, 0, 100)
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(db.worker(&worker).unwrap()["status"], "notified");
    }
    #[test]
    fn legacy_answers_remain_visible_until_inherited_context_contains_them() {
        let (_dir, db, parent, _child) = fixture();
        db.conn
            .execute(
                "INSERT INTO questions VALUES('legacy',?,'original question','original answer')",
                [&parent],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO question_context VALUES('legacy','input_consumed',NULL)",
                [],
            )
            .unwrap();
        assert_eq!(
            answered_questions(&db, &parent).unwrap()[0]["answer"],
            "original answer"
        );
        db.conn.execute("INSERT INTO context_records VALUES('answer:legacy',?,1,'answer','original answer','caller',1)",[&parent]).unwrap();
        assert!(answered_questions(&db, &parent).unwrap().is_empty());
    }
}

#[cfg(test)]
mod scheduling_limits {
    use super::*;
    #[tokio::test]
    async fn runtime_ceiling_changes_without_interrupting_dispatched_attempts() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let temp = tempfile::tempdir().unwrap();
                let db = Store::open(temp.path()).unwrap();
                let mut settings = Settings {
                    concurrency: 64,
                    ..Default::default()
                };
                settings.limits.workers = 64;
                let mut plan = template::compile(
                    "simulated",
                    &template::load_templates(temp.path()).unwrap(),
                    BTreeMap::from([("task".into(), "limits".into())]),
                )
                .unwrap();
                let step = plan.steps[0].clone();
                plan.steps = (0..6)
                    .map(|n| {
                        let mut s = step.clone();
                        s.id = format!("step-{n}");
                        s.needs.clear();
                        s.scope = vec![format!("file-{n}")];
                        s
                    })
                    .collect();
                db.submit("limits", temp.path(), &settings, &plan).unwrap();
                crate::management::set(&db, "concurrency", "3").unwrap();
                let mut scheduler = Scheduler {
                    running: HashMap::new(),
                    limit: 64,
                };
                scheduler.tick(&db, true).await.unwrap();
                assert_eq!(crate::management::status(&db).unwrap()["active"], 3);
                crate::management::set(&db, "concurrency", "1").unwrap();
                scheduler.tick(&db, true).await.unwrap();
                assert_eq!(crate::management::status(&db).unwrap()["active"], 3);
                crate::management::set(&db, "concurrency", "4").unwrap();
                scheduler.tick(&db, true).await.unwrap();
                assert_eq!(crate::management::status(&db).unwrap()["active"], 4);
                crate::management::set(&db, "draining", "true").unwrap();
                crate::management::set(&db, "concurrency", "6").unwrap();
                scheduler.tick(&db, true).await.unwrap();
                assert_eq!(crate::management::status(&db).unwrap()["active"], 4);
                for (_, _, _, handle) in scheduler.running.into_values() {
                    handle.abort();
                }
            })
            .await;
    }
}
