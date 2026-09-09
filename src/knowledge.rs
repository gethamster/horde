//! Task-owned claims with opt-in family visibility. Never drives execution state.
use crate::{
    config::Settings,
    delegation,
    store::{Store, id, now},
};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const SCOPES: &[&str] = &["task", "family"];
const PAGE_BYTES: usize = 256 * 1024;

pub fn migrate(c: &rusqlite::Connection) -> Result<()> {
    c.execute_batch("BEGIN IMMEDIATE;
CREATE TABLE IF NOT EXISTS knowledge_state(id INTEGER PRIMARY KEY CHECK(id=1),revision INTEGER NOT NULL);
INSERT OR IGNORE INTO knowledge_state VALUES(1,0);
CREATE TABLE IF NOT EXISTS knowledge_meta(id TEXT PRIMARY KEY REFERENCES knowledge(id),scope TEXT NOT NULL DEFAULT 'task' CHECK(scope IN ('task','family')),topic TEXT NOT NULL DEFAULT '',valid_under TEXT NOT NULL DEFAULT '{}',origin TEXT NOT NULL DEFAULT '{}',superseded_by TEXT REFERENCES knowledge(id),retracted INTEGER NOT NULL DEFAULT 0,retraction TEXT,created INTEGER NOT NULL DEFAULT 0);
INSERT OR IGNORE INTO knowledge_meta(id) SELECT id FROM knowledge;
CREATE TABLE IF NOT EXISTS knowledge_write_receipts(id TEXT PRIMARY KEY REFERENCES knowledge(id),request_hash TEXT NOT NULL);
CREATE VIRTUAL TABLE IF NOT EXISTS knowledge_fts USING fts5(content,content='knowledge',content_rowid='rowid');
CREATE TRIGGER IF NOT EXISTS knowledge_insert AFTER INSERT ON knowledge BEGIN
 INSERT INTO knowledge_meta(id) VALUES(new.id);
 INSERT INTO knowledge_fts(rowid,content) VALUES(new.rowid,new.content);
 UPDATE knowledge_state SET revision=revision+1 WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS knowledge_update AFTER UPDATE ON knowledge BEGIN
 INSERT INTO knowledge_fts(knowledge_fts,rowid,content) VALUES('delete',old.rowid,old.content);
 INSERT INTO knowledge_fts(rowid,content) VALUES(new.rowid,new.content);
 UPDATE knowledge_state SET revision=revision+1 WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS knowledge_delete AFTER DELETE ON knowledge BEGIN
 INSERT INTO knowledge_fts(knowledge_fts,rowid,content) VALUES('delete',old.rowid,old.content);
 UPDATE knowledge_state SET revision=revision+1 WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS knowledge_meta_update AFTER UPDATE ON knowledge_meta BEGIN
 UPDATE knowledge_state SET revision=revision+1 WHERE id=1;
END;
CREATE TRIGGER IF NOT EXISTS knowledge_edge_insert AFTER INSERT ON knowledge_edges BEGIN
 UPDATE knowledge_state SET revision=revision+1 WHERE id=1;
END;
CREATE TABLE IF NOT EXISTS knowledge_index_version(version INTEGER PRIMARY KEY);
COMMIT;")?;
    let indexed: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM knowledge_index_version WHERE version=1)",
        [],
        |r| r.get(0),
    )?;
    if !indexed {
        c.execute_batch("BEGIN IMMEDIATE; INSERT INTO knowledge_fts(knowledge_fts) VALUES('rebuild'); INSERT OR IGNORE INTO knowledge_index_version VALUES(1); COMMIT;")?;
    }
    Ok(())
}

pub fn topics(db: &Store, task: &str) -> Result<Vec<String>> {
    let root = delegation::root(db, task)?;
    let settings: Settings =
        serde_json::from_str(db.task(&root)?["settings"].as_str().context("settings")?)?;
    Ok(settings.knowledge_topics)
}

pub fn apply_topics(schema: &mut Value, topics: &[String]) {
    if !topics.is_empty() && schema["properties"].get("topic").is_some() {
        schema["properties"]["topic"]["enum"] = json!(topics);
    }
}

