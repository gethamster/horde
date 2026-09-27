use crate::{
    accounts,
    config::Settings,
    execution_selection, projects,
    store::{Store, hash, now},
};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde_json::{Value, json};

/// Release a stopped Run's submit-time account pin so its next agent step can
/// choose from the project's currently granted subscription pool.
pub fn unpin_account(db: &Store, task: &str, account: &str, key: &str) -> Result<Value> {
    accounts::identifier(account)?;
    ensure!(
        !key.is_empty() && key.len() <= 96 && key.bytes().all(|byte| byte.is_ascii_graphic()),
        "invalid idempotency_key"
    );
    let operation = format!("run-account-unpin:{key}");
    let request = json!({"account":account});
    db.atomic(|| {
        if let Some(existing) = db
            .rows(
                "SELECT data FROM external_ops WHERE task=? AND name=?",
                &[&task, &operation],
            )?
            .into_iter()
            .next()
        {
            let data: Value = serde_json::from_str(
                existing["data"].as_str().context("account unpin receipt")?,
            )?;
            ensure!(data["request"] == request, "idempotency key reused with different account");
            return Ok(data["response"].clone());
        }
        let row = db.task(task)?;
        ensure!(
            ["running", "blocked", "failed", "succeeded"].contains(&row["status"].as_str().unwrap_or("")),
            "Run is not in a rebindable state"
        );
        ensure!(
            db.rows("SELECT task FROM remote_links WHERE task=?", &[&task])?.is_empty(),
            "remote Run account selection cannot be changed locally"
        );
        let live: i64 = db.conn.query_row(
            "SELECT COUNT(*) FROM attempts a JOIN steps s ON s.id=a.step WHERE s.task=? AND a.state IN ('running','uncertain')",
            [task],
            |r| r.get(0),
        )?;
        let active_workers: i64 = db.conn.query_row(
            "SELECT COUNT(*) FROM workers WHERE task=? AND status IN ('working','uncertain')",
            [task],
            |r| r.get(0),
        )?;
        ensure!(live == 0 && active_workers == 0, "Run has an active or uncertain attempt");
        let held: i64 = db.conn.query_row(
            "SELECT COUNT(*) FROM account_reservations r JOIN steps s ON s.id=r.step WHERE s.task=? AND r.state IN ('active','revoked','uncertain')",
            [task],
            |r| r.get(0),
        )?;
        let remote_held: i64 = db.conn.query_row(
            "SELECT COUNT(*) FROM account_remote_reservations WHERE task=? AND state IN ('active','revoked','uncertain')",
            [task],
            |r| r.get(0),
        )?;
        let project_held: i64 = db.conn.query_row(
            "SELECT COUNT(*) FROM project_remote_reservations WHERE task=? AND state IN ('active','revoked','uncertain')",
            [task],
            |r| r.get(0),
        )?;
        ensure!(held == 0 && remote_held == 0 && project_held == 0, "Run has an unresolved account reservation");
        ensure!(
            execution_selection::policy(db, task)?.is_none_or(|policy| policy["selected"].get("account").is_none()),
            "immutable execution selection pins the account"
        );
        let project = projects::task_project(db, task)?;
        let raw = row["settings"].as_str().context("Run settings")?;
        let mut settings: Settings = serde_json::from_str(raw)?;
        let roles: Vec<String> = settings
            .executors
            .keys()
            .filter(|role| settings.executor(role).is_some_and(|config| config.account.as_deref() == Some(account)))
            .cloned()
            .collect();
        ensure!(!roles.is_empty(), "Run no longer pins that account");
        for role in &roles {
            let config = settings.executor(role).context("pinned executor")?;
            let alternate: i64 = db.conn.query_row(
                "SELECT COUNT(*) FROM accounts a JOIN account_grants g ON g.account=a.id JOIN auth_profiles p ON p.account=a.id WHERE g.project=? AND a.id<>? AND a.provider=? AND a.auth_mode=? AND a.base_url=? AND a.state='active' AND a.authenticated=1 AND p.credential_version>0 AND (p.expires_at IS NULL OR p.expires_at>?)",
                params![project, account, config.kind, config.auth_mode, config.base_url, now()],
                |r| r.get(0),
            )?;
            ensure!(alternate > 0, "no alternate authenticated account is granted to this project");
        }
        for executor in settings.executors.values_mut() {
            if executor.account.as_deref() == Some(account) {
                executor.account = None;
            }
        }
        for provider in settings.providers.values_mut() {
            if provider.account.as_deref() == Some(account) {
                provider.account = None;
            }
        }
        for role in &roles {
            ensure!(
                settings.executor(role).is_some_and(|config| config.account.is_none()),
                "account pin remains after rebind"
            );
        }
        let changed = serde_json::to_string(&settings)?;
        let updated = db.conn.execute(
            "UPDATE tasks SET settings=? WHERE id=? AND settings=?",
            params![changed, task, raw],
        )?;
        ensure!(updated == 1, "Run settings changed during account rebind");
        let response = json!({"task":task,"project":project,"unpin_account":account,
            "roles":roles,"settings_before":hash(raw.as_bytes()),
            "settings_after":hash(changed.as_bytes()),"idempotency_key":key});
        db.conn.execute("INSERT INTO external_ops(task,name,state,data) VALUES(?,?,'succeeded',?)",
            params![task,operation,json!({"request":request,"response":response}).to_string()])?;
        db.event(task, "run.account_unpinned", response.clone())?;
        Ok(response)
    })
}
