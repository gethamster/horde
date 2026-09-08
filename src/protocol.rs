use crate::{
    config::Settings,
    store::{Store, id, now},
    template,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

pub const OPERATIONS: &[(&str, &str)] = &[
    (
        "runtime_updates_resume",
        "Resume queued fleet updates after inspecting a failed rollout",
    ),
    (
        "runtime_reconcile",
        "Inspect and adopt a provider resource after uncertain provisioning",
    ),
    ("runtime_list", "List managed runtime"),
    ("runtime_inspect", "Inspect managed runtime"),
    ("runtime_create", "Create managed runtime"),
    ("runtime_destroy", "Destroy managed runtime"),
    ("runtime_restart", "Restart managed runtime"),
    ("runtime_update", "Update managed runtime"),
    ("runtime_stop", "Stop managed runtime"),
    ("runtime_start", "Start managed runtime"),
    ("runtime_config_get", "Read runtime concurrency"),
    (
        "runtime_config_set",
        "Change runtime concurrency without interrupting work",
    ),
    ("runtime_drain", "Stop new dispatches"),
    ("runtime_resume", "Resume runtime dispatch"),
    ("runtime_status", "Inspect version and drain status"),
    ("account_status", "Inspect account capacity and freshness"),
    (
        "account_observe",
        "Record an explicit account quota observation",
    ),
    ("management_events", "Read runtime management events"),
    ("management_ack", "Acknowledge management events"),
    (
        "integrate_child",
        "Import a completed child and verify the combined result",
    ),
    (
        "environments",
        "Inspect owned application environments and cleanup state",
    ),
    (
        "refresh_bundles",
        "Explicitly refresh selected application bundle versions",
    ),
    (
        "delegate_task",
        "Create a bounded child task; id is required for retry deduplication",
    ),
    ("list_children", "Inspect immediate child tasks"),
    (
        "read_context",
        "Read original source context using bounded pages",
    ),
    (
        "update_context",
        "Root caller: append authoritative context with provenance and invalidate stale acceptance",
    ),
    (
        "pending_questions",
        "Read questions addressed to this caller",
    ),
    (
        "escalate_question",
        "Forward the original question one level to your parent",
    ),
    (
        "ack_events",
        "Acknowledge durable events through a consumer cursor",
    ),
    (
        "propose_steps",
        "Planner-only: propose parallel or dependent steps. Runtime validates and inserts them before the planning step’s pending successors",
    ),
    (
        "request_question",
        "Ask for required information; hold this worker while its caller answers or escalates",
    ),
    (
        "metrics",
        "Reported usage, cost, latency, retries, and coordination overhead",
    ),
    (
        "submit_task",
        "Submit objective, repo, and optional template; returns durable task id",
    ),
    (
        "inspect",
        "Inspect a task including steps, attempts, workers, questions, and integration",
    ),
    ("list_tasks", "List submitted tasks and their status"),
    ("events", "Read ordered activity events"),
    ("cancel", "Cancel a task"),
    (
        "resume",
        "Resume paused or failed work; interrupted processes require reconciliation",
    ),
    ("answer_question", "Answer a pending question"),
    (
        "register_worker",
        "Create task-scoped worker identity and token",
    ),
    (
        "register_workspace",
        "Register path, branch and base before editing",
    ),
    (
        "send_message",
        "Persist a message to worker id, group:name, or task. Supply id for deduplication",
    ),
    (
        "read_messages",
        "Read unacknowledged messages; after is an optional sequence cursor",
    ),
    (
        "acknowledge_messages",
        "Acknowledge an explicit list of message ids",
    ),
    ("list_workers", "List worker identities and status"),
    (
        "set_worker_status",
        "Set idle, working, blocked, or stopped status",
    ),
    ("join_channel", "Join a named group channel"),
    (
        "claim_paths",
        "Acquire exclusive paths in a registered workspace",
    ),
    (
        "transfer_claim",
        "Atomically hand off a claim to another worker",
    ),
    (
        "release_claims",
        "Release claims after worker is stopped or idle",
    ),
    (
        "reconcile_worker",
        "Confirm an interrupted process exited, then make step resumable",
    ),
    (
        "add_steps",
        "Append validated steps as a durable workflow revision",
    ),
    (
        "put_artifact",
        "Store content with input fingerprint and verification status",
    ),
    ("get_artifact", "Read a task artifact by hash"),
    (
        "reuse_artifact",
        "Find a verified artifact with identical input fingerprint",
    ),
    (
        "add_knowledge",
        "Store fact, decision or evidence with provenance",
    ),
    ("knowledge", "Read knowledge for this task"),
    (
        "link_knowledge",
        "Link two knowledge records with a relationship",
    ),
    (
        "integrate",
        "Integrate a committed worker worktree; optional validation argv",
    ),
];
fn string<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing string {k}"))
}
fn strings(v: &Value, k: &str) -> Result<Vec<String>> {
    serde_json::from_value(v[k].clone()).with_context(|| format!("{k} must be a string array"))
}
pub fn schema(name: &str) -> Value {
    let fields: &[(&str, &str)] = match name {
        "runtime_updates_resume" => &[],
        "runtime_reconcile" => &[
            ("id", "string"),
            ("request_id", "string"),
            ("resource", "string"),
        ],
        "runtime_list" => &[],
        "runtime_inspect" => &[("id", "string")],
        "runtime_create" | "runtime_destroy" | "runtime_restart" | "runtime_update"
        | "runtime_stop" | "runtime_start" => &[
            ("id", "string"),
            ("request_id", "string"),
            ("profile", "string"),
            ("version", "string"),
        ],
        "runtime_config_get" | "runtime_drain" | "runtime_resume" | "runtime_status"
        | "account_status" => &[],
        "runtime_config_set" => &[("concurrency", "integer")],
        "account_observe" => &[
            ("account", "string"),
            ("provider", "string"),
            ("window", "string"),
            ("used_percent", "number"),
            ("reset_at", "integer"),
            ("observed_at", "integer"),
            ("source", "string"),
        ],
        "management_events" => &[("after", "integer")],
        "management_ack" => &[("consumer", "string"), ("seq", "integer")],
        "submit_task" => &[
            ("context", "array"),
            ("objective", "string"),
            ("repo", "string"),
            ("template", "string"),
        ],
        "send_message" => &[
            ("task", "string"),
            ("worker", "string"),
            ("id", "string"),
            ("destination", "string"),
            ("body", "string"),
            ("refs", "object"),
            ("actionable", "boolean"),
        ],
        "read_messages" => &[
            ("task", "string"),
            ("worker", "string"),
            ("after", "integer"),
            ("limit", "integer"),
        ],
        "acknowledge_messages" => &[("task", "string"), ("worker", "string"), ("ids", "array")],
        "register_workspace" => &[
            ("task", "string"),
            ("worker", "string"),
            ("path", "string"),
            ("branch", "string"),
            ("base", "string"),
        ],
        "claim_paths" => &[("task", "string"), ("worker", "string"), ("paths", "array")],
        "transfer_claim" => &[
            ("task", "string"),
            ("worker", "string"),
            ("to", "string"),
            ("path", "string"),
        ],
        "set_worker_status" => &[
            ("task", "string"),
            ("worker", "string"),
            ("status", "string"),
        ],
        "join_channel" => &[
            ("task", "string"),
            ("worker", "string"),
            ("channel", "string"),
        ],
        "put_artifact" => &[
            ("task", "string"),
            ("worker", "string"),
            ("step", "string"),
            ("name", "string"),
            ("content", "string"),
            ("inputs", "object"),
            ("verified", "boolean"),
        ],
        "get_artifact" => &[("task", "string"), ("hash", "string")],
        "reuse_artifact" => &[("task", "string"), ("name", "string"), ("inputs", "object")],
        "add_knowledge" => &[
            ("task", "string"),
            ("step", "string"),
            ("kind", "string"),
            ("content", "string"),
            ("provenance", "object"),
            ("inputs", "object"),
            ("verified", "boolean"),
        ],
        "link_knowledge" => &[
            ("task", "string"),
            ("source", "string"),
            ("target", "string"),
            ("relation", "string"),
        ],
        "integrate_child" => &[
            ("task", "string"),
            ("worker", "string"),
            ("child", "string"),
            ("validation", "array"),
        ],
        "delegate_task" => &[
            ("task", "string"),
            ("worker", "string"),
            ("id", "string"),
            ("objective", "string"),
            ("template", "string"),
            ("peer", "string"),
            ("bundles", "array"),
        ],
        "read_context" => &[
            ("task", "string"),
            ("after", "integer"),
            ("limit", "integer"),
        ],
        "update_context" => &[
            ("task", "string"),
            ("id", "string"),
            ("kind", "string"),
            ("content", "string"),
            ("provenance", "string"),
            ("mandatory", "boolean"),
            ("supersedes", "array"),
        ],
        "escalate_question" => &[
            ("task", "string"),
            ("worker", "string"),
            ("question", "string"),
            ("commentary", "string"),
        ],
        "ack_events" => &[
            ("task", "string"),
            ("consumer", "string"),
            ("seq", "integer"),
        ],
        "request_question" => &[
            ("id", "string"),
            ("evidence", "string"),
            ("recommendation", "string"),
            ("human_only", "boolean"),
            ("task", "string"),
            ("worker", "string"),
            ("question", "string"),
        ],
        "answer_question" => &[
            ("worker", "string"),
            ("human", "boolean"),
            ("task", "string"),
            ("question", "string"),
            ("answer", "string"),
        ],
        "propose_steps" => &[("task", "string"), ("worker", "string"), ("steps", "array")],
        "add_steps" => &[("task", "string"), ("steps", "array")],
        "integrate" => &[
            ("task", "string"),
            ("worker", "string"),
            ("validation", "array"),
        ],
        "register_worker" => &[("task", "string"), ("step", "string")],
        "release_claims" | "reconcile_worker" => &[("task", "string"), ("worker", "string")],
        "events" => &[
            ("task", "string"),
            ("after", "integer"),
            ("consumer", "string"),
        ],
        "list_tasks" => &[],
        _ => &[("task", "string")],
    };
    let properties: serde_json::Map<String, Value> = fields
        .iter()
        .map(|(k, t)| {
            let mut s = json!({"type":t});
            if *t == "array" {
                s["items"] = if *k == "steps" {
                    template::step_schema()
                } else if *k == "context" {
                    json!({"type":"object"})
                } else {
                    json!({"type":"string"})
                };
            }
            if *k == "steps" {
                s["description"] = json!("Workflow Step objects. Call this tool with a steps array; step IDs are data, never tool names. Use expanded steps, not nested template invocations.");
                if name == "propose_steps" {
                    s["minItems"] = json!(1);
                    s["maxItems"] = json!(32);
                    s["items"]["properties"]["kind"]["enum"] = json!(["agent", "command", "simulated", "environment"]);
                    s["items"]["properties"]["template"] = json!({"type":"null","description":"Planner proposals must be expanded; omit template."});
                }
            }
            (k.to_string(), s)
        })
        .collect();
    let required: &[&str] = match name {
        "runtime_config_set" => &["concurrency"],
        "runtime_create" => &["id", "profile", "request_id"],
        "runtime_inspect" => &["id"],
        "runtime_update" => &["id", "request_id", "version"],
        "runtime_destroy" | "runtime_restart" | "runtime_start" | "runtime_stop" => {
            &["id", "request_id"]
        }
        "runtime_reconcile" => &["id", "request_id", "resource"],
        "account_observe" => &["account", "provider", "window", "observed_at", "source"],
        "management_ack" => &["consumer", "seq"],
        "submit_task" => &["objective", "repo"],
        "delegate_task" => &["id", "objective"],
        "integrate_child" => &["child", "validation"],
        "update_context" => &["content", "provenance"],
        "ack_events" => &["consumer", "seq"],
        "escalate_question" => &["question"],
        "send_message" => &["id", "destination", "body"],
        "request_question" => &["question"],
        "propose_steps" | "add_steps" => &["steps"],
        "register_workspace" => &["path", "branch", "base"],
        "claim_paths" => &["paths"],
        "transfer_claim" => &["to", "path"],
        "acknowledge_messages" => &["ids"],
        "set_worker_status" => &["status"],
        "join_channel" => &["channel"],
        "put_artifact" => &["name", "content"],
        "get_artifact" => &["hash"],
        "reuse_artifact" => &["name", "inputs"],
        "add_knowledge" => &["kind", "content", "provenance"],
        "link_knowledge" => &["source", "target", "relation"],
        "answer_question" => &["question", "answer"],
        _ => &[],
    };
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
pub fn worker_allowed(name: &str) -> bool {
    [
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
        if args["verified"] == true {
            bail!("workers cannot certify artifacts or knowledge");
        }
    }
    if let Some(result) = crate::management::dispatch(db, name, &args)? {
        return Ok(result);
    }
    if name == "shutdown" {
        std::fs::write(db.root.join("shutdown.request"), b"shutdown")?;
        return Ok(json!({"stopping":true}));
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
        for s in &plan.steps {
            if s.kind == "agent" && !settings.executors.contains_key(&s.role) {
                bail!("unconfigured executor role {}", s.role);
            }
        }
        return db.atomic(|| {
            let oid = db.submit(objective, &repo, &settings, &plan)?;
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
        "integrate_child" => crate::federation::integrate_child(db,oid,&args),
        "environments" => Ok(json!(db.rows("SELECT id,task,attempt,kind,state,pid,created,expires,evidence FROM app_environments WHERE task=?",&[&oid])?)),
        "refresh_bundles" => db.atomic(|| {let names:Vec<String>=db.rows("SELECT name FROM task_bundles WHERE task=?",&[&oid])?.iter().filter_map(|r|r["name"].as_str().map(str::to_owned)).collect();db.conn.execute("DELETE FROM task_bundles WHERE task=?",[oid])?;crate::secrets::select(db,oid,&names)?;Ok(json!({"refreshed":true}))}),
        "metrics" => crate::metrics::report(db, oid),
        "inspect" => Ok(
            json!({"task":db.task(oid)?,"outputs":db.rows("SELECT outputs FROM workflow_outputs WHERE task=?",&[&oid])?,"steps":db.steps(oid)?,"workers":db.rows("SELECT id,step,status,workspace,branch,base FROM workers WHERE task=?",&[&oid])?,"attempts":db.rows("SELECT a.* FROM attempts a JOIN steps t ON a.step=t.id WHERE t.task=? ORDER BY a.started",&[&oid])?,"questions":db.rows("SELECT * FROM questions WHERE task=?",&[&oid])?,"claims":db.rows("SELECT * FROM claims WHERE task=?",&[&oid])?,"integrations":db.rows("SELECT * FROM integrations WHERE task=?",&[&oid])?,"external_ops":db.rows("SELECT * FROM external_ops WHERE task=?",&[&oid])?}),
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
            json!({"tools":OPERATIONS.iter().map(|(n,d)|json!({"name":n,"description":d,"inputSchema":schema(n)})).collect::<Vec<_>>()}),
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
