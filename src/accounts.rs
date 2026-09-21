//! Explicit quota identities and project grants. Secret material lives outside SQLite.
use crate::{
    config::{ExecutorConfig, Settings},
    store::{Store, now},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
mod credentials;
mod remote;
pub use credentials::*;
pub use remote::*;

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS accounts(id TEXT PRIMARY KEY,owner_project TEXT NOT NULL REFERENCES projects(id),name TEXT NOT NULL,provider TEXT NOT NULL,auth_mode TEXT NOT NULL,base_url TEXT NOT NULL,concurrency INTEGER NOT NULL CHECK(concurrency>0),authenticated INTEGER NOT NULL DEFAULT 0,state TEXT NOT NULL DEFAULT 'active',last_dispatch INTEGER NOT NULL DEFAULT 0,UNIQUE(owner_project,name));
CREATE TABLE IF NOT EXISTS auth_profiles(id TEXT PRIMARY KEY,account TEXT UNIQUE NOT NULL REFERENCES accounts(id),credential_version INTEGER NOT NULL DEFAULT 0,kind TEXT,expires_at INTEGER);
CREATE TABLE IF NOT EXISTS account_grants(project TEXT NOT NULL REFERENCES projects(id),account TEXT NOT NULL REFERENCES accounts(id),created INTEGER NOT NULL,PRIMARY KEY(project,account));
CREATE TABLE IF NOT EXISTS account_grant_revocations(project TEXT NOT NULL REFERENCES projects(id),account TEXT NOT NULL REFERENCES accounts(id),version INTEGER NOT NULL,PRIMARY KEY(project,account));
CREATE TABLE IF NOT EXISTS account_reservations(step TEXT PRIMARY KEY REFERENCES steps(id),project TEXT NOT NULL REFERENCES projects(id),account TEXT NOT NULL REFERENCES accounts(id),profile TEXT NOT NULL REFERENCES auth_profiles(id),credential_version INTEGER NOT NULL,state TEXT NOT NULL,created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS project_remote_reservations(request_id TEXT PRIMARY KEY,project TEXT NOT NULL REFERENCES projects(id),task TEXT NOT NULL REFERENCES tasks(id),runtime TEXT NOT NULL,state TEXT NOT NULL,created INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS account_remote_reservations(request_id TEXT PRIMARY KEY,project TEXT NOT NULL REFERENCES projects(id),account TEXT NOT NULL REFERENCES accounts(id),runtime TEXT NOT NULL,task TEXT NOT NULL,attempt TEXT NOT NULL,role TEXT NOT NULL,profile TEXT NOT NULL,credential_version INTEGER NOT NULL,state TEXT NOT NULL,created INTEGER NOT NULL,UNIQUE(runtime,attempt));
CREATE VIEW IF NOT EXISTS account_allocations AS SELECT account,state FROM account_reservations UNION ALL SELECT account,state FROM account_remote_reservations;
CREATE TABLE IF NOT EXISTS credential_deliveries(request_id TEXT PRIMARY KEY,project TEXT NOT NULL REFERENCES projects(id),account TEXT NOT NULL REFERENCES accounts(id),runtime TEXT NOT NULL,version INTEGER NOT NULL,state TEXT NOT NULL,created INTEGER NOT NULL);
CREATE TRIGGER IF NOT EXISTS immutable_account_identity BEFORE UPDATE OF id,owner_project,provider,auth_mode,base_url ON accounts BEGIN SELECT RAISE(ABORT,'account identity is immutable'); END;")?;
    Ok(())
}
pub fn identifier(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
        "invalid resource identifier"
    );
    Ok(())
}
pub fn profile_directory(root: &Path, project: &str, account: &str) -> Result<PathBuf> {
    identifier(project)?;
    identifier(account)?;
    Ok(root
        .join("projects")
        .join(project)
        .join("accounts")
        .join(account))
}
pub fn authorized(db: &Store, project: &str, account: &str) -> Result<()> {
    ensure!(db.conn.query_row("SELECT COUNT(*) FROM account_grants g JOIN accounts a ON a.id=g.account WHERE g.project=? AND g.account=? AND a.state='active'",params![project,account],|r|r.get::<_,i64>(0))?==1,"account is not granted to this project");
    Ok(())
}
pub fn validate_account(db: &Store, project: &str, config: &ExecutorConfig) -> Result<bool> {
    let Some(id) = config.account.as_deref() else {
        ensure!(
            project == "default" || config.kind == "simulated",
            "project requires an explicit managed account"
        );
        return Ok(false);
    };
    let exists: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?)",
        [id],
        |r| r.get(0),
    )?;
    if !exists {
        ensure!(project == "default", "unknown account");
        return Ok(false);
    }
    authorized(db, project, id)?;
    let matches: bool = db.conn.query_row(
        "SELECT provider=? AND auth_mode=? AND base_url=? FROM accounts WHERE id=?",
        params![config.kind, config.auth_mode, config.base_url, id],
        |r| r.get(0),
    )?;
    ensure!(
        matches,
        "account provider does not match executor configuration"
    );
    Ok(true)
}
fn argument<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("{key} required"))
}
fn inspect(db: &Store, project: &str, account: &str) -> Result<Value> {
    authorized(db, project, account)?;
    db.rows("SELECT a.*,p.id AS profile,p.credential_version,p.kind AS credential_kind,p.expires_at,(SELECT COUNT(*) FROM account_allocations r WHERE r.account=a.id AND r.state IN ('active','revoked','uncertain')) AS active FROM accounts a JOIN auth_profiles p ON p.account=a.id WHERE a.id=?",&[&account])?.into_iter().next().context("account missing")
}
pub fn dispatch(db: &Store, name: &str, args: &Value) -> Result<Option<Value>> {
    if !name.starts_with("account_") {
        return Ok(None);
    }
    let project = crate::projects::resolve(db, args["project"].as_str().unwrap_or("default"))?;
    let value = match name {
        "account_create" => {
            let name = argument(args, "name")?;
            identifier(name)?;
            let provider = argument(args, "provider")?;
            identifier(provider)?;
            let auth_mode = args["auth_mode"].as_str().unwrap_or("api");
            ensure!(
                ["api", "login"].contains(&auth_mode),
                "unsupported authentication mode"
            );
            let limit = args["concurrency"].as_u64().unwrap_or(1);
            ensure!(limit > 0 && limit <= 4096, "invalid account concurrency");
            let base_url = args["base_url"].as_str().unwrap_or("");
            if !base_url.is_empty() {
                let url = reqwest::Url::parse(base_url)?;
                ensure!(
                    matches!(url.scheme(), "http" | "https")
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.query().is_none()
                        && url.fragment().is_none(),
                    "invalid account endpoint"
                );
            }
            let id = crate::store::id();
            let profile = crate::store::id();
            db.atomic(||{db.conn.execute("INSERT INTO accounts(id,owner_project,name,provider,auth_mode,base_url,concurrency) VALUES(?,?,?,?,?,?,?)",params![id,project,name,provider,auth_mode,base_url,limit])?;
                db.conn.execute("INSERT INTO auth_profiles(id,account) VALUES(?,?)",params![profile,id])?;
                db.conn.execute("INSERT INTO account_grants VALUES(?,?,?)",params![project,id,now()])?;Ok(())})?;
            inspect(db, &project, &id)?
        }
        "account_usage" => report_project(db, &project)?,
        "account_list" => {
            json!({"accounts":db.rows("SELECT a.*,p.id AS profile,p.credential_version,p.kind AS credential_kind,p.expires_at FROM accounts a JOIN account_grants g ON g.account=a.id JOIN auth_profiles p ON p.account=a.id WHERE g.project=? ORDER BY a.name,a.id",&[&project])?})
        }
        "account_inspect" => inspect(db, &project, argument(args, "account")?)?,
        "account_grant" => {
            let account = argument(args, "account")?;
            db.atomic(|| {
                let generation:i64=db.conn.query_row("SELECT MAX(?,COALESCE((SELECT version+1 FROM account_grant_revocations WHERE project=? AND account=?),0))",params![now(),project,account],|r|r.get(0))?;
                db.conn.execute("DELETE FROM account_grant_revocations WHERE project=? AND account=?",params![project,account])?;
                db.conn.execute(
                    "INSERT OR IGNORE INTO account_grants VALUES(?,?,?)",
                    params![project, account, generation],
                )?;
                Ok(())
            })?;
            inspect(db, &project, account)?
        }
        "account_revoke" => {
            let account = argument(args, "account")?;
            authorized(db, &project, account)?;
            db.atomic(||{db.conn.execute("INSERT OR REPLACE INTO account_grant_revocations SELECT project,account,created FROM account_grants WHERE project=? AND account=?",params![project,account])?;
                db.conn.execute("DELETE FROM account_grants WHERE project=? AND account=?",params![project,account])?;
                db.conn.execute("UPDATE account_reservations SET state='revoked' WHERE project=? AND account=? AND state='active'",params![project,account])?;
                db.conn.execute("UPDATE account_remote_reservations SET state='revoked' WHERE project=? AND account=? AND state='active'",params![project,account])?;
                db.conn.execute("UPDATE credential_deliveries SET state='revocation_pending' WHERE project=? AND account=? AND state='delivered'",params![project,account])?;Ok(())})?;
            json!({"project":project,"account":account,"revoked":true,"reconciliation":"required"})
        }
        "account_credential_set" => {
            let account = argument(args, "account")?;
            let path = argument(args, "credential_file")?;
            let bytes = std::fs::read(path).context("cannot read credential file")?;
            ensure!(bytes.len() <= 64 * 1024, "credential file too large");
            let credential: Credential = serde_json::from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("invalid credential file"))?;
            json!({"account":account,"credential_version":set_credential(db,&project,account,&credential)?})
        }
        "account_delivery_list" => {
            let account = argument(args, "account")?;
            authorized(db, &project, account)?;
            json!({"deliveries":db.rows("SELECT * FROM credential_deliveries WHERE project=? AND account=? ORDER BY created",&[&project,&account])?})
        }
        _ => return Ok(None),
    };
    Ok(Some(value))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Binding {
    pub role: String,
    pub account: Option<String>,
    pub profile: Option<String>,
    pub credential_version: Option<i64>,
}
/// Read-only candidate choice; the caller must reserve inside its dispatch transaction.
pub fn select_account(
    db: &Store,
    project: &str,
    config: &ExecutorConfig,
) -> Result<Option<String>> {
    let rows=db.rows("SELECT a.id,a.concurrency,a.last_dispatch,p.expires_at,(SELECT COUNT(*) FROM account_allocations r WHERE r.account=a.id AND r.state IN ('active','revoked','uncertain')) AS active FROM accounts a JOIN account_grants g ON g.account=a.id JOIN auth_profiles p ON p.account=a.id WHERE g.project=? AND a.provider=? AND a.auth_mode=? AND a.base_url=? AND a.state='active' AND a.authenticated=1 AND p.credential_version>0 ORDER BY CAST((SELECT COUNT(*) FROM account_allocations r WHERE r.account=a.id AND r.state IN ('active','revoked','uncertain')) AS REAL)/a.concurrency,a.last_dispatch,a.id",&[&project,&config.kind,&config.auth_mode,&config.base_url])?;
    for row in rows {
        let id = row["id"].as_str().context("account id missing")?;
        if config.account.as_deref().is_some_and(|pin| pin != id)
            || row["expires_at"].as_i64().is_some_and(|t| t <= now())
            || row["active"].as_i64() >= row["concurrency"].as_i64()
        {
            continue;
        }
        if crate::capacity::available(db, id)? {
            return Ok(Some(id.into()));
        }
    }
    Ok(None)
}
pub fn choose_for_step(
    db: &Store,
    task: &str,
    settings: &Settings,
    role: &str,
    step: &str,
) -> Result<Option<Binding>> {
    let project = crate::projects::task_project(db, task)?;
    let failures: i64 = db.conn.query_row(
        "SELECT COUNT(*) FROM attempts WHERE step=? AND state='failed'",
        [step],
        |r| r.get(0),
    )?;
    let mut role = role;
    for _ in 0..failures {
        if let Some(next) = settings.fallbacks.get(role) {
            role = next
        } else {
            break;
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    while seen.insert(role) {
        let Some(config) = settings.executor(role) else {
            return Ok(Some(Binding {
                role: role.into(),
                account: None,
                profile: None,
                credential_version: None,
            }));
        };
        if let Some(account) = select_account(db, &project, &config)? {
            let (profile, version) = db.conn.query_row(
                "SELECT id,credential_version FROM auth_profiles WHERE account=?",
                [&account],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )?;
            return Ok(Some(Binding {
                role: role.into(),
                account: Some(account),
                profile: Some(profile),
                credential_version: Some(version),
            }));
        }
        let managed:bool=db.conn.query_row("SELECT EXISTS(SELECT 1 FROM accounts a WHERE (a.owner_project=? OR EXISTS(SELECT 1 FROM account_grants g WHERE g.account=a.id AND g.project=?)) AND a.provider=? AND a.auth_mode=? AND a.base_url=?)",params![project,project,config.kind,config.auth_mode,config.base_url],|r|r.get(0))?;
        let pinned_managed = if let Some(pin) = &config.account {
            db.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?)",
                [pin],
                |r| r.get(0),
            )?
        } else {
            false
        };
        if config.kind == "simulated"
            || (project == "default"
                && !managed
                && !pinned_managed
                && crate::capacity::available(db, &crate::capacity::account(&config))?)
        {
            return Ok(Some(Binding {
                role: role.into(),
                account: config.account,
                profile: None,
                credential_version: None,
            }));
        }
        match settings.fallbacks.get(role) {
            Some(next) => role = next,
            None => return Ok(None),
        }
    }
    Ok(None)
}
pub fn reserve(db: &Store, step: &str, binding: &Binding) -> Result<()> {
    let (Some(account), Some(profile), Some(version)) = (
        &binding.account,
        &binding.profile,
        binding.credential_version,
    ) else {
        return Ok(());
    };
    let task: String = db
        .conn
        .query_row("SELECT task FROM steps WHERE id=?", [step], |r| r.get(0))?;
    let project = crate::projects::task_project(db, &task)?;
    db.atomic(||{
        authorized(db,&project,account)?;
        if let Some(existing)=db.conn.query_row("SELECT account,profile,credential_version,state FROM account_reservations WHERE step=?",[step],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?))).optional()? {
            if existing.3=="active"{ensure!(existing.0==*account&&existing.1==*profile&&existing.2==version,"reservation already exists");return Ok(())}
            ensure!(existing.3=="released","revoked reservation requires reconciliation");
        }
        let eligible:bool=db.conn.query_row("SELECT a.authenticated=1 AND p.credential_version=? AND (p.expires_at IS NULL OR p.expires_at>?) AND (SELECT COUNT(*) FROM account_allocations r WHERE r.account=a.id AND r.state IN ('active','revoked','uncertain'))<a.concurrency FROM accounts a JOIN auth_profiles p ON p.account=a.id WHERE a.id=? AND p.id=?",params![version,now(),account,profile],|r|r.get(0))?;
        ensure!(eligible&&crate::capacity::available(db,account)?,"account capacity or authentication unavailable");
        db.conn.execute("INSERT INTO account_reservations VALUES(?,?,?,?,?,'active',?) ON CONFLICT(step) DO UPDATE SET project=excluded.project,account=excluded.account,profile=excluded.profile,credential_version=excluded.credential_version,state=excluded.state,created=excluded.created",params![step,project,account,profile,version,now()])?;
        db.conn.execute("UPDATE accounts SET last_dispatch=? WHERE id=?",params![now(),account])?;Ok(())
    })
}
pub fn release_step(db: &Store, step: &str) -> Result<()> {
    let live: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts WHERE step=? AND state IN ('running','uncertain'))",
        [step],
        |r| r.get(0),
    )?;
    ensure!(
        !live,
        "cannot release a running or uncertain account reservation"
    );
    db.conn.execute(
        "UPDATE account_reservations SET state='released' WHERE step=?",
        [step],
    )?;
    Ok(())
}
pub fn apply_binding(config: &ExecutorConfig, binding: &Binding) -> ExecutorConfig {
    ExecutorConfig {
        account: binding.account.clone(),
        ..config.clone()
    }
}