fn scope(args: &Value) -> Result<&str> {
    let scope = args
        .get("scope")
        .map(|v| v.as_str().context("scope must be task or family"))
        .transpose()?
        .unwrap_or("task");
    ensure!(
        SCOPES.contains(&scope),
        "invalid knowledge scope; expected task or family"
    );
    Ok(scope)
}
fn text<'a>(args: &'a Value, key: &str, max: usize) -> Result<&'a str> {
    let value = args[key]
        .as_str()
        .with_context(|| format!("{key} must be a string"))?;
    ensure!(
        !value.trim().is_empty() && value.len() <= max,
        "{key} must be nonempty and at most {max} bytes"
    );
    Ok(value)
}
fn object(args: &Value, key: &str, required: bool) -> Result<Value> {
    let value = args
        .get(key)
        .cloned()
        .unwrap_or_else(|| if required { Value::Null } else { json!({}) });
    ensure!(value.is_object(), "{key} must be an object");
    ensure!(
        serde_json::to_vec(&value)?.len() <= 16384,
        "{key} exceeds 16 KiB"
    );
    Ok(value)
}
fn row(db: &Store, key: &str) -> Result<Value> {
    db.rows("SELECT k.*,m.scope,m.topic,m.valid_under,m.origin,m.superseded_by,m.retracted,m.retraction,m.created,t.root FROM knowledge k JOIN knowledge_meta m ON k.id=m.id JOIN task_tree t ON t.task=k.task WHERE k.id=?", &[&key])?.into_iter().next().context("knowledge record not found")
}
fn visible(row: &Value, task: &str, root: &str) -> bool {
    row["task"] == task || (row["scope"] == "family" && row["root"] == root)
}
fn may_revise(row: &Value, task: &str, root: &str, operator: bool) -> Result<()> {
    ensure!(
        visible(row, task, root),
        "knowledge record is not visible to this task"
    );
    ensure!(
        row["task"] == task || operator,
        "only the originating task or an operator may revise knowledge"
    );
    Ok(())
}
fn decorate(mut row: Value) -> Result<Value> {
    for key in ["valid_under", "origin", "retraction"] {
        if let Some(raw) = row[key].as_str() {
            row[key] = serde_json::from_str(raw)?;
        }
    }
    if row["origin"] == json!({}) {
        row["origin"] = json!({"task":row["task"],"step":row["step"]});
    }
    row["superseded"] = json!(row["superseded_by"].is_string());
    row["retracted"] = json!(row["retracted"] == 1);
    Ok(row)
}

