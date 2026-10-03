//! Operator-only correction of a quiescent Run's frozen model selection.
use crate::{
    accounts,
    config::Settings,
    execution_selection, projects,
    store::{Store, hash, now},
};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde_json::{Value, json};
use std::io::Write;

pub fn rebind_model(
    db: &Store,
    task: &str,
    provider: &str,
    expected_settings_hash: &str,
    expected_project_config_hash: &str,
    key: &str,
) -> Result<Value> {
    accounts::identifier(task)?;
    accounts::identifier(provider)?;
    for digest in [expected_settings_hash, expected_project_config_hash] {
        ensure!(
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
            "expected hashes must be lowercase SHA256"
        );
    }
    ensure!(
        !key.is_empty() && key.len() <= 96 && key.bytes().all(|c| c.is_ascii_graphic()),
        "invalid idempotency_key"
    );
    let operation = format!("run-model-rebind:{key}");
    let project = projects::task_project(db, task)?;
    let request = json!({"project":project,"provider":provider,"expected_settings_hash":expected_settings_hash,
        "expected_project_config_hash":expected_project_config_hash});
    db.atomic(|| {
        if let Some(existing) = db.rows("SELECT state,data FROM external_ops WHERE task=? AND name=?", &[&task, &operation])?.first() {
            let data: Value = serde_json::from_str(existing["data"].as_str().context("model recovery receipt")?)?;
            ensure!(existing["state"] == "succeeded" && data["request"] == request, "idempotency key reused with different model recovery");
            return Ok(data["response"].clone());
        }
        let row = db.task(task)?;
        ensure!(["failed", "blocked"].contains(&row["status"].as_str().unwrap_or("")), "Run must be failed or stopped before model recovery");
        ensure!(execution_selection::policy(db, task)?.is_none(), "immutable execution selection cannot be rebound");
        quiescent(db, task)?;
        let raw = row["settings"].as_str().context("Run settings")?;
        ensure!(hash(raw.as_bytes()) == expected_settings_hash, "Run settings hash conflict");
        let original: Settings = serde_json::from_str(raw)?;
        let saved: Value = serde_json::from_str(raw)?;
        ensure!(saved["providers"][provider].is_object(), "Run lacks an explicit saved provider snapshot");
        let (configured, config_path) = configured(db, &project, expected_project_config_hash)?;
        let old = original.providers.get(provider).context("Run provider not configured")?;
        let current = configured.providers.get(provider).context("project provider not configured")?;
        let model = current.model.as_deref().context("project provider requires an explicit corrected model")?;
        ensure!(!model.is_empty() && model.len() <= 256 && model.bytes().all(|b| b.is_ascii_graphic()) && model != "auto", "project model must be an explicit bounded identifier");
        ensure!(old.model.as_deref() != Some(model), "Run already uses the configured model");
        let mut permitted = current.clone();
        permitted.model = old.model.clone();
        if current.account.is_none() { permitted.account = old.account.clone(); }
        ensure!(serde_json::to_value(&permitted)? == serde_json::to_value(old)?, "project provider differs beyond its model or retained account pin");
        let roles: Vec<_> = original.executors.iter().filter(|(_, e)| e.provider() == provider).map(|(role, _)| role.clone()).collect();
        ensure!(!roles.is_empty(), "Run has no role using this provider");
        for role in &roles {
            let executor = original.executors.get(role).context("Run role")?;
            ensure!(executor.model.is_none(), "Run role has an explicit immutable model override");
            let config = original.executor(role).context("Run executor")?;
            authenticated(db, &project, &config)?;
        }
        // Change the existing JSON value, preserving all other fields, including
        // submit-time pins, overlays and provenance not normalized by Settings.
        let mut changed: Value = serde_json::from_str(raw)?;
        changed["providers"][provider]["model"] = json!(model);
        let changed = serde_json::to_string(&changed)?;
        let _: Settings = serde_json::from_str(&changed)?;
        ensure!(hash(&std::fs::read(&config_path)?) == expected_project_config_hash, "project configuration changed during model recovery");
        ensure!(db.conn.execute("UPDATE tasks SET settings=? WHERE id=? AND settings=? AND status=?",
            params![changed, task, raw, row["status"].as_str().context("Run status")?])? == 1, "Run changed during model recovery");
        let response = json!({"task":task,"project":project,"provider":provider,"model_before":old.model,
            "model_after":model,"roles":roles,"settings_before":expected_settings_hash,
            "settings_after":hash(changed.as_bytes()),"project_config_hash":expected_project_config_hash,
            "idempotency_key":key,"resumed":false});
        db.conn.execute("INSERT INTO external_ops(task,name,state,data) VALUES(?,?,'succeeded',?)",
            params![task, operation, json!({"request":request,"response":response,"settings_before_raw":raw}).to_string()])?;
        db.event(task, "run.model_rebound", response.clone())?;
        Ok(response)
    })
}

