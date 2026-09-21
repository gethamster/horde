//! Controller-owned remote reservations survive loss of the worker connection.
use super::*;
#[allow(clippy::too_many_arguments)]
pub fn reserve_remote(
    db: &Store,
    project: &str,
    task: &str,
    runtime: &str,
    attempt: &str,
    request_id: &str,
    settings: &Settings,
    role: &str,
) -> Result<Option<Binding>> {
    identifier(runtime)?;
    identifier(attempt)?;
    identifier(request_id)?;
    crate::projects::authorize_task(db, project, task)?;
    ensure!(
        crate::projects::runtime_allowed(db, project, runtime)?,
        "runtime is not granted to project"
    );
    db.atomic(|| {
        if let Some(old) = db
            .rows(
                "SELECT * FROM account_remote_reservations WHERE request_id=?",
                &[&request_id],
            )?
            .first()
        {
            ensure!(
                old["project"] == project
                    && old["task"] == task
                    && old["runtime"] == runtime
                    && old["attempt"] == attempt
                    && old["role"] == role,
                "remote reservation identity changed"
            );
            ensure!(
                old["state"] == "active",
                "remote reservation no longer active; reconciliation required"
            );
            let account = old["account"].as_str().context("account missing")?;
            authorized(db, project, account)?;
            ensure!(
                old["credential_version"].as_i64() == Some(credential_version(db, account)?),
                "credential replaced; reconcile remote reservation"
            );
            return Ok(Some(Binding {
                role: role.into(),
                account: Some(account.into()),
                profile: old["profile"].as_str().map(str::to_owned),
                credential_version: old["credential_version"].as_i64(),
            }));
        }
        let config = remote_config(db, project, task, runtime, settings, role)?;
        if config.kind == "simulated" {
            return Ok(Some(Binding {
                role: role.into(),
                account: None,
                profile: None,
                credential_version: None,
            }));
        }
        let Some(account) = select_account(db, project, &config)? else {
            return Ok(None);
        };
        let (profile, version) = db.conn.query_row(
            "SELECT id,credential_version FROM auth_profiles WHERE account=?",
            [&account],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )?;
        db.conn.execute(
            "INSERT INTO account_remote_reservations VALUES(?,?,?,?,?,?,?,?,?,'active',?)",
            params![
                request_id,
                project,
                account,
                runtime,
                task,
                attempt,
                role,
                profile,
                version,
                now()
            ],
        )?;
        db.conn.execute(
            "UPDATE accounts SET last_dispatch=? WHERE id=?",
            params![now(), account],
        )?;
        Ok(Some(Binding {
            role: role.into(),
            account: Some(account),
            profile: Some(profile),
            credential_version: Some(version),
        }))
    })
}
/// Only call after an authenticated worker positively reports process termination.
pub fn release_remote(db: &Store, runtime: &str, request_id: &str) -> Result<()> {
    let changed = db.conn.execute(
        "UPDATE account_remote_reservations SET state='released' WHERE runtime=? AND request_id=?",
        params![runtime, request_id],
    )?;
    ensure!(
        changed == 1,
        "remote reservation missing or belongs to another runtime"
    );
    Ok(())
}
pub fn remote_reservation(db: &Store, runtime: &str, request_id: &str) -> Result<Value> {
    db.rows(
        "SELECT * FROM account_remote_reservations WHERE runtime=? AND request_id=?",
        &[&runtime, &request_id],
    )?
    .into_iter()
    .next()
    .context("remote reservation missing")
}
/// Peer loss retains capacity. Operators must reconcile the remote process before releasing it.
pub fn mark_runtime_uncertain(db: &Store, runtime: &str) -> Result<()> {
    db.conn.execute("UPDATE project_remote_reservations SET state='uncertain' WHERE runtime=? AND state='active'",[runtime])?;
    db.conn.execute("UPDATE account_remote_reservations SET state='uncertain' WHERE runtime=? AND state='active'",[runtime])?;
    Ok(())
}