pub fn add(db: &Store, task: &str, args: &Value, operator: bool) -> Result<Value> {
    let scope = scope(args)?;
    let kind = text(args, "kind", 32)?;
    ensure!(
        crate::protocol::KNOWLEDGE_KINDS.contains(&kind),
        "invalid knowledge kind; expected one of: {}",
        crate::protocol::KNOWLEDGE_KINDS.join(", ")
    );
    let content = text(args, "content", 65536)?;
    let provenance = object(args, "provenance", true)?;
    let conditions = object(args, "valid_under", false)?;
    let topic = args
        .get("topic")
        .map(|v| v.as_str().context("topic must be a string"))
        .transpose()?
        .unwrap_or("");
    ensure!(topic.len() <= 128, "topic exceeds 128 bytes");
    let vocabulary = topics(db, task)?;
    ensure!(
        args.get("topic").is_none()
            || vocabulary.is_empty()
            || vocabulary.iter().any(|t| t == topic),
        "unknown knowledge topic; expected one of: {}",
        vocabulary.join(", ")
    );
    let step = args["step"].as_str();
    if let Some(step) = step {
        ensure!(
            db.steps(task)?.iter().any(|s| s["id"] == step),
            "step not in task"
        );
    }
    let root = delegation::root(db, task)?;
    let mut supersedes: Vec<String> = args
        .get("supersedes")
        .cloned()
        .map(serde_json::from_value)
        .transpose()?
        .unwrap_or_default();
    ensure!(supersedes.len() <= 100, "at most 100 superseded records");
    supersedes.sort();
    supersedes.dedup();
    let key = args
        .get("id")
        .map(|v| v.as_str().context("id must be a string"))
        .transpose()?
        .map(str::to_owned)
        .unwrap_or_else(id);
    ensure!(!key.is_empty() && key.len() <= 128, "invalid knowledge id");
    let origin = args
        .get("_knowledge_origin")
        .cloned()
        .unwrap_or_else(|| json!({"task":task,"step":step}));
    let verified = operator && args["verified"].as_bool().unwrap_or(false);
    let inputs = args.get("inputs").cloned().unwrap_or(Value::Null);
    ensure!(
        serde_json::to_vec(&inputs)?.len() <= 16384,
        "inputs exceeds 16 KiB"
    );
    let request_hash=crate::store::hash(json!({"task":task,"scope":scope,"kind":kind,"content":content,"provenance":provenance,"conditions":conditions,"topic":topic,"origin":origin,"verified":verified,"inputs":inputs,"supersedes":supersedes}).to_string().as_bytes());
    db.atomic(|| {
        // A caller-supplied ID makes retries after a lost remote reply idempotent.
        let receipt=db.rows("SELECT request_hash FROM knowledge_write_receipts WHERE id=?",&[&key])?;
        if let Some(old)=receipt.first() {
            ensure!(old["request_hash"]==request_hash,"knowledge id reused with a different claim");
            return Ok(json!({"id":key,"duplicate":true}));
        }
        for old in &supersedes {
            let old=row(db,old)?;
            may_revise(&old,task,&root,operator)?;
            ensure!(old["scope"]==scope,"supersession must preserve knowledge scope");
            ensure!(old["superseded_by"].is_null() && old["retracted"]==0,"cannot supersede an inactive claim");
        }
        db.conn.execute("INSERT INTO knowledge(id,task,step,kind,content,provenance,verified,inputs) VALUES(?,?,?,?,?,?,?,?)",params![key,task,step,kind,content,provenance.to_string(),verified,inputs.to_string()])?;
        db.conn.execute("UPDATE knowledge_meta SET scope=?,topic=?,valid_under=?,origin=?,created=? WHERE id=?",params![scope,topic,conditions.to_string(),origin.to_string(),now(),key])?;
        db.conn.execute("INSERT INTO knowledge_write_receipts VALUES(?,?)",params![key,request_hash])?;
        for old in &supersedes {
            db.conn.execute("UPDATE knowledge_meta SET superseded_by=? WHERE id=?",params![key,old])?;
            db.conn.execute("INSERT OR IGNORE INTO knowledge_edges VALUES(?,?,'supersedes')",params![key,old])?;
        }
        db.event(task,"knowledge.added",json!({"id":key,"scope":scope,"topic":topic,"supersedes":supersedes,"step":step}))?;
        Ok(json!({"id":key}))
    })
}

#[derive(Serialize, Deserialize)]
struct Cursor {
    revision: i64,
    offset: i64,
    filter: String,
}

fn read_snapshot<T>(db: &Store, read: impl FnOnce() -> Result<T>) -> Result<T> {
    if !db.conn.is_autocommit() {
        return read();
    }
    let transaction = db.conn.unchecked_transaction()?;
    let result = read()?;
    transaction.commit()?;
    Ok(result)
}

