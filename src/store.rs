use crate::{
    config::Settings,
    template::{Plan, Step},
};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const SCHEMA_VERSION: u32 = 4;
/// Status of the per-task synthetic worker row that carries operator steering messages.
/// Operator rows never receive mail, never wake, and are hidden from worker listings.
pub const OPERATOR_STATUS: &str = "operator";
/// Identity of the operator sender for a task; messages from it are operator steering.
pub fn operator_id(oid: &str) -> String {
    format!("operator:{oid}")
}

#[path = "legacy_store.rs"]
mod legacy_store;

pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
pub fn scope(path: &str) -> Result<String> {
    if path.is_empty() {
        bail!("empty path");
    }
    let p = Path::new(path);
    let mut parts = vec![];
    for c in p.components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str().context("non UTF-8 path")?),
            Component::CurDir => {}
            _ => bail!("path must be relative without parent traversal"),
        }
    }
    if parts.first().is_some_and(|s| *s == ".git") {
        bail!(".git is reserved");
    }
    Ok(if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    })
}
pub fn overlaps(a: &str, b: &str) -> bool {
    a == "."
        || b == "."
        || a == b
        || a.starts_with(&format!("{b}/"))
        || b.starts_with(&format!("{a}/"))
}
pub fn contains(prefix: &str, path: &str) -> bool {
    prefix == "." || path == prefix || path.starts_with(&format!("{prefix}/"))
}
pub struct Store {
    pub conn: Connection,
    pub root: PathBuf,
}
impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
        }
        let conn = Connection::open(root.join("state.sqlite3"))?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > i64::from(SCHEMA_VERSION) {
            bail!("database schema {version} is newer than this runtime supports");
        }
        legacy_store::migrate(&conn, root)?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY, objective TEXT NOT NULL, repo TEXT NOT NULL, status TEXT NOT NULL, settings TEXT NOT NULL, plan TEXT NOT NULL, created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS revisions(task TEXT NOT NULL REFERENCES tasks(id), revision INTEGER NOT NULL, plan TEXT NOT NULL, created INTEGER NOT NULL, PRIMARY KEY(task,revision));