/// Reserve every remote invocation, including commands and simulated executors.
pub fn reserve_project_remote(
    db: &Store,
    project: &str,
    task: &str,
    runtime: &str,
    request_id: &str,
) -> Result<bool> {
    identifier(runtime)?;
    identifier(request_id)?;
    crate::projects::authorize_task(db, project, task)?;
    ensure!(
        crate::projects::runtime_allowed(db, project, runtime)?,
        "runtime is not granted to project"
    );
    db.atomic(|| {
        if let Some(old) = db
            .rows(
                "SELECT * FROM project_remote_reservations WHERE request_id=?",
                &[&request_id],
            )?
            .first()
        {
            ensure!(
                old["project"] == project && old["task"] == task && old["runtime"] == runtime,
                "project reservation identity changed"
            );
            ensure!(
                old["state"] == "active",
                "project reservation is no longer active; reconcile before retrying"
            );
            return Ok(true);
        }
        let limit: i64 = db.conn.query_row(
            "SELECT concurrency FROM projects WHERE id=?",
            [project],
            |r| r.get(0),
        )?;
        if crate::project_runtime::active(db, project)? >= limit {
            return Ok(false);
        }
        db.conn.execute(
            "INSERT INTO project_remote_reservations VALUES(?,?,?,?,'active',?)",
            params![request_id, project, task, runtime, now()],
        )?;
        Ok(true)
    })
}
/// The authority releases only after the bound worker reports a terminated invocation.
pub fn release_project_remote(
    db: &Store,
    project: &str,
    task: &str,
    runtime: &str,
    request_id: &str,
) -> Result<()> {
    identifier(runtime)?;
    identifier(request_id)?;
    crate::projects::authorize_task(db, project, task)?;
    db.atomic(|| {
        if let Some(old) = db
            .rows(
                "SELECT project,task,runtime FROM project_remote_reservations WHERE request_id=?",
                &[&request_id],
            )?
            .first()
        {
            ensure!(
                old["project"] == project && old["task"] == task && old["runtime"] == runtime,
                "project reservation identity mismatch"
            );
            db.conn.execute(
                "UPDATE project_remote_reservations SET state='released' WHERE request_id=?",
                [request_id],
            )?;
        } else {
            // A cancelled request can arrive before its delayed acquire. Preserve cancellation.
            db.conn.execute(
                "INSERT INTO project_remote_reservations VALUES(?,?,?,?,'released',?)",
                params![request_id, project, task, runtime, now()],
            )?;
        }
        Ok(())
    })
}

fn remote_config(
    db: &Store,
    project: &str,
    task: &str,
    runtime: &str,
    settings: &Settings,
    role: &str,
) -> Result<ExecutorConfig> {
    let config = settings.executor(role).context("unknown executor role")?;
    let Some(policy) = crate::execution_selection::policy(db, task)? else {
        return Ok(config);
    };
    let selected = &policy["selected"];
    if selected.is_null() {
        return Ok(config);
    }
    ensure!(
        selected["runtime"] == runtime,
        "account request targets a different selected runtime"
    );
    let kind = selected["kind"]
        .as_str()
        .context("selected remote provider kind missing")?;
    let model = selected["model"].as_str().map(str::to_owned);
    if kind == "simulated" {
        return Ok(ExecutorConfig {
            kind: kind.into(),
            account: None,
            model,
            ..config
        });
    }
    let (auth_mode, base_url, account) = if let Some(account) = selected["account"].as_str() {
        authorized(db, project, account)?;
        let (provider, auth_mode, base_url) = db.conn.query_row(
            "SELECT provider,auth_mode,base_url FROM accounts WHERE id=?",
            [account],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )?;
        ensure!(
            provider == kind,
            "selected account does not match remote provider kind"
        );
        ensure!(
            selected["auth_mode"].is_null() || selected["auth_mode"] == auth_mode,
            "selected account authentication differs from remote capability"
        );
        ensure!(
            selected["base_url"].is_null() || selected["base_url"] == base_url,
            "selected account endpoint differs from remote capability"
        );
        ensure!(
            selected["endpoint_hash"].is_null()
                || selected["endpoint_hash"] == crate::store::hash(base_url.as_bytes()),
            "selected account endpoint differs from remote capability"
        );
        (auth_mode, base_url, Some(account.to_owned()))
    } else {
        let auth_mode = selected["auth_mode"]
            .as_str()
            .context("remote capability lacks authentication metadata; refresh its capabilities")?;
        let base_url = if let Some(hash) = selected["endpoint_hash"].as_str() {
            db.rows("SELECT DISTINCT a.base_url FROM accounts a JOIN account_grants g ON g.account=a.id WHERE g.project=? AND a.provider=? AND a.auth_mode=?", &[&project,&kind,&auth_mode])?.into_iter()
                .filter_map(|row| row["base_url"].as_str().map(str::to_owned))
                .find(|url| crate::store::hash(url.as_bytes()) == hash)
                .context("no granted account matches the remote provider endpoint")?
        } else {
            selected["base_url"]
                .as_str()
                .context("remote capability lacks endpoint metadata; refresh its capabilities")?
                .to_owned()
        };
        (auth_mode.to_owned(), base_url, None)
    };
    Ok(ExecutorConfig {
        kind: kind.into(),
        auth_mode,
        base_url,
        account,
        model,
        ..config
    })
}
