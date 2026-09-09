use crate::{
    config::Settings,
    store::{Store, id, now},
    template,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

mod operations;
pub use operations::OPERATIONS;

fn string<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing string {k}"))
}
fn strings(v: &Value, k: &str) -> Result<Vec<String>> {
    serde_json::from_value(v[k].clone()).with_context(|| format!("{k} must be a string array"))
}
mod schemas;
pub use schemas::{admin_schema, schema};

pub fn worker_allowed(name: &str) -> bool {
    [
        "runtime_capabilities",
        "plan_execution",
        "list_skills",
        "read_skill",
        "integrate_child",
        "environments",
        "delegate_task",
        "list_children",
        "read_context",
        "pending_questions",
        "escalate_question",
        "answer_question",
        "propose_steps",
        "request_question",
        "send_message",
        "read_messages",
        "acknowledge_messages",
        "list_workers",
        "set_worker_status",
        "join_channel",
        "claim_paths",
        "transfer_claim",
        "register_workspace",
        "put_artifact",
        "get_artifact",
        "reuse_artifact",
        "add_knowledge",
        "knowledge",
        "link_knowledge",
    ]
    .contains(&name)
}
pub fn dispatch(db: &Store, name: &str, mut args: Value, token: Option<&str>) -> Result<Value> {
    if !args.is_object() {
        bail!("arguments must be an object");
    }
    if let Some(token) = token {
        if !worker_allowed(name) {
            bail!("operation unavailable to worker credentials");
        }
        if args
            .as_object()
            .is_some_and(|a| a.keys().any(|k| k.starts_with("_")))
        {
            bail!("reserved runtime argument");
        }
        let w = db.authenticate(token)?;
        for (key, source) in [("task", "task"), ("worker", "id")] {
            if args.get(key).is_some() && args[key] != w[source] {
                bail!("worker credential scope violation");
            }
            args[key] = w[source].clone();
        }
        if let Some(step) = args.get("step")
            && step != &w["step"]
        {
            bail!("cannot attribute to another worker's step");
        }
        // Workers may report evidence, but verification is an authoritative runtime decision.
        if args
            .as_object_mut()
            .expect("arguments object")
            .remove("verified")
            .is_some()
        {
            db.event(w["task"].as_str().context("worker task")?, "tool.argument_dropped",
                json!({"tool":name,"worker":w["id"],"field":"verified","reason":"verification is runtime-owned"}))?;
        }
        if matches!(name, "put_artifact" | "add_knowledge") {
            args["step"] = w["step"].clone();
        }
    }
    if matches!(name, "runtime_capabilities" | "plan_execution") {
        if let Some(task) = args["task"].as_str() {
            db.task(task)?;
            if let Some(value) = crate::federation::forward(db, task, name, &args)? {
                return Ok(value);
            }
        }
        return if name == "runtime_capabilities" {
            crate::capabilities::inventory(db)
        } else {
            crate::orchestration::plan(db, &args)
        };
    }
    if name == "agent_setup" {
        return crate::agent_setup::dispatch(db, &args);
    }
    match name {
        "skill_pack_list" => {
            return crate::skill_catalog::report(&crate::skill_catalog::load_for(&db.root)?);
        }
        "skill_pack_install" => {
            let packet =
                crate::skill_catalog::load_from(std::path::Path::new(string(&args, "path")?))?;
            return crate::management::install_catalog(db, &packet);
        }
        "skill_inspect" => return crate::skill_policy::inspect(db, &args),
        "skill_propose" => return crate::skill_policy::propose(db, &args),
        "skill_apply" => return crate::skill_policy::apply(db, &args),
        "skill_history" => return crate::skill_policy::history(db, &args),
        "skill_rollback" => return crate::skill_policy::rollback(db, &args),
        _ => {}
    }
    if name == "runtime_rename" {
        return crate::runtime_directory::rename(db, string(&args, "id")?, string(&args, "name")?);
    }
    if name == "runtime_forget" {
        return crate::runtime_directory::forget(db, string(&args, "id")?);
    }
    if matches!(
        name,
        "runtime_inspect" | "runtime_destroy" | "runtime_stop" | "runtime_start"
    ) {
        args["id"] = json!(crate::runtime_directory::resolve_known(
            db,
            string(&args, "id")?
        )?);
    }
    if let Some(result) = crate::management::dispatch(db, name, &args)? {
        return Ok(result);
    }
    if name == "shutdown" {
        std::fs::write(db.root.join("shutdown.request"), b"shutdown")?;
        return Ok(json!({"stopping":true}));
    }
    if name == "submit_task" && args.get("request_id").is_some() {
        return crate::submission::submit(db, &args);
    }
    if name == "submit_task" && args["execution"].is_object() {
        let selection = crate::execution_selection::prepare(db, &args["execution"], None)?;
        let selected = selection["selected"]["runtime"]
            .as_str()
            .context("selected runtime")?;
        let inventory = crate::capabilities::inventory(db)?;
        let local = inventory["runtimes"]
            .as_array()
            .context("runtimes")?
            .iter()
            .any(|runtime| runtime["local"] == true && runtime["runtime"] == selected);
        if !local && args["on"].is_null() {
            args["on"] = json!(selected);
        }
    }
    if name == "submit_task" && !args["on"].is_null() && args["on"] != "local" {
        return crate::remote_submit::submit(db, &args);
    }
    if name == "submit_task" {
        let repo = Path::new(string(&args, "repo")?).canonicalize()?;
        let settings = Settings::load(&repo)?;
        let templates = template::load_templates(&crate::branding::templates(&repo))?;
        let objective = string(&args, "objective")?;
        let plan = template::compile(
            args["template"]
                .as_str()
                .unwrap_or(&settings.default_template),
            &templates,
            BTreeMap::from([("task".into(), objective.into())]),
        )?;
        let selection = args
            .get("execution")
            .map(|input| crate::execution_selection::prepare(db, input, None))
            .transpose()?;
        for s in &plan.steps {
            if selection.is_none() && s.kind == "agent" && !settings.executors.contains_key(&s.role)
            {
                bail!("unconfigured executor role {}", s.role);
            }
        }
        return db.atomic(|| {
            let oid = db.submit(objective, &repo, &settings, &plan)?;
            if let Some(selection) = &selection {
                crate::execution_selection::pin(db, &oid, selection)?;
                crate::execution_selection::validate_target(db, &oid, None)?;
            }
            if let Some(records) = args["context"].as_array() {
                for record in records {
                    crate::delegation::update_context(db, &oid, record)?;
                }
            }
            Ok(json!({"id":oid}))
        });
    }
    if name == "list_tasks" {
        return Ok(json!(db.rows(
            "SELECT id,objective,repo,status,created FROM tasks ORDER BY created DESC",
            &[]
        )?));
    }
    let oid = string(&args, "task")?;
    db.task(oid)?;
    if token.is_some() && worker_allowed(name) {
        db.event(
            oid,
            "coordination.call",
            json!({"worker":args["worker"],"tool":name}),
        )?;
    }
    if let Some(wid) = args["worker"].as_str()
        && db.worker(wid)?["task"] != oid
    {
        bail!("worker belongs to another task");
    }
    if [
        "integrate_child",
        "add_knowledge",
        "knowledge",
        "link_knowledge",
        "delegate_task",
        "list_children",
        "read_context",
        "pending_questions",
        "request_question",
        "answer_question",
        "escalate_question",
    ]
    .contains(&name)
        && let Some(result) = crate::federation::forward(db, oid, name, &args)?
    {
        if name == "request_question" {
            let qid = result["id"].as_str().context("remote question ID")?;
            db.atomic(|| {
                db.conn.execute(
                    "INSERT OR IGNORE INTO questions VALUES(?,?,?,NULL)",
                    rusqlite::params![qid, oid, args["question"].as_str()],
                )?;
                db.conn.execute(
                    "INSERT OR IGNORE INTO question_context VALUES(?,'input',?)",
                    rusqlite::params![qid, args["worker"].as_str()],
                )?;
                Ok(())
            })?;
        }
        return Ok(result);
    }
    match name {
        "remote_result" => crate::remote_submit::result(db,oid),
        "integrate_child" => crate::federation::integrate_child(db,oid,&args),
        "environments" => Ok(json!(db.rows("SELECT id,task,attempt,kind,state,pid,created,expires,evidence FROM app_environments WHERE task=?",&[&oid])?)),
        "refresh_bundles" => db.atomic(|| {let names:Vec<String>=db.rows("SELECT name FROM task_bundles WHERE task=?",&[&oid])?.iter().filter_map(|r|r["name"].as_str().map(str::to_owned)).collect();db.conn.execute("DELETE FROM task_bundles WHERE task=?",[oid])?;crate::secrets::select(db,oid,&names)?;Ok(json!({"refreshed":true}))}),
        "metrics" => crate::metrics::report(db, oid),
        "inspect" => Ok(
            json!({"task":db.task(oid)?,"execution":crate::execution_selection::policy(db,oid)?,"outputs":db.rows("SELECT outputs FROM workflow_outputs WHERE task=?",&[&oid])?,"steps":db.steps(oid)?,"workers":db.rows("SELECT id,step,status,workspace,branch,base FROM workers WHERE task=?",&[&oid])?,"attempts":db.rows("SELECT a.* FROM attempts a JOIN steps t ON a.step=t.id WHERE t.task=? ORDER BY a.started",&[&oid])?,"questions":db.rows("SELECT * FROM questions WHERE task=?",&[&oid])?,"claims":db.rows("SELECT * FROM claims WHERE task=?",&[&oid])?,"integrations":db.rows("SELECT * FROM integrations WHERE task=?",&[&oid])?,"external_ops":db.rows("SELECT * FROM external_ops WHERE task=?",&[&oid])?,"remote":db.rows("SELECT * FROM remote_links WHERE task=?",&[&oid])?,"artifacts":db.rows("SELECT name,hash,verified FROM artifact_links WHERE task=?",&[&oid])?}),
        ),
        "events" => {
            let cursor:i64=if let Some(consumer)=args["consumer"].as_str(){db.conn.query_row("SELECT COALESCE((SELECT seq FROM event_receipts WHERE task=? AND consumer=?),0)",rusqlite::params![oid,consumer],|r|r.get(0))?}else{0};
            Ok(json!(db.rows("SELECT * FROM events WHERE task=? AND seq>? ORDER BY seq LIMIT 1000",&[&oid,&args["after"].as_i64().unwrap_or(cursor)])?))
        },
        "register_worker" => db.register(oid, args["step"].as_str()),
        "list_workers" => Ok(json!(db.rows(
            "SELECT id,step,status,workspace,branch,base,updated FROM workers WHERE task=?",
            &[&oid]
        )?)),
        "register_workspace" => {
            crate::git::register(
                db,
                oid,
                string(&args, "worker")?,
                Path::new(string(&args, "path")?),
                string(&args, "branch")?,
                string(&args, "base")?,
            )?;
            Ok(json!({"registered":true}))
        }
        "send_message" => db.send(
            oid,
            string(&args, "worker")?,
            string(&args, "id")?,
            string(&args, "destination")?,
            string(&args, "body")?,
            args.get("refs").unwrap_or(&json!({})),
            args["actionable"].as_bool().unwrap_or(false),
        ),
        "read_messages" => db.messages(
            string(&args, "worker")?,
            args["after"].as_i64().unwrap_or(0),
            args["limit"].as_i64().unwrap_or(100),
        ),
        "acknowledge_messages" => {
            db.acknowledge(string(&args, "worker")?, &strings(&args, "ids")?)?;
            Ok(json!({"acknowledged":true}))
        }
        "join_channel" => {
            let channel = string(&args, "channel")?;
            if channel.is_empty() || channel == "task" {
                bail!("invalid channel name");
            }
            db.conn.execute(
                "INSERT OR IGNORE INTO channels VALUES(?,?,?)",
                rusqlite::params![oid, channel, string(&args, "worker")?],
            )?;
            Ok(json!({"joined":true}))
        }
        "set_worker_status" => {
            let status = string(&args, "status")?;
            if !["idle", "working", "blocked", "stopped"].contains(&status) {
                bail!("invalid worker status");
            }
            db.conn.execute(
                "UPDATE workers SET status=?,updated=? WHERE id=?",
                rusqlite::params![status, now(), string(&args, "worker")?],
            )?;
            db.event(oid, "worker.status", args.clone())?;
            Ok(json!({"status":status}))
        }
        "claim_paths" => {
            db.claim(oid, string(&args, "worker")?, &strings(&args, "paths")?)?;
            Ok(json!({"claimed":true}))
        }
        "transfer_claim" => {
            db.transfer(
                oid,
                string(&args, "worker")?,
                string(&args, "to")?,
                string(&args, "path")?,
            )?;
            Ok(json!({"transferred":true}))
        }
        "release_claims" => {
            let wid = string(&args, "worker")?;
            let w = db.worker(wid)?;
            if !["idle", "stopped"].contains(&w["status"].as_str().unwrap_or("")) {
                bail!("stop or reconcile worker before releasing claims");
            }
            let running: i64 = db.conn.query_row(
                "SELECT COUNT(*) FROM attempts WHERE worker=? AND state IN ('running','uncertain')",
                [wid],
                |r| r.get(0),
            )?;
            if running > 0 {
                bail!("worker still has an active or uncertain attempt");
            }
            db.conn
                .execute("DELETE FROM claims WHERE worker=?", [wid])?;
            Ok(json!({"released":true}))
        }
        "cancel" => {
            db.atomic(||{db.conn.execute("WITH RECURSIVE subtree(id) AS (SELECT ? UNION ALL SELECT t.task FROM task_tree t JOIN subtree s ON t.parent=s.id) UPDATE tasks SET status='cancelled' WHERE id IN (SELECT id FROM subtree)",[oid])?;db.conn.execute("UPDATE steps SET state='cancelled' WHERE task IN (SELECT id FROM tasks WHERE status='cancelled') AND state IN ('pending','running','waiting')",[])?;db.event(oid,"task.cancelled",json!({}))?;Ok(())})?;
            Ok(json!({"cancelled":true}))
        }
        "resume" => {
            if !db.rows("SELECT task FROM remote_links WHERE task=?", &[&oid])?.is_empty() {
                bail!("this task runs on a remote runtime and cannot resume locally; submit a new task with --on targeting that runtime");
            }
            if db.task(oid)?["status"]=="blocked" && db.steps(oid)?.iter().all(|t|t["state"]=="succeeded"||t["state"]=="skipped") {
                bail!("completed result needs revalidation; add a verification step before resuming");
            }
            let uncertain:i64=db.conn.query_row("SELECT COUNT(*) FROM attempts a JOIN steps t ON t.id=a.step WHERE t.task=? AND a.state IN ('uncertain','running')",[oid],|r|r.get(0))?;
            if uncertain > 0 {
                bail!("reconcile interrupted workers or wait for running attempts before resuming");
            }
            let questions: i64 = db.conn.query_row(
                "SELECT COUNT(*) FROM questions WHERE task=? AND answer IS NULL",
                [oid],
                |r| r.get(0),
            )?;
            if questions > 0 {
                bail!("answer pending questions before resuming");
            }
            db.atomic(||{db.conn.execute("UPDATE steps SET state='pending' WHERE task=? AND state IN ('failed','cancelled','uncertain')",[oid])?;db.conn.execute("UPDATE tasks SET status='running' WHERE id=?",[oid])?;db.event(oid,"task.resumed",json!({}))?;Ok(())})?;
            Ok(json!({"resumed":true}))
        }
        "request_question" => crate::delegation::ask(db,oid,&args),
        "delegate_task" | "list_children" | "read_context" | "update_context" | "pending_questions" | "escalate_question" | "ack_events" => crate::delegation::dispatch(db,oid,name,&args),
        "answer_question" => {
            let routed: i64 = db.conn.query_row("SELECT COUNT(*) FROM question_routes WHERE question=?", [args["question"].as_str()], |r|r.get(0))?;
            if routed > 0 {return crate::delegation::question_action(db,oid,&args,false);}
            if token.is_some(){bail!("workers cannot answer administrative questions");}
            let answer = string(&args, "answer")?;
            let qid = string(&args, "question")?;
            db.atomic(||{
                if db.conn.execute("UPDATE questions SET answer=? WHERE id=? AND task=? AND answer IS NULL",rusqlite::params![answer,qid,oid])?!=1{bail!("unknown or answered question");}
                let context=db.rows("SELECT * FROM question_context WHERE question=?",&[&qid])?;
                let input=context.first().is_some_and(|q|q["purpose"]=="input");
                let pending:i64=db.conn.query_row("SELECT COUNT(*) FROM questions WHERE task=? AND answer IS NULL",[oid],|r|r.get(0))?;
                if pending==0 && (input || answer.eq_ignore_ascii_case("yes")) {
                    db.conn.execute("UPDATE tasks SET status='running' WHERE id=? AND status='waiting'",[oid])?;
                    if input {db.conn.execute("UPDATE question_context SET purpose='input_answered' WHERE question=?",[qid])?;db.conn.execute("UPDATE steps SET state='pending' WHERE id IN (SELECT step FROM workers WHERE id IN (SELECT worker FROM question_context WHERE question=?)) AND state='failed'",[qid])?;}
                } else if !input && !answer.eq_ignore_ascii_case("yes") {db.conn.execute("UPDATE tasks SET status='paused' WHERE id=? AND status='waiting'",[oid])?;}
                db.event(oid,"question.answered",args.clone())?;Ok(json!({"answered":true}))
            })
        }
        "reconcile_worker" => {
            let wid = string(&args, "worker")?;
            let attempts = db.rows(
                "SELECT id,pid FROM attempts WHERE worker=? AND state='uncertain'",
                &[&wid],
            )?;
            for a in &attempts {
                if let Some(pid) = a["pid"].as_i64()
                    && crate::executor::process_alive(pid as i32)
                {
                    bail!(
                        "process {pid} still exists; stop it and inspect its effects before reconciliation"
                    );
                }
            }
            db.atomic(||{db.conn.execute("UPDATE attempts SET state='interrupted',finished=? WHERE worker=? AND state='uncertain'",rusqlite::params![now(),wid])?;db.conn.execute("UPDATE workers SET status='stopped' WHERE id=?",[wid])?;db.event(oid,"worker.reconciled",json!({"worker":wid}))?;Ok(())})?;
            Ok(json!({"reconciled":true,"claims_preserved":true}))
        }
        "list_skills" => crate::skills::catalog(db, oid),
        "read_skill" => crate::skills::read(db, oid, &args),
        "propose_steps" => {
            let wid = string(&args, "worker")?;
            let w = db.worker(wid)?;
            let step = w["step"]
                .as_str()
                .context("planner requires an assigned step")?;
            let step_row = db
                .rows("SELECT * FROM steps WHERE id=?", &[&step])?
                .into_iter()
                .next()
                .context("planner step")?;
            let planner = Store::step(&step_row)?;
            if planner.role != "planner" || step_row["state"] != "running" {
                bail!("only an actively assigned planner may propose workflow changes");
            }
            let task = db.task(oid)?;
            let settings: Settings =
                serde_json::from_str(task["settings"].as_str().context("settings")?)?;
            let mut plan: template::Plan =
                serde_json::from_str(task["plan"].as_str().context("plan")?)?;
            let mut added = template::parse_steps(&args["steps"])?;
            if added.is_empty() || added.len() > 32 {
                bail!("propose between 1 and 32 steps");
            }
            for (index, step) in added.iter_mut().enumerate() {
                if step.template.is_some() {
                    bail!("steps[{index}].template: planner proposals must be expanded coding or verification steps; omit template");
                }
                if step.kind == "delivery" {
                    bail!("steps[{index}].kind: planner proposals cannot request delivery; use agent, command, simulated, or environment");
                }
                if !settings.executors.contains_key(&step.role) {
                    bail!("steps[{index}].role: unknown executor role {}; choose one of: {}", step.role, settings.executors.keys().cloned().collect::<Vec<_>>().join(", "));
                }
                if !settings.allow_commands && step.kind == "command" {
                    bail!("steps[{index}].kind: commands are disabled");
                }
                if !step.needs.contains(&planner.id) {
                    step.needs.push(planner.id.clone());
                }
            }
            let new_ids: Vec<_> = added.iter().map(|s| s.id.clone()).collect();
            let existing = db.steps(oid)?;
            let mut updated = vec![];
            for step in &mut plan.steps {
                if step.needs.contains(&planner.id)
                    && existing
                        .iter()
                        .any(|t| t["name"] == step.id && t["state"] == "pending")
                {
                    step.needs.extend(new_ids.clone());
                    updated.push(step.clone());
                }
            }
            plan.steps.extend(added.clone());
            template::validate(&plan.steps)?;
            crate::skills::validate_steps(&crate::skills::packet(db, oid)?, &plan.steps)?;
            db.atomic(|| {
                crate::delegation::invalidate_acceptance(db, oid)?;
                for step in &added {
                    db.conn.execute(
                        "INSERT INTO steps(id,task,name,spec,state) VALUES(?,?,?,?,'pending')",
                        rusqlite::params![id(), oid, step.id, serde_json::to_string(step)?],
                    )?;
                }
                for step in &updated {
                    db.conn.execute(
                        "UPDATE steps SET spec=? WHERE task=? AND name=? AND state='pending'",
                        rusqlite::params![serde_json::to_string(step)?, oid, step.id],
                    )?;
                }
                let revision: i64 = db.conn.query_row(
                    "SELECT MAX(revision)+1 FROM revisions WHERE task=?",
                    [oid],
                    |r| r.get(0),
                )?;
                let serialized = serde_json::to_string(&plan)?;
                db.conn.execute(
                    "INSERT INTO revisions VALUES(?,?,?,?)",
                    rusqlite::params![oid, revision, serialized, now()],
                )?;
                db.conn.execute(
                    "UPDATE tasks SET plan=? WHERE id=?",
                    rusqlite::params![serialized, oid],
                )?;
                db.event(
                    oid,
                    "workflow.proposed",
                    json!({"worker":wid,"revision":revision,"steps":new_ids}),
                )?;
                Ok(json!({"revision":revision,"steps":new_ids}))
            })
        }
        "add_steps" => {
            let added = template::parse_steps(&args["steps"])?;
            if let Some(result)=crate::federation::revise_child(db,oid,&args["steps"])?{return Ok(result);}
            let task = db.task(oid)?;
            let mut plan: template::Plan =
                serde_json::from_str(task["plan"].as_str().context("plan")?)?;
            plan.steps.extend(added.clone());
            template::validate(&plan.steps)?;
            crate::skills::validate_steps(&crate::skills::packet(db, oid)?, &plan.steps)?;
            db.atomic(||{crate::delegation::invalidate_acceptance(db,oid)?;for s in &added{db.conn.execute("INSERT INTO steps(id,task,name,spec,state) VALUES(?,?,?,?,'pending')",rusqlite::params![id(),oid,s.id,serde_json::to_string(s)?])?;}let rev:i64=db.conn.query_row("SELECT COALESCE(MAX(revision),0)+1 FROM revisions WHERE task=?",[oid],|r|r.get(0))?;let serialized=serde_json::to_string(&plan)?;db.conn.execute("INSERT INTO revisions VALUES(?,?,?,?)",rusqlite::params![oid,rev,serialized,now()])?;db.conn.execute("UPDATE tasks SET plan=?,status=CASE WHEN status='succeeded' THEN 'running' ELSE status END WHERE id=?",rusqlite::params![serialized,oid])?;db.event(oid,"workflow.revised",json!({"revision":rev}))?;Ok(json!({"revision":rev}))})
        }
        "put_artifact" => Ok(
            json!({"hash":db.artifact(oid,args["step"].as_str(),string(&args,"name")?,string(&args,"content")?.as_bytes(),args.get("inputs").unwrap_or(&json!({})),args["verified"].as_bool().unwrap_or(false))?}),
        ),
        "get_artifact" => {
            let h = string(&args, "hash")?;
            if h.len() != 64 || !h.bytes().all(|c| c.is_ascii_hexdigit()) {
                bail!("invalid hash");
            }
            if db
                .rows(
                    "SELECT hash FROM artifact_links WHERE task=? AND hash=?",
                    &[&oid, &h],
                )?
                .is_empty()
            {
                bail!("artifact not linked to task");
            }
            let data = std::fs::read(db.root.join("artifacts").join(h))?;
            if crate::store::hash(&data) != h {
                bail!("artifact integrity failure");
            }
            Ok(json!({"hash":h,"content":String::from_utf8(data)?}))
        }
        "reuse_artifact" => Ok(json!(db.rows(
            "SELECT * FROM artifact_links WHERE task=? AND name=? AND inputs=? AND verified=1",
            &[&oid, &string(&args, "name")?, &args["inputs"].to_string()]
        )?)),
        "add_knowledge" => {
            let kid = id();
            let kind = string(&args, "kind")?;
            if !["fact", "decision", "evidence"].contains(&kind) {
                bail!("invalid knowledge kind");
            }
            if args["provenance"].is_null() {
                bail!("provenance is required");
            }
            if let Some(step) = args["step"].as_str()
                && !db.steps(oid)?.iter().any(|t| t["id"] == step)
            {
                bail!("step not in task");
            }
            db.conn.execute(
                "INSERT INTO knowledge VALUES(?,?,?,?,?,?,?,?)",
                rusqlite::params![
                    kid,
                    oid,
                    args["step"].as_str(),
                    kind,
                    string(&args, "content")?,
                    args["provenance"].to_string(),
                    args["verified"].as_bool().unwrap_or(false),
                    args["inputs"].to_string()
                ],
            )?;
            let root=crate::delegation::root(db,oid)?;let version=crate::delegation::tree(db,oid)?["version"].as_i64();
            db.conn.execute("INSERT INTO context_records VALUES(?,?,?,?,?,?,0)",rusqlite::params![kid,root,version,format!("supporting_{kind}"),string(&args,"content")?,json!({"source_task":oid,"provenance":args["provenance"],"verified":args["verified"].as_bool().unwrap_or(false)}).to_string()])?;
            Ok(json!({"id":kid}))
        }
        "knowledge" => Ok(json!(
            db.rows("SELECT * FROM knowledge WHERE task=?", &[&oid])?
        )),
        "link_knowledge" => {
            let source = string(&args, "source")?;
            let target = string(&args, "target")?;
            for k in [source, target] {
                if db
                    .rows(
                        "SELECT id FROM knowledge WHERE id=? AND task=?",
                        &[&k, &oid],
                    )?
                    .is_empty()
                {
                    bail!("knowledge outside task");
                }
            }
            db.conn.execute(
                "INSERT OR IGNORE INTO knowledge_edges VALUES(?,?,?)",
                rusqlite::params![source, target, string(&args, "relation")?],
            )?;
            Ok(json!({"linked":true}))
        }
        "integrate" => crate::git::integrate(
            db,
            oid,
            string(&args, "worker")?,
            &args
                .get("validation")
                .map(|x| serde_json::from_value::<Vec<String>>(x.clone()))
                .transpose()?
                .unwrap_or_default(),
        ),
        _ => bail!("unknown operation {name}"),
    }
}
pub fn mcp_response(
    request: &Value,
    call: impl FnOnce(&str, Value) -> Result<Value>,
) -> Option<Value> {
    let id = request.get("id")?.clone();
    let method = request["method"].as_str().unwrap_or("");
    let result = match method {
        "initialize" => Ok(
            json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"horde","version":env!("CARGO_PKG_VERSION")}}),
        ),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(
            json!({"tools":OPERATIONS.iter().map(|(n,d)|json!({"name":n,"description":d,"inputSchema":admin_schema(n)})).collect::<Vec<_>>()}),
        ),
        "tools/call" => {
            let p = &request["params"];
            match call(
                p["name"].as_str().unwrap_or(""),
                p.get("arguments").cloned().unwrap_or(json!({})),
            ) {
                Ok(v) => {
                    Ok(json!({"content":[{"type":"text","text":v.to_string()}],"isError":false}))
                }
                Err(e) => {
                    Ok(json!({"content":[{"type":"text","text":format!("{e:#}")}],"isError":true}))
                }
            }
        }
        _ => Err(anyhow::anyhow!("method not found")),
    };
    Some(match result {
        Ok(v) => json!({"jsonrpc":"2.0","id":id,"result":v}),
        Err(e) => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":e.to_string()}}),
    })
}