pub fn read(db: &Store, task: &str, args: &Value) -> Result<Value> {
    let scope = scope(args)?;
    let legacy = [
        "scope",
        "topic",
        "query",
        "after",
        "limit",
        "include_inactive",
    ]
    .iter()
    .all(|k| args.get(k).is_none());
    let root = delegation::root(db, task)?;
    let topic = args
        .get("topic")
        .map(|v| v.as_str().context("topic must be a string"))
        .transpose()?;
    let query = args
        .get("query")
        .map(|v| v.as_str().context("query must be a string"))
        .transpose()?;
    ensure!(
        query.is_none_or(|q| !q.trim().is_empty() && q.len() <= 1024),
        "query must be nonempty and at most 1024 bytes"
    );
    let inactive = args
        .get("include_inactive")
        .map(|v| v.as_bool().context("include_inactive must be boolean"))
        .transpose()?
        .unwrap_or(false);
    let limit = args
        .get("limit")
        .map(|v| v.as_i64().context("limit must be an integer"))
        .transpose()?
        .unwrap_or(50)
        .clamp(1, 100);
    let filter = crate::store::hash(
        json!([task, scope, topic, query, inactive])
            .to_string()
            .as_bytes(),
    );
    let cursor = args
        .get("after")
        .filter(|v| !v.is_null())
        .map(|v| -> Result<Cursor> {
            let s = v
                .as_str()
                .context("after must be the returned cursor string")?;
            ensure!(s.len() <= 2048, "invalid knowledge cursor");
            Ok(serde_json::from_slice(
                &hex::decode(s).context("invalid knowledge cursor")?,
            )?)
        })
        .transpose()?;
    read_snapshot(db, || {
        if legacy {
            return Ok(json!(db.rows("SELECT k.* FROM knowledge k JOIN knowledge_meta m ON m.id=k.id WHERE k.task=? AND m.retracted=0 AND m.superseded_by IS NULL ORDER BY k.rowid", &[&task])?));
        }
        let revision: i64 =
            db.conn
                .query_row("SELECT revision FROM knowledge_state WHERE id=1", [], |r| {
                    r.get(0)
                })?;
        let offset = if let Some(cursor) = &cursor {
            ensure!(
                cursor.filter == filter && cursor.offset >= 0,
                "cursor does not match this knowledge query"
            );
            ensure!(
                cursor.revision == revision,
                "knowledge changed during pagination; restart without after"
            );
            cursor.offset
        } else {
            0
        };
        let score = if query.is_some() {
            "bm25(knowledge_fts)"
        } else {
            "0.0"
        };
        let join = if query.is_some() {
            "JOIN knowledge_fts ON knowledge_fts.rowid=k.rowid"
        } else {
            ""
        };
        let matched = if query.is_some() {
            "AND knowledge_fts MATCH ?7"
        } else {
            "AND ?7 IS NULL"
        };
        let sql = format!(
            "SELECT k.*,m.scope,m.topic,m.valid_under,m.origin,m.superseded_by,m.retracted,m.retraction,m.created,t.root,{score} AS rank FROM knowledge k JOIN knowledge_meta m ON m.id=k.id JOIN task_tree t ON t.task=k.task {join} WHERE ((?1='task' AND k.task=?2) OR (?1='family' AND m.scope='family' AND t.root=?3)) AND (?4 IS NULL OR m.topic=?4) AND (?5 OR (m.retracted=0 AND m.superseded_by IS NULL)) {matched} ORDER BY rank,k.rowid LIMIT ?6 OFFSET ?8"
        );
        let candidates = db
            .rows(
                &sql,
                &[
                    &scope,
                    &task,
                    &root,
                    &topic,
                    &inactive,
                    &(limit + 1),
                    &query,
                    &offset,
                ],
            )
            .context("knowledge search failed; use a valid FTS5 query")?;
        let mut records = vec![];
        let mut bytes = 0;
        let mut more = false;
        for candidate in candidates {
            let mut record = decorate(candidate)?;
            let key = record["id"].as_str().context("knowledge id")?.to_owned();
            let edges=db.rows("SELECT e.* FROM knowledge_edges e JOIN knowledge k ON k.id=e.target JOIN knowledge_meta m ON m.id=k.id JOIN task_tree t ON t.task=k.task WHERE e.source=? AND (k.task=? OR (m.scope='family' AND t.root=?)) ORDER BY e.relation,e.target LIMIT 101", &[&key,&task,&root])?;
            record["edges_truncated"] = json!(edges.len() > 100);
            record["edges_next"] = if edges.len() > 100 {
                json!(hex::encode(serde_json::to_vec(&Cursor {
                    revision,
                    offset: 100,
                    filter: crate::store::hash(json!(["edges", task, key]).to_string().as_bytes())
                })?))
            } else {
                Value::Null
            };
            record["edges"] = json!(edges.into_iter().take(100).collect::<Vec<_>>());
            let size = serde_json::to_vec(&record)?.len();
            if records.len() >= limit as usize || bytes + size > PAGE_BYTES {
                ensure!(
                    !records.is_empty(),
                    "knowledge record exceeds page byte limit"
                );
                more = true;
                break;
            }
            bytes += size;
            records.push(record);
        }
        let next = if more {
            Some(hex::encode(serde_json::to_vec(&Cursor {
                revision,
                offset: offset + records.len() as i64,
                filter: filter.clone(),
            })?))
        } else {
            None
        };
        Ok(json!({"scope":scope,"root":root,"records":records,"next":next,"revision":revision}))
    })
}

