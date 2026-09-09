//! Administrative runtime state. Worker credentials never enter these handlers.
use crate::{
    config::Settings,
    store::{Store, now},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

pub fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS runtime_presence(runtime TEXT PRIMARY KEY,observed INTEGER NOT NULL,status TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS runtime_enrollments(runtime TEXT PRIMARY KEY,fingerprint TEXT UNIQUE NOT NULL,token_hash TEXT NOT NULL,expires INTEGER NOT NULL,state TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS fleet_enrollment_keys(id TEXT PRIMARY KEY,name TEXT NOT NULL,token_hash TEXT NOT NULL,expires INTEGER NOT NULL,max_workers INTEGER NOT NULL,concurrency INTEGER NOT NULL,state TEXT NOT NULL,created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS fleet_enrollment_members(runtime TEXT PRIMARY KEY,key_id TEXT NOT NULL REFERENCES fleet_enrollment_keys(id),public_key_hash TEXT UNIQUE NOT NULL,state TEXT NOT NULL,created INTEGER NOT NULL,current_fingerprint TEXT);
CREATE TABLE IF NOT EXISTS fleet_enrollment_certificates(fingerprint TEXT PRIMARY KEY,runtime TEXT NOT NULL REFERENCES fleet_enrollment_members(runtime),certificate_pem TEXT NOT NULL,expires INTEGER NOT NULL,renew_after INTEGER NOT NULL,concurrency INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS fleet_member_key ON fleet_enrollment_members(key_id);
CREATE INDEX IF NOT EXISTS fleet_certificate_runtime ON fleet_enrollment_certificates(runtime);
CREATE TABLE IF NOT EXISTS runtime_settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS account_capacity(account TEXT NOT NULL,window TEXT NOT NULL,provider TEXT NOT NULL,used REAL,reset INTEGER,observed INTEGER NOT NULL,source TEXT NOT NULL,PRIMARY KEY(account,window));
CREATE TABLE IF NOT EXISTS attempt_accounts(attempt TEXT PRIMARY KEY REFERENCES attempts(id),account TEXT NOT NULL,role TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS management_events(seq INTEGER PRIMARY KEY AUTOINCREMENT,kind TEXT NOT NULL,data TEXT NOT NULL,created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS management_receipts(consumer TEXT PRIMARY KEY,seq INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS managed_runtimes(id TEXT PRIMARY KEY,profile TEXT NOT NULL,spec TEXT NOT NULL,resource TEXT,state TEXT NOT NULL,version TEXT,created INTEGER NOT NULL,error TEXT);
CREATE TABLE IF NOT EXISTS runtime_operations(id TEXT PRIMARY KEY,runtime TEXT NOT NULL,action TEXT NOT NULL,args TEXT NOT NULL,state TEXT NOT NULL,created INTEGER NOT NULL,result TEXT);")?;
    Ok(())
}
pub fn event(db: &Store, kind: &str, data: Value) -> Result<()> {
    db.conn.execute(
        "INSERT INTO management_events(kind,data,created) VALUES(?,?,?)",
        params![kind, data.to_string(), now()],
    )?;
    Ok(())
}
pub fn value(db: &Store, key: &str) -> Result<Option<String>> {
    Ok(db
        .conn
        .query_row(
            "SELECT value FROM runtime_settings WHERE key=?",
            [key],
            |r| r.get(0),
        )
        .optional()?)
}
pub fn set(db: &Store, key: &str, v: &str) -> Result<()> {
    db.conn.execute("INSERT INTO runtime_settings VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,v])?;
    Ok(())
}
pub fn limit(db: &Store) -> Result<usize> {
    match value(db, "concurrency")? {
        Some(v) => Ok(v.parse()?),
        None => {
            if let Ok(v) = crate::branding::var("HORDE_CONCURRENCY") {
                let n: usize = v.parse()?;
                ensure!((1..=64).contains(&n), "invalid HORDE_CONCURRENCY");
                Ok(n)
            } else {
                Ok(Settings::load_user()?.concurrency)
            }
        }
    }
}
pub fn draining(db: &Store) -> Result<bool> {
    Ok(value(db, "draining")?.as_deref() == Some("true"))
}
pub fn dispatch(db: &Store, name: &str, args: &Value) -> Result<Option<Value>> {
    if let Some(v) = crate::fleet::dispatch(db, name, args)? {
        return Ok(Some(v));
    }
    let result = match name {
        "runtime_updates_resume" => {
            set(db, "fleet_updates_paused", "false")?;
            json!({"updates_resumed":true})
        }
        "runtime_config_get" => json!({"concurrency":limit(db)?,"draining":draining(db)?}),
        "runtime_config_set" => {
            let n = args["concurrency"]
                .as_u64()
                .context("concurrency is required")?;
            ensure!(
                (1..=64).contains(&n),
                "concurrency must be between 1 and 64"
            );
            db.atomic(|| {
                set(db, "concurrency", &n.to_string())?;
                event(db, "config.changed", json!({"concurrency":n}))
            })?;
            json!({"concurrency":n})
        }
        "runtime_drain" => {
            db.atomic(|| {
                set(db, "draining", "true")?;
                event(db, "runtime.draining", json!({}))
            })?;
            status(db)?
        }
        "runtime_resume" => {
            db.atomic(|| {
                set(db, "draining", "false")?;
                event(db, "runtime.resumed", json!({}))
            })?;
            status(db)?
        }
        "runtime_status" => status(db)?,
        "account_status" => crate::capacity::report(db)?,
        "account_observe" => {
            let snapshot: crate::capacity::Snapshot = serde_json::from_value(args.clone())?;
            crate::capacity::observe(db, &snapshot)?;
            json!({"recorded":true})
        }
        "management_events" => {
            let after = args["after"].as_i64().unwrap_or(0);
            db.rows(
                "SELECT * FROM management_events WHERE seq>? ORDER BY seq LIMIT 1000",
                &[&after],
            )?
            .into()
        }
        "management_ack" => {
            let consumer = args["consumer"].as_str().context("consumer required")?;
            let seq = args["seq"].as_i64().context("seq required")?;
            let max: i64 = db.conn.query_row(
                "SELECT COALESCE(MAX(seq),0) FROM management_events",
                [],
                |r| r.get(0),
            )?;
            ensure!(seq >= 0 && seq <= max, "invalid event cursor");
            db.conn.execute("INSERT INTO management_receipts VALUES(?,?) ON CONFLICT(consumer) DO UPDATE SET seq=MAX(seq,excluded.seq)",params![consumer,seq])?;
            json!({"acknowledged":seq})
        }
        _ => return Ok(None),
    };
    Ok(Some(result))
}
pub fn status(db: &Store) -> Result<Value> {
    let active: i64 = db.conn.query_row(
        "SELECT COUNT(*) FROM attempts WHERE state='running'",
        [],
        |r| r.get(0),
    )?;
    let catalog = crate::skill_catalog::load_for(&db.root)
        .and_then(|packet| crate::skill_catalog::summary(&packet));
    let (skill_pack, skill_pack_error) = match catalog {
        Ok(report) => (report, None),
        Err(error) => (Value::Null, Some(error.to_string())),
    };
    Ok(
        json!({"pid":std::process::id(),"version":env!("CARGO_PKG_VERSION"),"concurrency":limit(db)?,"active":active,"draining":draining(db)?,"drained":draining(db)?&&active==0,"update_state":value(db,"update_state")?,"fleet_updates_paused":value(db,"fleet_updates_paused")?.as_deref()==Some("true"),"skill_pack":skill_pack,"skill_pack_error":skill_pack_error}),
    )
}

/// Return management progress without retransmitting a captured skill packet.
pub fn operation_receipt(row: &Value) -> Result<Value> {
    if row["action"] != "runtime_skills_update" {
        return Ok(row.clone());
    }
    let mut receipt = row.as_object().context("operation receipt")?.clone();
    if let Some(args) = row["args"].as_str() {
        let args: Value = serde_json::from_str(args)?;
        let mut visible = args.as_object().context("operation arguments")?.clone();
        if let Some(packet) = visible.remove("packet") {
            let packet: crate::skills::Packet = serde_json::from_value(packet)?;
            receipt.insert(
                "requested_catalog".into(),
                crate::skill_catalog::summary(&packet)?,
            );
        }
        receipt.insert("args".into(), json!(Value::Object(visible).to_string()));
    }
    if let Some(result) = row["result"].as_str() {
        let result: Value = serde_json::from_str(result)?;
        receipt.insert(
            "result".into(),
            json!(operation_receipt(&result)?.to_string()),
        );
    }
    Ok(Value::Object(receipt))
}

/// Seed missing defaults without racing a user or parent catalog change.
pub fn bootstrap_catalog(db: &Store, packet: &crate::skills::Packet) -> Result<Value> {
    db.atomic(|| {
        let current = db.root.join("skill-packs/CURRENT");
        let executable = std::env::current_exe()?;
        let adjacent = executable.parent().context("executable directory")?.join("skills");
        for path in [&current, &adjacent] {
            match std::fs::symlink_metadata(path) {
                Ok(_) => return Ok(json!({"skipped":true})),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        let pending: bool = db.conn.query_row("SELECT EXISTS(SELECT 1 FROM runtime_operations WHERE runtime='local' AND action='runtime_skills_update' AND state='local_pending')", [], |row| row.get(0))?;
        if pending || crate::skill_catalog::load_defaults().is_ok() {
            return Ok(json!({"skipped":true}));
        }
        crate::skill_catalog::install(&db.root, packet)
    })
}

/// Local and parent-requested catalog installs share durable arrival ordering.
pub fn install_catalog(db: &Store, packet: &crate::skills::Packet) -> Result<Value> {
    let request = crate::store::id();
    remote_command(
        db,
        "local-admin",
        &json!({"action":"runtime_skills_update","request_id":request,"packet":packet}),
    )?;
    let id = format!("local-admin:{request}");
    for op in db.rows("SELECT * FROM runtime_operations WHERE runtime='local' AND action='runtime_skills_update' AND state='local_pending' ORDER BY rowid", &[])? {
        install_skills(db, &op)?;
        if op["id"] == id { break; }
    }
    let receipt = db
        .rows(
            "SELECT state,result FROM runtime_operations WHERE id=?",
            &[&id],
        )?
        .remove(0);
    let result: Value =
        serde_json::from_str(receipt["result"].as_str().context("skill update result")?)?;
    ensure!(
        receipt["state"] == "succeeded",
        "{}",
        result["error"].as_str().unwrap_or("skill update failed")
    );
    Ok(result)
}

pub fn remote_command(db: &Store, peer: &str, args: &Value) -> Result<Value> {
    if let Some(action) = args["action"].as_str()
        && ["runtime_status", "runtime_drain", "runtime_resume"].contains(&action)
    {
        return dispatch(db, action, &json!({}))?.context("management action unavailable");
    }
    let request = args["request_id"].as_str().context("request_id required")?;
    let id = format!("{peer}:{request}");
    let action = args["action"].as_str().context("action required")?;
    ensure!(
        ["runtime_update", "runtime_restart", "runtime_skills_update"].contains(&action),
        "unsupported management command"
    );
    if action == "runtime_update" {
        crate::update::validate_version(args["version"].as_str().context("version required")?)?;
    }
    if action == "runtime_skills_update" {
        let packet: crate::skills::Packet = serde_json::from_value(args["packet"].clone())?;
        crate::skills::validate(&packet)?;
    }
    ensure!(
        !request.is_empty() && request.len() <= 256,
        "invalid request_id"
    );
    db.atomic(|| {
        let old = db.rows("SELECT * FROM runtime_operations WHERE id=?", &[&id])?;
        if let Some(old) = old.first() {
            ensure!(old["args"].as_str() == Some(args.to_string().as_str()) && old["action"] == action,
                "changed management command for request ID");
            let mut reply = old.as_object().context("operation receipt")?.clone();
            if action == "runtime_skills_update" && old["state"] == "succeeded" {
                let result: Value = serde_json::from_str(old["result"].as_str().context("skill update result")?)?;
                reply.insert("catalog".into(), result);
            }
            return operation_receipt(&Value::Object(reply));
        }
        db.conn.execute("INSERT INTO runtime_operations(id,runtime,action,args,state,created) VALUES(?,'local',?,?,'local_pending',?)",params![id,action,args.to_string(),now()])?;
        Ok(json!({"request_id":request,"state":"accepted"}))
    })
}

fn install_skills(db: &Store, op: &Value) -> Result<()> {
    let id = op["id"].as_str().context("operation id")?;
    db.atomic(|| {
        let pending: bool = db.conn.query_row(
            "SELECT state='local_pending' FROM runtime_operations WHERE id=?",
            [id],
            |row| row.get(0),
        )?;
        if !pending {
            return Ok(());
        }
        let args: Value = serde_json::from_str(op["args"].as_str().context("args")?)?;
        let packet: crate::skills::Packet = serde_json::from_value(args["packet"].clone())?;
        let (state, result) = match crate::skill_catalog::install(&db.root, &packet) {
            Ok(catalog) => ("succeeded", catalog),
            Err(error) => ("failed", json!({"error":error.to_string()})),
        };
        db.conn.execute(
            "UPDATE runtime_operations SET state=?,result=? WHERE id=? AND state='local_pending'",
            params![state, result.to_string(), id],
        )?;
        event(
            db,
            "runtime.skills_updated",
            json!({"request_id":id,"state":state,"result":result}),
        )
    })
}

pub fn local_commands(db: &Store) -> Result<()> {
    for op in db.rows("SELECT * FROM runtime_operations WHERE runtime='local' AND state IN ('local_pending','draining') ORDER BY rowid LIMIT 1",&[])? {
        let id=op["id"].as_str().context("operation id")?;
        let action=op["action"].as_str().context("action")?;
        if action == "runtime_skills_update" {
            install_skills(db, &op)?;
        } else if action=="runtime_update" {
            let args:Value=serde_json::from_str(op["args"].as_str().context("args")?)?;
            let log=std::fs::OpenOptions::new().create(true).append(true).open(db.root.join("update.log"))?;
            db.conn.execute("UPDATE runtime_operations SET state='running' WHERE id=?",[id])?;
            let child=std::process::Command::new(std::env::current_exe()?).arg("--data-dir").arg(&db.root).args(["update","--version",args["version"].as_str().context("version")?,"--operation",id]).stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log).spawn();
            if let Err(e)=child{db.conn.execute("UPDATE runtime_operations SET state='failed',result=? WHERE id=?",params![json!({"error":e.to_string()}).to_string(),id])?;}
        }else{
            set(db,"draining","true")?;
            db.conn.execute("UPDATE runtime_operations SET state='draining' WHERE id=?",[id])?;
            if status(db)?["drained"]==true {
                if value(db,"service_installed")?.as_deref()!=Some("true")&&crate::branding::var("HORDE_SUPERVISED").as_deref()!=Ok("1") {
                    db.conn.execute("UPDATE runtime_operations SET state='failed',result=? WHERE id=?",params![json!({"error":"remote restart requires a supervisor service"}).to_string(),id])?;continue;
                }
                db.conn.execute("UPDATE runtime_operations SET state='restarting' WHERE id=?",[id])?;
                set(db,"restart_requested","true")?;set(db,"draining","false")?;
                std::fs::write(db.root.join("shutdown.request"),b"restart")?;
            }else if op["created"].as_i64().unwrap_or(0)+1800<now(){db.conn.execute("UPDATE runtime_operations SET state='blocked' WHERE id=?",[id])?;}
        }
    }
    Ok(())
}
