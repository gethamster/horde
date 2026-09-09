//! Bounded task trees and versioned, source-preserving context.
use crate::{
    config::Settings,
    store::{Store, id},
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub workers: usize,
    pub children: usize,
    pub depth: usize,
    pub environments: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            workers: 4,
            children: 16,
            depth: 3,
            environments: 2,
        }
    }
}
impl Limits {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=64).contains(&self.workers)
                && self.children <= 256
                && self.depth <= 8
                && (1..=16).contains(&self.environments),
            "invalid delegation limits"
        );
        Ok(())
    }
}

pub fn migrate(c: &rusqlite::Connection) -> Result<()> {
    c.execute_batch("BEGIN IMMEDIATE;
CREATE TABLE IF NOT EXISTS task_tree(task TEXT PRIMARY KEY REFERENCES tasks(id),root TEXT NOT NULL,parent TEXT,caller_worker TEXT,depth INTEGER NOT NULL,request_id TEXT,request_hash TEXT,version INTEGER NOT NULL DEFAULT 1,limits TEXT NOT NULL,UNIQUE(parent,request_id));
CREATE TABLE IF NOT EXISTS context_records(id TEXT PRIMARY KEY,root TEXT NOT NULL,version INTEGER NOT NULL,kind TEXT NOT NULL,content TEXT NOT NULL,provenance TEXT NOT NULL,mandatory INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS context_attempts(attempt TEXT PRIMARY KEY,root TEXT NOT NULL,version INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS question_routes(question TEXT PRIMARY KEY REFERENCES questions(id),origin TEXT NOT NULL,target TEXT,request_id TEXT NOT NULL,envelope TEXT NOT NULL,answerer TEXT,human_only INTEGER NOT NULL DEFAULT 0,UNIQUE(origin,request_id));
CREATE TABLE IF NOT EXISTS event_receipts(task TEXT NOT NULL,consumer TEXT NOT NULL,seq INTEGER NOT NULL,PRIMARY KEY(task,consumer));
CREATE TABLE IF NOT EXISTS task_bundles(task TEXT NOT NULL,name TEXT NOT NULL,version TEXT NOT NULL,PRIMARY KEY(task,name));
CREATE TABLE IF NOT EXISTS remote_environment_leases(task TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS app_process_groups(environment TEXT NOT NULL,pid INTEGER NOT NULL,identity TEXT NOT NULL,PRIMARY KEY(environment,pid));
CREATE TABLE IF NOT EXISTS app_process_identity(environment TEXT PRIMARY KEY,identity TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS app_environments(id TEXT PRIMARY KEY,task TEXT NOT NULL,attempt TEXT,kind TEXT NOT NULL,state TEXT NOT NULL,spec TEXT NOT NULL,workspace TEXT NOT NULL,pid INTEGER,created INTEGER NOT NULL,expires INTEGER NOT NULL,evidence TEXT);
CREATE TABLE IF NOT EXISTS foreign_children(parent TEXT NOT NULL,remote_child TEXT NOT NULL,local_child TEXT NOT NULL,PRIMARY KEY(parent,remote_child));
CREATE TABLE IF NOT EXISTS local_child_bases(task TEXT PRIMARY KEY,base TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS question_escalations(question TEXT NOT NULL,caller TEXT NOT NULL,commentary TEXT NOT NULL,PRIMARY KEY(question,caller));
CREATE TABLE IF NOT EXISTS remote_context(task TEXT PRIMARY KEY,packet TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS child_acceptance(parent TEXT NOT NULL,child TEXT NOT NULL,version INTEGER NOT NULL,evidence TEXT NOT NULL,PRIMARY KEY(parent,child));
CREATE TABLE IF NOT EXISTS remote_links(task TEXT PRIMARY KEY,peer TEXT NOT NULL,remote_id TEXT,state TEXT NOT NULL,request TEXT NOT NULL,base TEXT);
CREATE TABLE IF NOT EXISTS remote_origins(task TEXT PRIMARY KEY,owner_peer TEXT NOT NULL,owner_task TEXT NOT NULL,UNIQUE(owner_peer,owner_task));
COMMIT;")?;
    // Upgrade existing tasks without rewriting their original objective.
    for row in {
        let mut q = c.prepare(
            "SELECT id,objective,settings FROM tasks WHERE id NOT IN (SELECT task FROM task_tree)",
        )?;
        q.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    } {
        let settings: Settings = serde_json::from_str(&row.2)?;
        c.execute(
            "INSERT OR IGNORE INTO task_tree(task,root,depth,limits) VALUES(?,?,0,?)",
            params![row.0, row.0, serde_json::to_string(&settings.limits)?],
        )?;
        c.execute("INSERT OR IGNORE INTO context_records VALUES(?,?,1,'objective',?,'original submission',1)",params![format!("objective:{}",row.0),row.0,row.1])?;
    }
    Ok(())
}
pub fn initialize(db: &Store, oid: &str, objective: &str, settings: &Settings) -> Result<()> {
    settings.limits.validate()?;
    db.conn.execute(
        "INSERT INTO task_tree(task,root,depth,limits) VALUES(?,?,0,?)",
        params![oid, oid, serde_json::to_string(&settings.limits)?],
    )?;
    db.conn.execute(
        "INSERT INTO context_records VALUES(?,?,1,'objective',?,'original submission',1)",
        params![format!("objective:{oid}"), oid, objective],
    )?;
    crate::secrets::select(db, oid, &settings.secret_bundles)?;
    Ok(())
}
pub fn tree(db: &Store, oid: &str) -> Result<Value> {
    db.rows("SELECT * FROM task_tree WHERE task=?", &[&oid])?
        .into_iter()
        .next()
        .context("task tree missing")
}
pub fn root(db: &Store, oid: &str) -> Result<String> {
    Ok(tree(db, oid)?["root"].as_str().context("root")?.into())
}
pub fn contract(db: &Store, oid: &str, after: i64, limit: i64) -> Result<Value> {
    let t = tree(db, oid)?;
    let r = t["root"].as_str().context("root")?;
    let entries=db.rows("SELECT rowid AS cursor,* FROM context_records WHERE root=? AND rowid>? ORDER BY rowid LIMIT ?",&[&r,&after,&limit.clamp(1,100)])?;
    Ok(
        json!({"root":r,"parent":t["parent"],"version":t["version"],"records":entries,"next":entries.last().map(|r|&r["cursor"])}),
    )
}
pub fn mandatory(db: &Store, oid: &str) -> Result<Value> {
    let cached = db.rows("SELECT packet FROM remote_context WHERE task=?", &[&oid])?;
    if let Some(c) = cached.first() {
        let packet: Value = serde_json::from_str(c["packet"].as_str().context("remote context")?)?;
        return Ok(packet["context"].clone());
    }
    let t = tree(db, oid)?;
    let r = t["root"].as_str().context("root")?;
    let records=db.rows("SELECT id,version,kind,content,provenance FROM context_records WHERE root=? AND mandatory=1 ORDER BY version,id",&[&r])?;
    let bytes = serde_json::to_vec(&records)?.len();
    ensure!(
        bytes <= 256 * 1024,
        "mandatory context exceeds 256 KiB; caller must consolidate authoritative context before dispatch"
    );
    Ok(
        json!({"root":r,"parent":t["parent"],"version":t["version"],"records":records,"sources_tool":"read_context","supporting_sources":db.rows("SELECT id,kind,provenance FROM context_records WHERE root=? AND mandatory=0 ORDER BY rowid LIMIT 100",&[&r])?}),
    )
}
pub fn update_context(db: &Store, oid: &str, args: &Value) -> Result<Value> {
    ensure!(
        root(db, oid)? == oid,
        "only the root caller may revise inherited context"
    );
    let content = args["content"].as_str().context("content")?;
    let provenance = args["provenance"].as_str().context("provenance")?;
    ensure!(
        !content.trim().is_empty() && !provenance.trim().is_empty() && content.len() <= 65536,
        "context requires content and provenance, at most 64 KiB"
    );
    db.atomic(|| {
  let version=tree(db,oid)?["version"].as_i64().context("version")?+1;
  let cid=args["id"].as_str().map(str::to_owned).unwrap_or_else(id);
  if let Some(ids)=args["supersedes"].as_array() {for old in ids {
    let changed=db.conn.execute("UPDATE context_records SET mandatory=0 WHERE id=? AND root=? AND kind!='objective'",params![old.as_str().context("source ID")?,oid])?;
    ensure!(changed==1,"superseded source missing or original objective is protected");
  }}
  db.conn.execute("INSERT INTO context_records VALUES(?,?,?,?,?,?,?)",params![cid,oid,version,args["kind"].as_str().unwrap_or("requirement"),content,provenance,args["mandatory"].as_bool().unwrap_or(true)])?;
  db.conn.execute("UPDATE task_tree SET version=? WHERE root=?",params![version,oid])?;
  // Completed evidence becomes stale; require explicit verification rather than replaying side effects.
  db.conn.execute("UPDATE tasks SET status='blocked' WHERE id IN (SELECT task FROM task_tree WHERE root=?) AND status='succeeded'",[oid])?;
  db.event(oid,"context.updated",json!({"id":cid,"version":version,"requires_revalidation":true}))?;
  Ok(json!({"id":cid,"version":version}))
 })
}
pub fn pin(db: &Store, oid: &str, attempt: &str) -> Result<()> {
    let t = tree(db, oid)?;
    db.conn.execute(
        "INSERT INTO context_attempts VALUES(?,?,?)",
        params![attempt, t["root"].as_str(), t["version"].as_i64()],
    )?;
    Ok(())
}
pub fn check_pin(db: &Store, oid: &str, attempt: &str) -> Result<()> {
    let v: Option<i64> = db
        .conn
        .query_row(
            "SELECT version FROM context_attempts WHERE attempt=?",
            [attempt],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(v) = v {
        ensure!(
            Some(v) == tree(db, oid)?["version"].as_i64(),
            "context changed during execution; result requires revalidation"
        );
    }
    Ok(())
}
pub fn capacity(db: &Store, oid: &str) -> Result<bool> {
    let t = tree(db, oid)?;
    let r = t["root"].as_str().context("root")?;
    let limits: Limits = serde_json::from_str(t["limits"].as_str().context("limits")?)?;
    let active:i64=db.conn.query_row("SELECT COUNT(*) FROM attempts a JOIN steps t ON t.id=a.step JOIN task_tree ot ON ot.task=t.task WHERE ot.root=? AND a.state IN ('running','uncertain')",[r],|r|r.get(0))?;
    let reserved:i64=db.conn.query_row("SELECT COUNT(*) FROM remote_links l JOIN task_tree t ON t.task=l.task WHERE t.root=? AND l.state IN ('sending','running')",[r],|r|r.get(0))?;
    Ok(active + reserved < limits.workers as i64)
}
pub fn delegate(db: &Store, oid: &str, args: &Value) -> Result<Value> {
    let request = args["id"]
        .as_str()
        .context("delegation requires id for deduplication")?;
    let hash = crate::store::hash(serde_json::to_string(args)?.as_bytes());
    let old = db.rows(
        "SELECT task,request_hash FROM task_tree WHERE parent=? AND request_id=?",
        &[&oid, &request],
    )?;
    if let Some(old) = old.first() {
        ensure!(
            old["request_hash"] == hash,
            "delegation ID reused with different assignment"
        );
        return Ok(json!({"id":old["task"],"duplicate":true}));
    }
    let normalized = delegation_target(db, args)?;
    let args = &normalized;
    let parent = db.task(oid)?;
    let source = if let Some(worker) = args["worker"].as_str() {
        db.worker(worker)?["workspace"]
            .as_str()
            .map(std::path::PathBuf::from)
            .unwrap_or(crate::git::task_workspace(db, oid)?)
    } else {
        crate::git::task_workspace(db, oid)?
    };
    ensure!(
        crate::git::run(&source, &["status", "--porcelain"])?.is_empty(),
        "commit parent work before delegating"
    );
    let t = tree(db, oid)?;
    let r = t["root"].as_str().context("root")?;
    let limits: Limits = serde_json::from_str(t["limits"].as_str().context("limits")?)?;
    let objective = args["objective"].as_str().context("objective")?;
    let mut settings: Settings =
        serde_json::from_str(parent["settings"].as_str().context("settings")?)?;
    let repo = std::path::Path::new(parent["repo"].as_str().context("repo")?);
    let templates = crate::template::load_templates(&crate::branding::templates(repo))?;
    let mut plan = crate::template::compile(
        args["template"]
            .as_str()
            .unwrap_or(&settings.default_template),
        &templates,
        std::collections::BTreeMap::from([("task".into(), objective.into())]),
    )?;
    let skills = crate::skills::select(db, oid, args.get("skills"))?;
    if args["skills"].is_array() {
        for step in &mut plan.steps {
            if step.kind == "agent" {
                step.skills.extend(skills.keys().cloned());
                step.skills.sort();
                step.skills.dedup();
            }
        }
    }
    db.atomic(|| {
  let count:i64=db.conn.query_row("SELECT COUNT(*)-1 FROM task_tree WHERE root=?",[r],|r|r.get(0))?;
  ensure!(count<limits.children as i64 && t["depth"].as_u64().unwrap_or(0)<limits.depth as u64,"delegation tree limit reached");
  settings.secret_bundles.clear();
  settings.skills.clear();
  let child=db.submit_pinned(objective,repo,&settings,&plan,&skills)?;
  crate::execution_selection::inherit(db,oid,&child,args)?;
  crate::execution_selection::validate_target(db,&child,args["peer"].as_str())?;
  if args["peer"].is_null(){
   let target=db.root.join("delegated-repositories").join(&child);std::fs::create_dir_all(target.parent().context("repository directory")?)?;
   let base=if let Some(snapshot)=args.get("_snapshot"){
    crate::federation::unpack(snapshot,&target)?;snapshot["commit"].as_str().context("caller source commit")?.to_owned()
   }else{
    let base=crate::git::run(&source,&["rev-parse","HEAD"])?;
    let output=crate::executor::clean_command("git").args(["clone","--no-hardlinks"]).arg(&source).arg(&target).output()?;ensure!(output.status.success(),"cannot allocate child repository");crate::git::run(&target,&["checkout","--detach",&base])?;base
   };
   crate::git::run(&target,&["config","user.name","Horde"])?;crate::git::run(&target,&["config","user.email","task@localhost"])?;
   db.conn.execute("UPDATE tasks SET repo=? WHERE id=?",params![target.to_string_lossy(),child])?;
   db.conn.execute("INSERT INTO local_child_bases VALUES(?,?)",params![child,base])?;
  }
  db.conn.execute("DELETE FROM context_records WHERE root=?",[&child])?;
  db.conn.execute("UPDATE task_tree SET root=?,parent=?,caller_worker=?,depth=?,request_id=?,request_hash=?,version=?,limits=? WHERE task=?",params![r,oid,args["worker"].as_str(),t["depth"].as_i64().unwrap_or(0)+1,request,hash,t["version"].as_i64(),t["limits"].as_str(),child])?;
  crate::secrets::inherit(db,oid,&child,args.get("bundles"))?;
  if let Some(snapshot)=args.get("_snapshot"){db.artifact(&child,None,"caller-snapshot",&serde_json::to_vec(snapshot)?,&json!({}),false)?;}
  if let Some(peer)=args["peer"].as_str(){
   if args.get("_snapshot").is_none(){let snapshot=crate::federation::snapshot(&source)?;db.artifact(&child,None,"caller-snapshot",&serde_json::to_vec(&snapshot)?,&json!({}),false)?;}
   let config=crate::federation::config(db)?;
   ensure!(config.delegate_peers.iter().any(|p|p==peer),"remote runtime is not approved for delegation");
   db.conn.execute("INSERT INTO remote_links(task,peer,state,request) VALUES(?,?,'pending',?)",params![child,peer,request])?;
   db.conn.execute("UPDATE tasks SET status='remote' WHERE id=?",[&child])?;
  }
  db.conn.execute("UPDATE tasks SET status='running' WHERE id=? AND status='succeeded'",[oid])?;
  db.event(oid,"child.submitted",json!({"child":child,"id":request,"assignment":objective}))?;
  Ok(json!({"id":child,"root":r,"parent":oid}))
 })
}

fn delegation_target(db: &Store, args: &Value) -> Result<Value> {
    let mut normalized = args.clone();
    let inventory = if args["execution"].is_object() {
        Some(crate::capabilities::inventory(db)?)
    } else {
        None
    };
    if let Some(inventory) = inventory {
        let selected = args["execution"]["selected"]["runtime"]
            .as_str()
            .context("execution requires a selected runtime")?;
        let runtimes = inventory["runtimes"]
            .as_array()
            .context("runtime inventory")?;
        let local = runtimes
            .iter()
            .find(|runtime| runtime["local"] == true)
            .context("local runtime")?;
        let target =
            if selected == "local" || local["runtime"] == selected || local["name"] == selected {
                None
            } else {
                Some(crate::runtime_directory::resolve(db, selected)?)
            };
        if args["peer"].is_null() {
            normalized["peer"] = serde_json::to_value(target)?;
        }
    }
    if let Some(peer) = normalized["peer"].as_str() {
        if peer == "local" {
            normalized
                .as_object_mut()
                .context("arguments")?
                .remove("peer");
        } else if !crate::federation::config(db)?
            .delegate_peers
            .iter()
            .any(|id| id == peer)
        {
            normalized["peer"] = json!(crate::runtime_directory::resolve(db, peer)?);
        }
    }
    Ok(normalized)
}
pub fn ask(db: &Store, oid: &str, args: &Value) -> Result<Value> {
    let q = args["question"].as_str().context("question")?;
    ensure!(!q.trim().is_empty(), "question cannot be empty");
    let request = args["id"].as_str().map(str::to_owned).unwrap_or_else(id);
    let envelope = json!({"question":q,"worker":args["worker"],"evidence":args["evidence"],"recommendation":args["recommendation"],"human_only":args["human_only"].as_bool().unwrap_or(false)});
    db.atomic(|| {
  let old=db.rows("SELECT question,envelope FROM question_routes WHERE origin=? AND request_id=?",&[&oid,&request])?;
  if let Some(old)=old.first(){ensure!(serde_json::from_str::<Value>(old["envelope"].as_str().context("question envelope")?)?==envelope,"question ID reused with different content");return Ok(json!({"id":old["question"],"duplicate":true}));}
  let qid=id();let t=tree(db,oid)?;
  db.conn.execute("INSERT INTO questions VALUES(?,?,?,NULL)",params![qid,oid,q])?;
  db.conn.execute("INSERT INTO question_context VALUES(?,'input',?)",params![qid,args["worker"].as_str()])?;
  db.conn.execute("INSERT INTO question_routes(question,origin,target,request_id,envelope,human_only) VALUES(?,?,?,?,?,?)",params![qid,oid,t["parent"].as_str(),request,envelope.to_string(),envelope["human_only"].as_bool()])?;
  if args["worker"].is_null(){db.conn.execute("UPDATE tasks SET status='waiting' WHERE id=? AND status='running'",[oid])?;}
  if let Some(w)=t["caller_worker"].as_str(){db.conn.execute("UPDATE workers SET status='notified' WHERE id=? AND status='idle'",[w])?;}
  db.event(t["parent"].as_str().unwrap_or(oid),"question.pending",json!({"id":qid,"origin":oid,"envelope":envelope}))?;
  Ok(json!({"id":qid,"waiting":true,"instruction":"Finish this invocation with accepted=false. Only dependent work waits; the caller may answer or escalate."}))
 })
}
pub fn question_action(db: &Store, caller: &str, args: &Value, escalate: bool) -> Result<Value> {
    let qid = args["question"].as_str().context("question ID")?;
    let route=db.rows("SELECT r.*,q.answer FROM question_routes r JOIN questions q ON q.id=r.question WHERE r.question=?",&[&qid])?.into_iter().next().context("routed question missing")?;
    let origin = route["origin"].as_str().context("origin")?;
    let target = route["target"].as_str();
    if escalate {
        let old = db.rows(
            "SELECT commentary FROM question_escalations WHERE question=? AND caller=?",
            &[&qid, &caller],
        )?;
        if let Some(old) = old.first() {
            ensure!(
                old["commentary"] == args["commentary"].as_str().unwrap_or(""),
                "escalation retry changed commentary"
            );
            return Ok(json!({"escalated":true,"duplicate":true}));
        }
    }
    ensure!(
        target == Some(caller) || target.is_none() && caller == root(db, origin)?,
        "question is addressed to another caller"
    );
    if let Some(worker) = args["worker"].as_str() {
        ensure!(
            target.is_some(),
            "external caller must answer this question"
        );
        let mut branch = tree(db, origin)?;
        while branch["parent"] != caller {
            branch = tree(
                db,
                branch["parent"]
                    .as_str()
                    .context("question caller ancestry")?,
            )?;
        }
        ensure!(
            branch["caller_worker"] == worker,
            "question belongs to another calling worker"
        );
        ensure!(
            escalate || route["human_only"] != 1,
            "question requires an external human answer"
        );
    }
    db.atomic(|| {
  if escalate {
   ensure!(route["answer"].is_null(),"question already answered");
   let t=tree(db,caller)?;
   db.conn.execute("INSERT INTO question_escalations VALUES(?,?,?)",params![qid,caller,args["commentary"].as_str().unwrap_or("")])?;
   db.conn.execute("UPDATE question_routes SET target=? WHERE question=?",params![t["parent"].as_str(),qid])?;
   db.event(t["parent"].as_str().unwrap_or(caller),"question.escalated",json!({"id":qid,"origin":origin,"original":route["envelope"],"commentary":args["commentary"]}))?;
   return Ok(json!({"escalated":true,"target":t["parent"]}));
  }
  let answer=args["answer"].as_str().context("answer")?;
  if !route["answer"].is_null(){ensure!(route["answer"]==answer,"question already has a different answer");return Ok(json!({"answered":true,"duplicate":true}));}
  if route["human_only"]==1 {ensure!(args["human"]==true && args["worker"].is_null(),"human-only question requires caller attestation");}
  let author=args["worker"].as_str().unwrap_or("external caller");
  db.conn.execute("UPDATE questions SET answer=? WHERE id=?",params![answer,qid])?;
  db.conn.execute("UPDATE question_routes SET answerer=? WHERE question=?",params![author,qid])?;
  db.conn.execute("UPDATE question_context SET purpose='input_answered' WHERE question=?",[qid])?;
  db.conn.execute("UPDATE steps SET state='pending' WHERE state IN ('waiting','failed') AND id IN (SELECT step FROM workers WHERE id IN (SELECT worker FROM question_context WHERE question=?))",[qid])?;
  db.conn.execute("UPDATE tasks SET status='running' WHERE id=? AND status='waiting' AND NOT EXISTS(SELECT 1 FROM questions WHERE task=? AND answer IS NULL)",params![origin,origin])?;
  let r=root(db,origin)?;
  let version=tree(db,&r)?["version"].as_i64().unwrap_or(1)+1;
  db.conn.execute("UPDATE task_tree SET version=? WHERE root=?",params![version,r])?;
  db.conn.execute("UPDATE tasks SET status='blocked' WHERE id IN (SELECT task FROM task_tree WHERE root=?) AND status='succeeded'",[&r])?;
  db.conn.execute("INSERT INTO context_records VALUES(?,?,?,'answer',?,?,1)",params![format!("answer:{qid}"),r,version,json!({"question":route["envelope"],"answer":answer}).to_string(),json!({"question":qid,"author":author,"caller":caller}).to_string()])?;
  db.event(&r,"question.answered",json!({"id":qid,"origin":origin,"answer":answer,"author":author,"version":version}))?;
  Ok(json!({"answered":true,"version":version}))
 })
}
pub fn has_question(db: &Store, worker: &str) -> Result<bool> {
    let n:i64=db.conn.query_row("SELECT COUNT(*) FROM questions q JOIN question_context c ON c.question=q.id WHERE c.worker=? AND q.answer IS NULL",[worker],|r|r.get(0))?;
    Ok(n > 0)
}
pub fn dispatch(db: &Store, oid: &str, name: &str, args: &Value) -> Result<Value> {
    match name {
  "delegate_task"=>delegate(db,oid,args),
  "read_context"=>contract(db,oid,args["after"].as_i64().unwrap_or(0),args["limit"].as_i64().unwrap_or(50)),
  "update_context"=>update_context(db,oid,args),
  "list_children"=>Ok(json!(db.rows("SELECT ot.*,o.status,o.objective,(SELECT COALESCE(MAX(revision),0) FROM revisions WHERE task=o.id) AS revision FROM task_tree ot JOIN tasks o ON o.id=ot.task WHERE parent=?",&[&oid])?)),
  "pending_questions"=>Ok(json!(db.rows("SELECT r.*,q.question AS body,q.answer FROM question_routes r JOIN questions q ON q.id=r.question WHERE q.answer IS NULL AND (r.target=? OR (r.target IS NULL AND r.origin IN (SELECT task FROM task_tree WHERE root=?)))",&[&oid,&oid])?)),
  "escalate_question"=>question_action(db,oid,args,true),
  "ack_events"=>{let consumer=args["consumer"].as_str().context("consumer")?;let seq=args["seq"].as_i64().context("seq")?;let max:i64=db.conn.query_row("SELECT COALESCE(MAX(seq),0) FROM events WHERE task=?",[oid],|r|r.get(0))?;ensure!((0..=max).contains(&seq),"event cursor out of range");db.conn.execute("INSERT INTO event_receipts VALUES(?,?,?) ON CONFLICT(task,consumer) DO UPDATE SET seq=MAX(seq,excluded.seq)",params![oid,consumer,seq])?;Ok(json!({"acknowledged":true}))},
  _=>bail!("unknown delegation operation")
 }
}
/// Revisions invalidate acceptance of this result and every containing result.
/// Call inside the transaction that records the revision.
pub fn invalidate_acceptance(db: &Store, oid: &str) -> Result<()> {
    db.conn.execute(
        "WITH RECURSIVE ancestors(id) AS (SELECT ? UNION ALL SELECT t.parent FROM task_tree t JOIN ancestors a ON t.task=a.id WHERE t.parent IS NOT NULL) DELETE FROM child_acceptance WHERE child IN (SELECT id FROM ancestors)",
        [oid],
    )?;
    db.conn.execute(
        "WITH RECURSIVE ancestors(id) AS (SELECT ? UNION ALL SELECT t.parent FROM task_tree t JOIN ancestors a ON t.task=a.id WHERE t.parent IS NOT NULL) UPDATE tasks SET status='running' WHERE id IN (SELECT id FROM ancestors) AND status='succeeded'",
        [oid],
    )?;
    Ok(())
}
pub fn child_completion(db: &Store, oid: &str) -> Result<bool> {
    let children = db.rows(
        "SELECT o.status FROM tasks o JOIN task_tree t ON t.task=o.id WHERE t.parent=?",
        &[&oid],
    )?;
    let accepted:i64=db.conn.query_row("SELECT COUNT(*) FROM child_acceptance a JOIN task_tree t ON t.task=a.parent WHERE a.parent=? AND a.version=t.version",[oid],|r|r.get(0))?;
    Ok(children.iter().all(|c| c["status"] == "succeeded") && accepted == children.len() as i64)
}