pub fn retract(db: &Store, task: &str, args: &Value, operator: bool) -> Result<Value> {
    let key = text(args, "id", 128)?;
    let reason = text(args, "reason", 4096)?;
    let provenance = object(args, "provenance", true)?;
    let root = delegation::root(db, task)?;
    db.atomic(|| {
        let old = row(db, key)?;
        may_revise(&old, task, &root, operator)?;
        if old["retracted"] == 1 {
            let previous: Value =
                serde_json::from_str(old["retraction"].as_str().context("retraction")?)?;
            ensure!(
                previous["reason"] == reason && previous["provenance"] == provenance,
                "knowledge already retracted with different provenance"
            );
            return Ok(json!({"id":key,"retracted":true,"duplicate":true}));
        }
        db.conn.execute(
            "UPDATE knowledge_meta SET retracted=1,retraction=? WHERE id=?",
            params![
                json!({"task":task,"reason":reason,"provenance":provenance,"at":now()}).to_string(),
                key
            ],
        )?;
        db.event(
            task,
            "knowledge.retracted",
            json!({"id":key,"reason":reason}),
        )?;
        Ok(json!({"id":key,"retracted":true}))
    })
}

pub fn link(db: &Store, task: &str, args: &Value, operator: bool) -> Result<Value> {
    let source = text(args, "source", 128)?;
    let target = text(args, "target", 128)?;
    let relation = text(args, "relation", 128)?;
    let root = delegation::root(db, task)?;
    db.atomic(|| {
        let from = row(db, source)?;
        may_revise(&from, task, &root, operator)?;
        ensure!(
            visible(&row(db, target)?, task, &root),
            "knowledge target is not visible to this task"
        );
        db.conn.execute(
            "INSERT OR IGNORE INTO knowledge_edges VALUES(?,?,?)",
            params![source, target, relation],
        )?;
        Ok(json!({"linked":true}))
    })
}

pub fn edges(db: &Store, task: &str, args: &Value) -> Result<Value> {
    let source = text(args, "source", 128)?;
    let root = delegation::root(db, task)?;
    let limit = args
        .get("limit")
        .map(|v| v.as_i64().context("limit must be an integer"))
        .transpose()?
        .unwrap_or(50)
        .clamp(1, 100);
    let filter = crate::store::hash(json!(["edges", task, source]).to_string().as_bytes());
    let cursor = args
        .get("after")
        .filter(|v| !v.is_null())
        .map(|v| -> Result<Cursor> {
            let value = v.as_str().context("after must be a cursor string")?;
            ensure!(value.len() <= 2048, "invalid knowledge cursor");
            Ok(serde_json::from_slice(
                &hex::decode(value).context("invalid knowledge cursor")?,
            )?)
        })
        .transpose()?;
    read_snapshot(db, || {
        ensure!(
            visible(&row(db, source)?, task, &root),
            "knowledge record is not visible to this task"
        );
        let revision: i64 =
            db.conn
                .query_row("SELECT revision FROM knowledge_state WHERE id=1", [], |r| {
                    r.get(0)
                })?;
        let offset = if let Some(cursor) = cursor {
            ensure!(
                cursor.filter == filter && cursor.offset >= 0,
                "cursor does not match this knowledge query"
            );
            ensure!(
                cursor.revision == revision,
                "knowledge changed during pagination; restart without after"
            );
            cursor.offset
        } else {
            0
        };
        let mut edges=db.rows("SELECT e.* FROM knowledge_edges e JOIN knowledge k ON k.id=e.target JOIN knowledge_meta m ON m.id=k.id JOIN task_tree t ON t.task=k.task WHERE e.source=? AND (k.task=? OR (m.scope='family' AND t.root=?)) ORDER BY e.relation,e.target LIMIT ? OFFSET ?",&[&source,&task,&root,&(limit+1),&offset])?;
        let more = edges.len() > limit as usize;
        edges.truncate(limit as usize);
        let next = if more {
            Some(hex::encode(serde_json::to_vec(&Cursor {
                revision,
                offset: offset + edges.len() as i64,
                filter,
            })?))
        } else {
            None
        };
        Ok(json!({"source":source,"edges":edges,"next":next,"revision":revision}))
    })
}