fn authenticated(db: &Store, project: &str, config: &crate::config::ExecutorConfig) -> Result<()> {
    let eligible: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM accounts a JOIN account_grants g ON g.account=a.id JOIN auth_profiles p ON p.account=a.id WHERE g.project=? AND a.provider=? AND a.auth_mode=? AND a.base_url=? AND (? IS NULL OR a.id=?) AND a.state='active' AND a.authenticated=1 AND p.credential_version>0 AND (p.expires_at IS NULL OR p.expires_at>?))",
        params![project, config.kind, config.auth_mode, config.base_url, config.account, config.account, now()], |r| r.get(0))?;
    ensure!(
        eligible,
        "retained executor has no current authenticated project account grant"
    );
    Ok(())
}

fn configured(db: &Store, project: &str, expected: &str) -> Result<(Settings, std::path::PathBuf)> {
    let directory = if project == projects::DEFAULT_PROJECT {
        db.user_config_dir()
    } else {
        projects::storage_root(db, project)?
    };
    let path = directory.join("config.toml");
    ensure!(
        std::fs::metadata(&path)?.len() <= 1024 * 1024,
        "project configuration exceeds 1 MiB"
    );
    let bytes = std::fs::read(&path)?;
    ensure!(
        hash(&bytes) == expected,
        "project configuration hash conflict"
    );
    // Use the same owning configuration loader on an immutable private snapshot,
    // avoiding mixed reads when project_configure atomically replaces its file.
    let staged = tempfile::tempdir()?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(staged.path().join("config.toml"))?
        .write_all(&bytes)?;
    Ok((Settings::load_dir(staged.path())?, path))
}

fn quiescent(db: &Store, task: &str) -> Result<()> {
    for (sql, message) in [
        (
            "SELECT EXISTS(SELECT 1 FROM remote_links WHERE task=?)",
            "remote Run cannot be rebound locally",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM attempts a JOIN steps s ON s.id=a.step WHERE s.task=? AND a.state NOT IN ('failed','succeeded','cancelled'))",
            "Run has active or uncertain attempts",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM workers WHERE task=? AND status NOT IN ('failed','stopped','operator'))",
            "Run has active or uncertain workers",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM steps WHERE task=? AND state IN ('running','waiting','uncertain','succeeded'))",
            "Run has running or completed work",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM claims WHERE task=?)",
            "Run has outstanding workspace claims",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM questions WHERE task=? AND answer IS NULL)",
            "Run has pending questions",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM account_reservations r JOIN steps s ON s.id=r.step WHERE s.task=? AND r.state IN ('active','revoked','uncertain'))",
            "Run has unresolved account reservations",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM account_remote_reservations WHERE task=? AND state IN ('active','revoked','uncertain'))",
            "Run has unresolved remote account reservations",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM project_remote_reservations WHERE task=? AND state IN ('active','revoked','uncertain'))",
            "Run has unresolved project reservations",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM integrations WHERE task=?)",
            "Run has integration evidence",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM child_acceptance WHERE parent=?1 OR child=?1)",
            "Run has accepted child work",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM task_tree tt JOIN tasks t ON t.id=tt.task WHERE tt.parent=? AND t.status NOT IN ('failed','cancelled','succeeded'))",
            "Run has active child work",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM preview_jobs WHERE task=?)",
            "Run has preview evidence",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM delivery_authorizations WHERE task=?)",
            "Run has delivery authorization",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM external_ops WHERE task=? AND state NOT IN ('succeeded','failed','cancelled'))",
            "Run has an unresolved external operation",
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM events WHERE task=? AND kind IN ('run.checkpoint_verified','run.branch_published'))",
            "Run has a verified checkpoint or publication",
        ),
    ] {
        let blocked: bool = db.conn.query_row(sql, [task], |r| r.get(0))?;
        ensure!(!blocked, "{message}");
    }
    Ok(())
}