CREATE TABLE IF NOT EXISTS steps(id TEXT PRIMARY KEY, task TEXT NOT NULL REFERENCES tasks(id), name TEXT NOT NULL, spec TEXT NOT NULL, state TEXT NOT NULL, result TEXT, UNIQUE(task,name));
CREATE TABLE IF NOT EXISTS attempts(id TEXT PRIMARY KEY, step TEXT NOT NULL REFERENCES steps(id), worker TEXT, state TEXT NOT NULL, started INTEGER NOT NULL, finished INTEGER, pid INTEGER, result TEXT, usage TEXT);
CREATE TABLE IF NOT EXISTS workers(id TEXT PRIMARY KEY, task TEXT NOT NULL REFERENCES tasks(id), step TEXT REFERENCES steps(id), status TEXT NOT NULL, token_hash TEXT NOT NULL, workspace TEXT, branch TEXT, base TEXT, updated INTEGER NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS workspace_owner ON workers(workspace) WHERE workspace IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS branch_owner ON workers(task,branch) WHERE branch IS NOT NULL;
CREATE TABLE IF NOT EXISTS channels(task TEXT NOT NULL REFERENCES tasks(id), name TEXT NOT NULL, worker TEXT NOT NULL REFERENCES workers(id), PRIMARY KEY(task,name,worker));
CREATE TABLE IF NOT EXISTS messages(seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL, task TEXT NOT NULL REFERENCES tasks(id), sender TEXT NOT NULL REFERENCES workers(id), destination TEXT NOT NULL, body TEXT NOT NULL, refs TEXT NOT NULL, actionable INTEGER NOT NULL, created INTEGER NOT NULL, UNIQUE(sender,id));
CREATE TABLE IF NOT EXISTS receipts(message TEXT NOT NULL REFERENCES messages(id), worker TEXT NOT NULL REFERENCES workers(id), ack INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(message,worker));
CREATE TABLE IF NOT EXISTS cursors(worker TEXT PRIMARY KEY REFERENCES workers(id), seq INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS notifications(worker TEXT PRIMARY KEY REFERENCES workers(id), dispatched_seq INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS claims(task TEXT NOT NULL REFERENCES tasks(id), path TEXT NOT NULL, worker TEXT NOT NULL REFERENCES workers(id), PRIMARY KEY(task,path));
CREATE TABLE IF NOT EXISTS events(seq INTEGER PRIMARY KEY AUTOINCREMENT, task TEXT REFERENCES tasks(id), kind TEXT NOT NULL, data TEXT NOT NULL, created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS artifacts(hash TEXT PRIMARY KEY, size INTEGER NOT NULL, created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS artifact_links(task TEXT NOT NULL REFERENCES tasks(id), step TEXT REFERENCES steps(id), name TEXT NOT NULL, hash TEXT NOT NULL REFERENCES artifacts(hash), inputs TEXT NOT NULL, verified INTEGER NOT NULL, UNIQUE(task,step,name,hash));
CREATE TABLE IF NOT EXISTS knowledge(id TEXT PRIMARY KEY, task TEXT NOT NULL REFERENCES tasks(id), step TEXT REFERENCES steps(id), kind TEXT NOT NULL, content TEXT NOT NULL, provenance TEXT NOT NULL, verified INTEGER NOT NULL, inputs TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS knowledge_edges(source TEXT NOT NULL REFERENCES knowledge(id), target TEXT NOT NULL REFERENCES knowledge(id), relation TEXT NOT NULL, PRIMARY KEY(source,target,relation));
CREATE TABLE IF NOT EXISTS questions(id TEXT PRIMARY KEY, task TEXT NOT NULL REFERENCES tasks(id), question TEXT NOT NULL, answer TEXT);
CREATE TABLE IF NOT EXISTS question_context(question TEXT PRIMARY KEY REFERENCES questions(id), purpose TEXT NOT NULL, worker TEXT REFERENCES workers(id));
CREATE TABLE IF NOT EXISTS integrations(id TEXT PRIMARY KEY, task TEXT NOT NULL REFERENCES tasks(id), worker TEXT NOT NULL REFERENCES workers(id), commit_id TEXT NOT NULL, state TEXT NOT NULL, evidence TEXT, created INTEGER NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS integration_once ON integrations(task,worker,commit_id);
CREATE TABLE IF NOT EXISTS workflow_outputs(task TEXT PRIMARY KEY REFERENCES tasks(id), outputs TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS external_ops(task TEXT NOT NULL REFERENCES tasks(id), name TEXT NOT NULL, state TEXT NOT NULL, data TEXT NOT NULL, PRIMARY KEY(task,name));
INSERT OR IGNORE INTO notifications(worker) SELECT id FROM workers;
")?;
        crate::delegation::migrate(&conn)?;
        crate::management::migrate(&conn)?;
        crate::skills::migrate(&conn)?;
        if version < 4 {
            crate::knowledge::migrate(&conn)?;
        }
        if version != i64::from(SCHEMA_VERSION) {
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self {
            conn,
            root: root.to_owned(),
        })
    }
    pub fn atomic<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        if !self.conn.is_autocommit() {
            let savepoint = format!("nested_{}", id().replace('-', ""));
            self.conn.execute_batch(&format!("SAVEPOINT {savepoint}"))?;
            return match f() {
                Ok(v) => {
                    self.conn.execute_batch(&format!("RELEASE {savepoint}"))?;
                    Ok(v)
                }
                Err(e) => {
                    self.conn
                        .execute_batch(&format!("ROLLBACK TO {savepoint}; RELEASE {savepoint}"))?;
                    Err(e)
                }
            };
        }
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        match f() {
            Ok(v) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                self.conn.execute_batch("ROLLBACK")?;
                Err(e)
            }
        }
    }
    pub fn event(&self, task: &str, kind: &str, data: Value) -> Result<()> {
        self.conn.execute(
            "INSERT INTO events(task,kind,data,created) VALUES(?,?,?,?)",
            params![
                task,
                kind,
                crate::secrets::redact(self, task, &data).to_string(),
                now()
            ],
        )?;
        Ok(())
    }
    pub fn submit(
        &self,
        objective: &str,
        repo: &Path,
        settings: &Settings,
        plan: &Plan,
    ) -> Result<String> {
        let skills = crate::skills::capture(repo, &settings.skills)?;
        self.submit_pinned(objective, repo, settings, plan, &skills)
    }
    pub(crate) fn submit_pinned(
        &self,
        objective: &str,
        repo: &Path,
        settings: &Settings,
        plan: &Plan,
        skills: &crate::skills::Packet,
    ) -> Result<String> {
        crate::skills::validate(skills)?;
        crate::skills::validate_steps(skills, &plan.steps)?;
        if objective.trim().is_empty() {
            bail!("task must not be empty");
        }
        let oid = id();
        self.atomic(|| {
            self.conn.execute(
                "INSERT INTO tasks VALUES(?,?,?,?,?,?,?)",
                params![
                    oid,
                    objective,
                    repo.to_str().context("repository path")?,
                    if settings.autonomy {
                        "running"
                    } else {
                        "waiting"
                    },
                    serde_json::to_string(settings)?,
                    serde_json::to_string(plan)?,
                    now()
                ],
            )?;
            self.conn.execute(
                "INSERT INTO revisions VALUES(?,1,?,?)",
                params![oid, serde_json::to_string(plan)?, now()],
            )?;
            for step in &plan.steps {
                self.conn.execute(
                    "INSERT INTO steps(id,task,name,spec,state) VALUES(?,?,?,?, 'pending')",
                    params![id(), oid, step.id, serde_json::to_string(step)?],
                )?;
            }
            if !settings.autonomy {
                self.conn.execute(
                    "INSERT INTO questions VALUES(?,?,?,NULL)",
                    params![
                        id(),
                        oid,
                        "Start this task? Answer yes to authorize execution."
                    ],
                )?;
            }
            crate::delegation::initialize(self, &oid, objective, settings)?;
            crate::skills::bind(self, &oid, skills)?;
            self.event(&oid, "task.submitted", json!({"objective":objective}))?;
            Ok(())
        })?;
        Ok(oid)
    }
    pub fn rows(&self, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Value>> {
        let mut stmt = self.conn.prepare(sql)?;
        let names: Vec<String> = stmt.column_names().iter().map(|x| x.to_string()).collect();
        let rows = stmt.query_map(args, |r| {
            let mut m = serde_json::Map::new();
            for (i, n) in names.iter().enumerate() {
                let v = match r.get_ref(i)? {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(v) => json!(v),
                    rusqlite::types::ValueRef::Real(v) => json!(v),
                    rusqlite::types::ValueRef::Text(v) => {
                        Value::String(String::from_utf8_lossy(v).into())
                    }
                    rusqlite::types::ValueRef::Blob(_) => Value::Null,
                };
                m.insert(n.clone(), v);
            }
            Ok(Value::Object(m))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    pub fn task(&self, oid: &str) -> Result<Value> {
        self.rows("SELECT * FROM tasks WHERE id=?", &[&oid])?
            .into_iter()
            .next()
            .context("unknown task")
    }
    pub fn worker(&self, wid: &str) -> Result<Value> {
        self.rows(
            "SELECT id,task,step,status,workspace,branch,base,updated FROM workers WHERE id=?",
            &[&wid],
        )?
        .into_iter()
        .next()
        .context("unknown worker")
    }
    pub fn authenticate(&self, token: &str) -> Result<Value> {
        let wid: String = self
            .conn
            .query_row(
                "SELECT id FROM workers WHERE token_hash=?",
                [hash(token.as_bytes())],
                |r| r.get(0),
            )
            .optional()?
            .context("invalid worker token")?;
        self.worker(&wid)
    }
    pub fn register(&self, oid: &str, step: Option<&str>) -> Result<Value> {
        self.task(oid)?;
        if let Some(t) = step {
            let owner: String =
                self.conn
                    .query_row("SELECT task FROM steps WHERE id=?", [t], |r| r.get(0))?;
            if owner != oid {
                bail!("step belongs to another task");
            }
        }
        let wid = id();
        let token = format!("{}{}", id(), id());
        self.atomic(||{
            self.conn.execute("INSERT INTO workers(id,task,step,status,token_hash,updated) VALUES(?,?,?,'idle',?,?)",params![wid,oid,step,hash(token.as_bytes()),now()])?;
            self.conn.execute("INSERT INTO cursors(worker) VALUES(?)",[&wid])?;
            self.conn.execute("INSERT INTO notifications(worker) VALUES(?)",[&wid])?;
            self.event(oid,"worker.registered",json!({"worker":wid,"step":step}))?;
            Ok(json!({"id":wid,"token":token}))
        })
    }
    pub fn claim(&self, oid: &str, wid: &str, paths: &[String]) -> Result<()> {
        let w = self.worker(wid)?;
        if w["task"] != oid {
            bail!("wrong task");
        }
        if w["workspace"].is_null() {
            bail!("register a workspace before claiming writes");
        }
        let paths: Vec<_> = paths.iter().map(|p| scope(p)).collect::<Result<_>>()?;
        self.atomic(|| {
            let existing = self.rows("SELECT path,worker FROM claims WHERE task=?", &[&oid])?;
            for p in &paths {
                for c in &existing {
                    if c["worker"] != wid && overlaps(p, c["path"].as_str().unwrap_or(".")) {
                        bail!(
                            "claim conflict: {p} owned by {} ({})",
                            c["worker"],
                            c["path"]
                        );
                    }
                }
            }
            for p in paths {
                self.conn.execute(
                    "INSERT OR IGNORE INTO claims VALUES(?,?,?)",
                    params![oid, p, wid],
                )?;
            }
            self.event(oid, "claims.acquired", json!({"worker":wid}))?;
            Ok(())
        })
    }
    pub fn transfer(&self, oid: &str, from: &str, to: &str, path: &str) -> Result<()> {
        let p = scope(path)?;
        self.atomic(|| {
            let w = self.worker(to)?;
            if w["task"] != oid || w["workspace"].is_null() {
                bail!("recipient must have a registered workspace in this task");
            }
            let claims = self.rows("SELECT path,worker FROM claims WHERE task=?", &[&oid])?;
            if !claims.iter().any(|c| c["path"] == p && c["worker"] == from) {
                bail!("claim is not owned by sender");
            }
            for c in &claims {
                let other = c["path"].as_str().context("claim path")?;
                if overlaps(&p, other) && !contains(&p, other) && c["worker"] != to {
                    bail!("transfer the broader claim {other} instead");
                }
            }
            for c in &claims {
                let other = c["path"].as_str().context("claim path")?;
                if c["worker"] == from && contains(&p, other) {
                    self.conn.execute(
                        "UPDATE claims SET worker=? WHERE task=? AND path=?",
                        params![to, oid, other],
                    )?;
                }
            }
            self.event(
                oid,
                "claim.transferred",
                json!({"from":from,"to":to,"path":p,"includes_descendants":true}),
            )?;
            Ok(())
        })
    }
    pub fn check_write(&self, wid: &str, path: &str) -> Result<()> {
        let p = scope(path)?;
        let claims = self.rows("SELECT path FROM claims WHERE worker=?", &[&wid])?;
        if !claims
            .iter()
            .any(|c| contains(c["path"].as_str().unwrap_or(""), &p))
        {
            bail!("write outside declared scope: {p}");
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)] // Message envelope fields are kept explicit at the transactional boundary.
    pub fn send(
        &self,
        oid: &str,
        sender: &str,
        mid: &str,
        destination: &str,
        body: &str,
        refs: &Value,
        actionable: bool,
    ) -> Result<Value> {
        if body.len() > 262144 || mid.is_empty() {
            bail!("invalid message size or id");
        }
        self.atomic(||{
            let w=self.worker(sender)?;if w["task"]!=oid{bail!("wrong task");}
            if let Some(old)=self.rows("SELECT * FROM messages WHERE id=?",&[&mid])?.first(){
                if old["sender"]!=sender || old["body"]!=body || old["destination"]!=destination || serde_json::from_str::<Value>(old["refs"].as_str().context("message refs")?)?!=*refs || old["actionable"]!=json!(i64::from(actionable)){bail!("message id reused with different payload");}return Ok(json!({"id":mid,"duplicate":true}));
            }
            let operator=w["status"]==OPERATOR_STATUS;
            let recipients=self.recipients(oid,sender,destination,operator)?;
            let self_delivery=recipients.iter().any(|r|r["id"]==sender);
            self.conn.execute("INSERT INTO messages(id,task,sender,destination,body,refs,actionable,created) VALUES(?,?,?,?,?,?,?,?)",params![mid,oid,sender,destination,body,refs.to_string(),actionable,now()])?;
            for r in &recipients {let wid=r["id"].as_str().context("recipient")?;self.conn.execute("INSERT INTO receipts(message,worker) VALUES(?,?)",params![mid,wid])?;if actionable{self.conn.execute("UPDATE workers SET status='notified',updated=? WHERE id=? AND status='idle'",params![now(),wid])?;}}
            self.event(oid,"message.sent",json!({"id":mid,"sender":sender,"destination":destination,"recipients":recipients.len(),"operator":operator,"self_delivery":self_delivery}))?;
            Ok(json!({"id":mid,"recipients":recipients.len(),"duplicate":false}))
        })
    }
    /// Resolve the workers that receive a message.
    ///
    /// `task` broadcasts skip the sender while peers exist, deliver to the sender when it
    /// is the only worker (so a solo planner still hears itself), and reach every worker
    /// when the sender is the task's operator identity. Operator rows never receive mail.
    fn recipients(
        &self,
        oid: &str,
        sender: &str,
        destination: &str,
        operator: bool,
    ) -> Result<Vec<Value>> {
        if destination == "task" {
            let peers = self.rows(
                "SELECT id FROM workers WHERE task=? AND id<>? AND status<>? ORDER BY updated",
                &[&oid, &sender, &OPERATOR_STATUS],
            )?;
            return Ok(match (operator, peers.is_empty()) {
                (true, true) => bail!("task has no workers to steer"),
                (true, false) | (false, false) => peers,
                (false, true) => vec![json!({"id":sender})],
            });
        }
        if let Some(group) = destination.strip_prefix("group:") {
            let members = self.rows(
                "SELECT c.worker AS id FROM channels c JOIN workers w ON w.id=c.worker WHERE c.task=? AND c.name=? AND c.worker<>? AND w.status<>?",
                &[&oid, &group, &sender, &OPERATOR_STATUS],
            )?;
            if members.is_empty() {
                bail!("group has no other recipients");
            }
            return Ok(members);
        }
        let target = self.worker(destination)?;
        if target["task"] != oid {
            bail!("recipient belongs to another task");
        }
        if target["status"] == OPERATOR_STATUS {
            bail!("the operator identity does not receive messages");
        }
        Ok(vec![target])
    }
    /// Post an operator message as the synthetic `operator:TASK` sender.
    ///
    /// `destination` defaults to `task` (fan-out to every non-operator worker).
    /// Pass a worker id to target only that worker; it must belong to the task
    /// and must not be the operator identity.
    pub fn steer(
        &self,
        oid: &str,
        mid: &str,
        body: &str,
        refs: &Value,
        actionable: bool,
        destination: Option<&str>,
    ) -> Result<Value> {
        if self.task(oid)?["status"] == "cancelled" {
            bail!("task is cancelled");
        }
        let destination = destination.unwrap_or("task");
        let sender = operator_id(oid);
        self.atomic(|| {
            // The token hash is derived from an id that is never returned, so nothing can
            // authenticate as the operator row through worker credentials.
            let created = self.conn.execute(
                "INSERT OR IGNORE INTO workers(id,task,step,status,token_hash,updated) VALUES(?,?,NULL,?,?,?)",
                params![sender, oid, OPERATOR_STATUS, hash(id().as_bytes()), now()],
            )?;
            self.conn
                .execute("INSERT OR IGNORE INTO cursors(worker) VALUES(?)", [&sender])?;
            self.conn.execute(
                "INSERT OR IGNORE INTO notifications(worker) VALUES(?)",
                [&sender],
            )?;
            if created == 1 {
                self.event(oid, "operator.registered", json!({"worker":sender}))?;
            }
            let mut result = self.send(oid, &sender, mid, destination, body, refs, actionable)?;
            result["sender"] = json!(sender);
            Ok(result)
        })
    }
    pub fn messages(&self, wid: &str, after: i64, limit: i64) -> Result<Value> {
        self.worker(wid)?;
        Ok(json!(self.rows("SELECT m.* FROM messages m JOIN receipts r ON r.message=m.id WHERE r.worker=? AND r.ack=0 AND m.seq>? ORDER BY m.seq LIMIT ?",&[&wid,&after,&limit.clamp(1,1000)])?))
    }
    pub fn acknowledge(&self, wid: &str, ids: &[String]) -> Result<()> {
        self.atomic(||{for mid in ids {if self.conn.execute("UPDATE receipts SET ack=1 WHERE worker=? AND message=?",params![wid,mid])?!=1{bail!("message not in worker mailbox");}}
            // Cursor advances only past a contiguous acknowledged prefix, never skips an unread message.
            self.conn.execute("UPDATE cursors SET seq=COALESCE((SELECT MIN(m.seq)-1 FROM messages m JOIN receipts r ON m.id=r.message WHERE r.worker=? AND r.ack=0),(SELECT COALESCE(MAX(seq),0) FROM messages)) WHERE worker=?",params![wid,wid])?;Ok(())})
    }
    pub fn artifact(
        &self,
        oid: &str,
        step: Option<&str>,
        name: &str,
        bytes: &[u8],
        inputs: &Value,
        verified: bool,
    ) -> Result<String> {
        self.task(oid)?;
        if let Some(t) = step {
            let o: String = self
                .conn
                .query_row("SELECT task FROM steps WHERE id=?", [t], |r| r.get(0))?;
            if o != oid {
                bail!("artifact step belongs to another task");
            }
        }
        let h = hash(bytes);
        let dir = self.root.join("artifacts");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(&h);
        if !path.exists() {
            let tmp = dir.join(id());
            std::fs::write(&tmp, bytes)?;
            std::fs::File::open(&tmp)?.sync_all()?;
            std::fs::rename(tmp, path)?;
            std::fs::File::open(&dir)?.sync_all()?;
        }
        self.atomic(|| {
            self.conn.execute(
                "INSERT OR IGNORE INTO artifacts VALUES(?,?,?)",
                params![h, bytes.len() as i64, now()],
            )?;
            self.conn.execute(
                "INSERT OR REPLACE INTO artifact_links VALUES(?,?,?,?,?,?)",
                params![oid, step, name, h, inputs.to_string(), verified],
            )?;
            Ok(())
        })?;
        Ok(h)
    }
    pub fn steps(&self, oid: &str) -> Result<Vec<Value>> {
        self.rows("SELECT * FROM steps WHERE task=? ORDER BY rowid", &[&oid])
    }
    pub fn finish(
        &self,
        step: &str,
        attempt: &str,
        worker: &str,
        result: Result<Value>,
    ) -> Result<()> {
        let row = self
            .rows("SELECT * FROM steps WHERE id=?", &[&step])?
            .into_iter()
            .next()
            .context("step")?;
        let oid = row["task"].as_str().context("task")?;
        let loop_detected = result
            .as_ref()
            .err()
            .is_some_and(|e| e.is::<crate::native_protocol::RepeatedToolCall>());
        let success = result.is_ok();
        let value = match result {
            Ok(v) => v,
            Err(e) => e
                .downcast_ref::<crate::budget::Exhausted>()
                .map(|e| e.0.clone())
                .unwrap_or_else(|| json!({"error":format!("{e:#}")})),
        };
        let value = crate::secrets::redact(self, oid, &value);
        let waiting = crate::delegation::has_question(self, worker)?;
        let success = success && !waiting;
        self.atomic(|| {
            let current: String =
                self.conn
                    .query_row("SELECT state FROM steps WHERE id=?", [step], |r| r.get(0))?;
            let state = if current == "cancelled" {
                "cancelled"
            } else if waiting {
                "waiting"
            } else if success {
                "succeeded"
            } else {
                "failed"
            };
            self.conn.execute(
                "UPDATE steps SET state=?,result=? WHERE id=?",
                params![state, value.to_string(), step],
            )?;
            self.conn.execute(
                "UPDATE attempts SET state=?,finished=?,result=?,usage=COALESCE(?,usage) WHERE id=?",
                params![
                    state,
                    now(),
                    value.to_string(),
                    value.get("usage").map(Value::to_string),
                    attempt
                ],
            )?;
            self.conn.execute(
                "UPDATE workers SET status=?,updated=? WHERE id=?",
                params![if success {
                    let unread:i64=self.conn.query_row("SELECT COUNT(*) FROM messages m JOIN receipts r ON r.message=m.id WHERE r.worker=? AND r.ack=0 AND m.actionable=1 AND m.seq>COALESCE((SELECT dispatched_seq FROM notifications WHERE worker=?),0)",params![worker,worker],|r|r.get(0))?;
                    if unread>0 {"notified"} else {"idle"}
                } else { "failed" }, now(), worker],
            )?;
            let answered:i64=self.conn.query_row("SELECT COUNT(*) FROM question_context WHERE worker=? AND purpose='input_answered'",[worker],|r|r.get(0))?;
            if answered>0 {
                if !success && state!="cancelled" {self.conn.execute("UPDATE steps SET state='pending' WHERE id=?",[step])?;}
                self.conn.execute("UPDATE question_context SET purpose='input_consumed' WHERE worker=? AND purpose='input_answered'",[worker])?;
            }
            if loop_detected && state == "failed" {
                self.conn.execute("UPDATE tasks SET status='blocked' WHERE id=? AND status='running'", [oid])?;
                self.event(oid, "task.blocked", json!({"reason":"repeated_tool_call","step":step,"attempt":attempt}))?;
            }
            // Failed or uncertain workers retain claims until explicit reconciliation.
            if success {
                self.conn
                    .execute("DELETE FROM claims WHERE worker=?", [worker])?;
            }
            self.event(
                oid,
                "step.finished",
                json!({"step":step,"attempt":attempt,"state":state,"result":value,"timing":crate::budget::status(self,attempt)?}),
            )?;
            Ok(())
        })
    }
    pub fn step(row: &Value) -> Result<Step> {
        Ok(serde_json::from_str(
            row["spec"].as_str().context("step spec")?,
        )?)
    }
}