/// Reports only granted account observations and this project's attempt totals.
pub fn report_project(db: &Store, project: &str) -> Result<Value> {
    let project = crate::projects::resolve(db, project)?;
    let mut entries = Vec::new();
    for row in db.rows("SELECT a.*,p.credential_version,p.expires_at FROM accounts a JOIN account_grants g ON g.account=a.id JOIN auth_profiles p ON p.account=a.id WHERE g.project=? ORDER BY a.name,a.id",&[&project])? {
        let account=row["id"].as_str().context("account id")?;
        let mut entry=row.clone();
        entry["windows"]=json!(db.rows("SELECT * FROM account_capacity WHERE account=? ORDER BY window",&[&account])?);
        entry["quota_eligible"]=json!(crate::capacity::available(db,account)?);
        entry["active"]=json!(db.conn.query_row("SELECT COUNT(*) FROM account_allocations WHERE account=? AND state IN ('active','revoked','uncertain')",[account],|r|r.get::<_,i64>(0))?);
        let usage=db.rows("SELECT a.usage FROM attempts a JOIN attempt_accounts c ON c.attempt=a.id JOIN steps s ON s.id=a.step JOIN task_projects p ON p.task=s.task WHERE p.project=? AND c.account=?",&[&project,&account])?;
        entry["project_usage"]=totals(&usage);
        entries.push(entry);
    }
    let attempts=db.rows("SELECT a.usage FROM attempts a JOIN steps s ON s.id=a.step JOIN task_projects p ON p.task=s.task WHERE p.project=?",&[&project])?;
    let tasks=db.rows("SELECT t.status,COUNT(*) AS count FROM tasks t JOIN task_projects p ON p.task=t.id WHERE p.project=? GROUP BY t.status",&[&project])?;
    Ok(json!({"project":project,"accounts":entries,"tasks":tasks,"usage":totals(&attempts)}))
}
fn totals(rows: &[Value]) -> Value {
    let (mut tokens, mut usd, mut unknown_tokens, mut unknown_cost) = (0u64, 0.0, 0usize, 0usize);
    for row in rows {
        let usage = row["usage"]
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        let (t, c) = crate::metrics::totals(&usage);
        match t {
            Some(value) => tokens = tokens.saturating_add(value),
            None => unknown_tokens += 1,
        };
        match c {
            Some(value) => usd += value,
            None => unknown_cost += 1,
        };
    }
    json!({"attempts":rows.len(),"reported_tokens":tokens,"reported_usd":usd,"unknown_token_attempts":unknown_tokens,"unknown_cost_attempts":unknown_cost})
}
