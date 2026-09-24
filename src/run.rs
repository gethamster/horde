//! Branch-based Runs, exact checkpoint verification, and safe remote reconciliation.
use crate::{
    git,
    store::{Store, id},
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const RUN_CHECKPOINT_DOMAIN: &[u8] = b"horde-run-checkpoint-v1\0";

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS run_bindings(
        task TEXT PRIMARY KEY REFERENCES tasks(id),
        project TEXT NOT NULL REFERENCES projects(id),
        tenant_id TEXT NOT NULL,
        thread_id TEXT,
        brief_id TEXT,
        created INTEGER NOT NULL
    );
    CREATE TRIGGER IF NOT EXISTS run_binding_immutable BEFORE UPDATE ON run_bindings
    BEGIN SELECT RAISE(ABORT,'Run identity is immutable'); END;
    CREATE TRIGGER IF NOT EXISTS run_binding_delete_immutable BEFORE DELETE ON run_bindings
    BEGIN SELECT RAISE(ABORT,'Run identity is immutable'); END;",
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO run_bindings(task,project,tenant_id,thread_id,brief_id,created)
        SELECT t.id,tp.project,COALESCE(pt.tenant_id,tp.project),NULL,NULL,t.created
        FROM tasks t JOIN task_projects tp ON tp.task=t.id
        LEFT JOIN project_tenants pt ON pt.project=tp.project",
        [],
    )?;
    Ok(())
}

fn run(repo: &Path, args: &[&str]) -> Result<String> {
    git::run(repo, args)
}
fn head(repo: &Path) -> Result<String> {
    run(repo, &["rev-parse", "--verify", "HEAD^{commit}"])
}
fn remote_ref(repo: &Path, name: &str) -> Option<String> {
    run(
        repo,
        &["rev-parse", "--verify", &format!("{name}^{{commit}}")],
    )
    .ok()
}

fn run_lock(db: &Store, oid: &str) -> Result<std::fs::File> {
    use fs2::FileExt;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(db.root.join(format!("integration-{oid}.lock")))?;
    lock.lock_exclusive()?;
    Ok(lock)
}

fn run_branch(db: &Store, oid: &str) -> Result<(PathBuf, String, String)> {
    ensure!(
        db.rows("SELECT task FROM remote_links WHERE task=?", &[&oid])?
            .is_empty(),
        "remote root tasks do not have a local Run branch"
    );
    let path = git::task_workspace(db, oid)?;
    let branch = format!("horde/{oid}");
    ensure!(
        run(&path, &["branch", "--show-current"])? == branch,
        "Run workspace is on an unexpected branch"
    );
    ensure!(
        run(&path, &["status", "--porcelain"])?.is_empty(),
        "Run workspace is dirty; reconcile before continuing"
    );
    let sha = head(&path)?;
    Ok((path, branch, sha))
}

/// Bind Adam's durable Thread/Brief identity when the task is created.
pub fn bind_run_context(
    db: &Store,
    oid: &str,
    thread: Option<&str>,
    brief: Option<&str>,
) -> Result<()> {
    ensure!(
        thread.is_some() == brief.is_some(),
        "thread_id and brief_id must be supplied together"
    );
    if let (Some(thread), Some(brief)) = (thread, brief) {
        for (name, value) in [("thread_id", thread), ("brief_id", brief)] {
            ensure!(
                !value.trim().is_empty()
                    && value.len() <= 256
                    && !value.chars().any(char::is_control),
                "invalid {name}"
            );
        }
    }
    let project = crate::projects::task_project(db, oid)?;
    let tenant = crate::projects::tenant(db, &project)?;
    db.conn.execute(
        "INSERT INTO run_bindings VALUES(?,?,?,?,?,?)",
        params![oid, project, tenant, thread, brief, crate::store::now()],
    )?;
    db.event(
        oid,
        "run.bound",
        json!({"project_id":project,"tenant_id":tenant,"thread_id":thread,"brief_id":brief}),
    )?;
    Ok(())
}

pub fn run_context(db: &Store, oid: &str) -> Result<Value> {
    let rows = db.rows(
        "SELECT project,tenant_id,thread_id,brief_id FROM run_bindings WHERE task=?",
        &[&oid],
    )?;
    if let Some(row) = rows.first() {
        return Ok(row.clone());
    }
    let project = crate::projects::task_project(db, oid)?;
    let tenant = crate::projects::tenant(db, &project)?;
    Ok(json!({"project":project,"tenant_id":tenant,"thread_id":null,"brief_id":null}))
}

/// Read the current remote base for a later release compare-and-swap. This
/// never uses a cached remote-tracking ref or the Run's original start commit.
pub fn main_head(db: &Store, oid: &str) -> Result<Value> {
    let task = db.task(oid)?;
    let repo = Path::new(task["repo"].as_str().context("Run repository")?);
    run(repo, &["config", "--get", "remote.origin.url"])
        .context("Run repository has no origin remote")?;
    let settings: crate::config::Settings =
        serde_json::from_str(task["settings"].as_str().context("Run settings")?)?;
    let configured = settings.delivery.base.trim();
    let branch = if configured.is_empty() {
        let listing = run(repo, &["ls-remote", "--symref", "origin", "HEAD"])?;
        listing
            .lines()
            .find_map(|line| line.strip_prefix("ref: refs/heads/")?.split('\t').next())
            .context("origin did not advertise a default branch")?
            .to_owned()
    } else {
        configured.to_owned()
    };
    let branch_ref = format!("refs/heads/{branch}");
    run(repo, &["check-ref-format", &branch_ref]).context("invalid release base branch")?;
    let listing = run(repo, &["ls-remote", "--heads", "origin", &branch_ref])?;
    let commit_sha = listing
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .find_map(|(sha, name)| (name == branch_ref).then(|| sha.to_owned()))
        .context("origin release base branch is missing")?;
    ensure!(
        (commit_sha.len() == 40 || commit_sha.len() == 64)
            && commit_sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "origin returned an invalid base commit"
    );
    Ok(
        json!({"run_id":oid,"project_id":crate::projects::task_project(db,oid)?,"branch_ref":branch_ref,"commit_sha":commit_sha,"expected_main_head":commit_sha}),
    )
}

fn hmac_sha256(key: &[u8], payload: &[u8]) -> String {
    let mut inner_pad = [0x36u8; 64];
    let mut outer_pad = [0x5cu8; 64];
    for (index, byte) in key.iter().enumerate() {
        inner_pad[index] ^= byte;
        outer_pad[index] ^= byte;
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(payload);
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner.finalize());
    hex::encode(outer.finalize())
}

fn checkpoint_attestation(
    db: &Store,
    oid: &str,
    branch: &str,
    sha: &str,
    validation: &[String],
    validation_id: &str,
) -> Result<Value> {
    let key = match std::env::var("HORDE_RUN_ATTESTATION_KEY") {
        Ok(key) => key,
        Err(std::env::VarError::NotPresent) => return Ok(Value::Null),
        Err(error) => return Err(error.into()),
    };
    let key = STANDARD
        .decode(key.trim())
        .context("invalid HORDE_RUN_ATTESTATION_KEY base64")?;
    ensure!(
        key.len() == 32,
        "HORDE_RUN_ATTESTATION_KEY must decode to 32 bytes"
    );
    let context = run_context(db, oid)?;
    require_signed_identity(&context)?;
    let payload = json!({
        "tenant_id":context["tenant_id"],
        "project_id":context["project"],
        "thread_id":context["thread_id"],
        "brief_id":context["brief_id"],
        "run_id":oid,
        "branch_ref":format!("refs/heads/{branch}"),
        "commit_sha":sha,
        "validation_id":validation_id,
        "passed_checks":[validation],
    });
    let bytes = serde_json::to_vec(&payload)?;
    let mut signed = RUN_CHECKPOINT_DOMAIN.to_vec();
    signed.extend_from_slice(&bytes);
    Ok(
        json!({"algorithm":"hmac-sha256","payload_b64":STANDARD.encode(&bytes),"signature_hex":hmac_sha256(&key, &signed)}),
    )
}

fn require_signed_identity(context: &Value) -> Result<()> {
    ensure!(
        context["thread_id"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
            && context["brief_id"]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
        "signed Run checkpoint requires durable thread_id and brief_id"
    );
    Ok(())
}

fn expected_head(actual: &str, expected: &str) -> Result<()> {
    ensure!(
        actual == expected,
        "Run head changed: expected {expected}, found {actual}"
    );
    Ok(())
}

fn last_observed_remote(db: &Store, oid: &str) -> Result<Option<String>> {
    let rows = db.rows(
        "SELECT data FROM events WHERE task=? AND kind='run.branch_observed' ORDER BY seq DESC LIMIT 1",
        &[&oid],
    )?;
    Ok(rows
        .first()
        .and_then(|row| row["data"].as_str())
        .and_then(|data| serde_json::from_str::<Value>(data).ok())
        .and_then(|data| data["head_sha"].as_str().map(str::to_owned)))
}

fn record_conflict(
    db: &Store,
    oid: &str,
    branch: &str,
    local: &str,
    remote: Option<&str>,
    reason: &str,
) -> Result<()> {
    db.event(
        oid,
        "run.branch_conflict",
        json!({
            "state":"repair_required",
            "reason":reason,
            "branch_ref":format!("refs/heads/{branch}"),
            "local_head_sha":local,
            "remote_head_sha":remote,
            "repair":"Restore the observed remote history or merge both heads on the remote Run branch, then reconcile again without a force push"
        }),
    )?;
    Ok(())
}

fn record_healthy(db: &Store, oid: &str, branch: &str, local: &str) -> Result<()> {
    let latest = db.rows(
        "SELECT kind FROM events WHERE task=? AND kind IN ('run.branch_conflict','run.reconciliation_healthy') ORDER BY seq DESC LIMIT 1",
        &[&oid],
    )?;
    if latest.first().and_then(|event| event["kind"].as_str()) == Some("run.branch_conflict") {
        db.event(
            oid,
            "run.reconciliation_healthy",
            json!({"state":"healthy","branch_ref":format!("refs/heads/{branch}"),"local_head_sha":local}),
        )?;
    }
    Ok(())
}

pub fn reconciliation_status(db: &Store, oid: &str) -> Result<Value> {
    let rows = db.rows(
        "SELECT data FROM events WHERE task=? AND kind IN ('run.branch_conflict','run.reconciliation_healthy') ORDER BY seq DESC LIMIT 1",
        &[&oid],
    )?;
    Ok(rows
        .first()
        .and_then(|row| row["data"].as_str())
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or(Value::Null))
}

/// Project-scoped v1 Run event stream. The immutable SQLite sequence is both
/// the cursor and the source for stable event/idempotency IDs across retries.
pub fn events(db: &Store, oid: &str, after: i64, limit: i64) -> Result<Value> {
    ensure!(after >= 0, "run event cursor must be nonnegative");
    ensure!(
        (1..=1000).contains(&limit),
        "run event limit must be 1..1000"
    );
    let context = run_context(db, oid)?;
    let rows = db.rows(
        "SELECT seq,kind,data,created FROM events WHERE task=? AND kind LIKE 'run.%' AND seq>? ORDER BY seq LIMIT ?",
        &[&oid, &after, &(limit + 1)],
    )?;
    let has_more = rows.len() > limit as usize;
    let mut page = Vec::new();
    let mut cursor = after;
    for row in rows.into_iter().take(limit as usize) {
        let seq = row["seq"].as_i64().context("Run event sequence")?;
        cursor = seq;
        let payload: Value = serde_json::from_str(row["data"].as_str().context("Run event data")?)?;
        let created = row["created"].as_i64().context("Run event time")?;
        let occurred_at = time::OffsetDateTime::from_unix_timestamp(created)?
            .format(&time::format_description::well_known::Rfc3339)?;
        let event_id = format!("horde:{oid}:{seq}");
        let mut envelope = json!({
            "schema_version":1,
            "event_id":event_id,
            "event_type":row["kind"],
            "occurred_at":occurred_at,
            "tenant_id":context["tenant_id"],
            "project_id":context["project"],
            "run_id":oid,
            "idempotency_key":event_id,
            "payload":payload,
        });
        for field in ["thread_id", "brief_id"] {
            if !context[field].is_null() {
                envelope[field] = context[field].clone();
            }
        }
        for field in ["branch_ref", "commit_sha"] {
            if !payload[field].is_null() {
                envelope[field] = payload[field].clone();
            }
        }
        if envelope.get("commit_sha").is_none() && !payload["head_sha"].is_null() {
            envelope["commit_sha"] = payload["head_sha"].clone();
        }
        page.push(envelope);
    }
    Ok(json!({"events":page,"next_cursor":cursor,"has_more":has_more}))
}

/// Fetches one task-owned ref without using origin's configured wildcard refspec.
/// A missing ref is allowed only before the Run has ever observed a remote head.
fn fetch_run_head(db: &Store, oid: &str, path: &Path, branch: &str) -> Result<Option<String>> {
    run(path, &["config", "--get", "remote.origin.url"])
        .context("Run branch requires an origin remote")?;
    let remote = run(
        path,
        &[
            "ls-remote",
            "--heads",
            "origin",
            &format!("refs/heads/{branch}"),
        ],
    )?;
    let observed = last_observed_remote(db, oid)?;
    if remote.is_empty() {
        if observed.is_some() {
            record_conflict(db, oid, branch, &head(path)?, None, "remote_disappeared")?;
            bail!("remote Run branch disappeared; restore the observed history before reconciling");
        }
        return Ok(None);
    }
    let refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
    run(path, &["fetch", "--no-tags", "origin", &refspec])?;
    let fetched = remote_ref(path, &format!("refs/remotes/origin/{branch}"))
        .context("remote Run branch did not resolve after fetch")?;
    if let Some(previous) = observed
        && run(path, &["merge-base", "--is-ancestor", &previous, &fetched]).is_err()
    {
        record_conflict(
            db,
            oid,
            branch,
            &head(path)?,
            Some(&fetched),
            "remote_rewrite",
        )?;
        bail!(
            "remote Run branch was rewound or rewritten; restore the observed history before reconciling"
        );
    }
    Ok(Some(fetched))
}

fn observe_remote(db: &Store, oid: &str, branch: &str, sha: &str) -> Result<()> {
    if last_observed_remote(db, oid)?.as_deref() != Some(sha) {
        db.event(
            oid,
            "run.branch_observed",
            json!({"branch_ref":format!("refs/heads/{branch}"),"head_sha":sha}),
        )?;
    }
    Ok(())
}

/// Accept only authorized fast-forward commits pushed to the Run branch.
/// Git remote branch protection authenticates the pusher; the project-scoped
/// operator API authenticates the caller requesting reconciliation.
pub fn reconcile_run(db: &Store, oid: &str, expected: &str) -> Result<Value> {
    let _lock = run_lock(db, oid)?;
    reconcile_locked(db, oid, expected)
}

/// The caller already holds the task integration lock. This is invoked before
/// merging worker commits so external branch edits join combined validation.
pub(crate) fn reconcile_at_integration_safe_point(db: &Store, oid: &str) -> Result<()> {
    let (path, _, head) = run_branch(db, oid)?;
    if run(&path, &["config", "--get", "remote.origin.url"]).is_ok() {
        reconcile_locked(db, oid, &head)?;
    }
    Ok(())
}

fn reconcile_locked(db: &Store, oid: &str, expected: &str) -> Result<Value> {
    let (path, branch, before) = run_branch(db, oid)?;
    expected_head(&before, expected)?;
    let Some(remote) = fetch_run_head(db, oid, &path, &branch)? else {
        return Ok(
            json!({"branch_ref":format!("refs/heads/{branch}"),"head_sha":before,"remote_head_sha":null,"state":"unpublished"}),
        );
    };
    if run(&path, &["merge-base", "--is-ancestor", &before, &remote]).is_err()
        && run(&path, &["merge-base", "--is-ancestor", &remote, &before]).is_err()
    {
        record_conflict(db, oid, &branch, &before, Some(&remote), "divergent_heads")?;
        bail!(
            "remote and local Run branch diverged; merge both heads on the remote Run branch and reconcile again"
        );
    }
    let state = if before == remote {
        "current"
    } else if run(&path, &["merge-base", "--is-ancestor", &before, &remote]).is_ok() {
        run(&path, &["merge", "--ff-only", &remote])?;
        "fast_forwarded"
    } else {
        "local_ahead"
    };
    let current = head(&path)?;
    observe_remote(db, oid, &branch, &remote)?;
    record_healthy(db, oid, &branch, &current)?;
    if state == "fast_forwarded" {
        db.event(oid, "run.reconciled", json!({"branch_ref":format!("refs/heads/{branch}"),"previous_head_sha":before,"head_sha":current,"remote_head_sha":remote}))?;
    }
    Ok(
        json!({"branch_ref":format!("refs/heads/{branch}"),"head_sha":current,"remote_head_sha":remote,"state":state}),
    )
}

fn verified_checkpoint(db: &Store, oid: &str, sha: &str) -> Result<bool> {
    let rows = db.rows(
        "SELECT data FROM events WHERE task=? AND kind='run.checkpoint_verified' ORDER BY seq DESC LIMIT 1",
        &[&oid],
    )?;
    Ok(rows
        .first()
        .and_then(|row| row["data"].as_str())
        .and_then(|data| serde_json::from_str::<Value>(data).ok())
        .and_then(|data| data["head_sha"].as_str().map(str::to_owned))
        .is_some_and(|head| head == sha))
}

/// Verify a reproducible checkpoint at an exact Run head. The validation
/// command is an argv vector, never a shell expression.
pub fn checkpoint_run(
    db: &Store,
    oid: &str,
    expected: &str,
    validation: &[String],
    idempotency_key: Option<&str>,
) -> Result<Value> {
    let _lock = run_lock(db, oid)?;
    ensure!(
        !validation.is_empty() && !validation[0].is_empty(),
        "checkpoint validation command is required"
    );
    ensure!(
        validation.len() <= 32 && validation.iter().all(|arg| arg.len() <= 4096),
        "checkpoint validation exceeds bounds"
    );
    if let Some(key) = idempotency_key {
        ensure!(
            !key.trim().is_empty() && key.len() <= 256 && !key.chars().any(char::is_control),
            "invalid checkpoint idempotency_key"
        );
    }
    let (path, branch, sha) = run_branch(db, oid)?;
    expected_head(&sha, expected)?;
    if run(&path, &["config", "--get", "remote.origin.url"]).is_ok() {
        reconcile_locked(db, oid, &sha)?;
        expected_head(&head(&path)?, expected)?;
    }
    if std::env::var_os("HORDE_RUN_ATTESTATION_KEY").is_some() {
        require_signed_identity(&run_context(db, oid)?)?;
    }
    if let Some(key) = idempotency_key {
        let previous = db.rows(
            "SELECT data FROM events WHERE task=? AND kind='run.checkpoint_verified' AND json_extract(data,'$.idempotency_key')=? ORDER BY seq DESC LIMIT 1",
            &[&oid, &key],
        )?;
        if let Some(raw) = previous.first().and_then(|row| row["data"].as_str()) {
            let recorded: Value = serde_json::from_str(raw)?;
            ensure!(
                recorded["commit_sha"] == sha && recorded["validation"] == json!(validation),
                "checkpoint idempotency_key reused with different request"
            );
            return Ok(
                json!({"branch_ref":recorded["branch_ref"],"head_sha":sha,"commit_sha":sha,"validation_id":recorded["validation_id"],"verified":true,"attestation":recorded["attestation"],"idempotency_key":key,"duplicate":true}),
            );
        }
    }
    let values = crate::secrets::values(db, oid)?;
    ensure!(
        !validation
            .iter()
            .any(|arg| values.values().any(|secret| !secret.is_empty()
                && (arg == secret || secret.len() >= 8 && arg.contains(secret)))),
        "checkpoint validation argv contains an application secret"
    );
    let output = crate::budget::command_output(
        crate::executor::clean_command(&validation[0])
            .args(&validation[1..])
            .current_dir(&path)
            .envs(&values),
    )?;
    ensure!(
        head(&path)? == sha && run(&path, &["status", "--porcelain"])?.is_empty(),
        "Run changed during validation; checkpoint was not recorded"
    );
    if !output.status.success() {
        db.event(oid, "run.checkpoint_failed", json!({"branch_ref":format!("refs/heads/{branch}"),"head_sha":sha,"validation":validation,"status":output.status.code()}))?;
        bail!("Run checkpoint validation failed");
    }
    let validation_id = id();
    let attestation = checkpoint_attestation(db, oid, &branch, &sha, validation, &validation_id)?;
    db.event(oid, "run.checkpoint_verified", json!({"branch_ref":format!("refs/heads/{branch}"),"head_sha":sha,"commit_sha":sha,"validation_id":validation_id,"validation":validation,"idempotency_key":idempotency_key,"attestation":attestation}))?;
    Ok(
        json!({"branch_ref":format!("refs/heads/{branch}"),"head_sha":sha,"commit_sha":sha,"validation_id":validation_id,"verified":true,"attestation":attestation,"idempotency_key":idempotency_key,"duplicate":false}),
    )
}

/// Publish only a verified exact head using Git's normal non-force push.
pub fn publish_run(db: &Store, oid: &str, expected: &str) -> Result<Value> {
    let _lock = run_lock(db, oid)?;
    let (path, branch, sha) = run_branch(db, oid)?;
    expected_head(&sha, expected)?;
    ensure!(
        verified_checkpoint(db, oid, &sha)?,
        "Run head has no current verified checkpoint"
    );
    let remote = fetch_run_head(db, oid, &path, &branch)?;
    if let Some(remote) = &remote {
        ensure!(
            run(&path, &["merge-base", "--is-ancestor", remote, &sha]).is_ok(),
            "remote Run branch has newer or divergent work; reconcile before publishing"
        );
    }
    if remote.as_deref() != Some(&sha) {
        let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
        let push = run(&path, &["push", "origin", &refspec]);
        let observed = fetch_run_head(db, oid, &path, &branch)?;
        if observed.as_deref() != Some(&sha) {
            push?;
            bail!("published Run branch did not match its verified checkpoint");
        }
    }
    observe_remote(db, oid, &branch, &sha)?;
    let last = db.rows(
        "SELECT data FROM events WHERE task=? AND kind='run.branch_published' ORDER BY seq DESC LIMIT 1",
        &[&oid],
    )?;
    let duplicate = last
        .first()
        .and_then(|row| row["data"].as_str())
        .and_then(|data| serde_json::from_str::<Value>(data).ok())
        .is_some_and(|event| event["head_sha"] == sha);
    if !duplicate {
        db.event(
            oid,
            "run.branch_published",
            json!({"branch_ref":format!("refs/heads/{branch}"),"head_sha":sha}),
        )?;
    }
    Ok(
        json!({"branch_ref":format!("refs/heads/{branch}"),"head_sha":sha,"published":true,"duplicate":duplicate}),
    )
}

#[cfg(test)]
mod run_attestation_tests {
    use super::{RUN_CHECKPOINT_DOMAIN, hmac_sha256, require_signed_identity};
    use serde_json::json;

    #[test]
    fn signing_requires_both_durable_run_refs() {
        assert!(require_signed_identity(&json!({"thread_id":null,"brief_id":null})).is_err());
        assert!(require_signed_identity(&json!({"thread_id":"thread-1","brief_id":null})).is_err());
        require_signed_identity(&json!({"thread_id":"thread-1","brief_id":"brief-1"})).unwrap();
    }

    #[test]
    fn hmac_matches_rfc_4231_sha256_vector() {
        let key = [0x0bu8; 20];
        assert_eq!(
            hmac_sha256(&key, b"Hi There"),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn checkpoint_domain_signature_matches_independent_vector() {
        let mut message = RUN_CHECKPOINT_DOMAIN.to_vec();
        message.extend_from_slice(br#"{"run_id":"run-1"}"#);
        assert_eq!(
            hmac_sha256(&[0x42u8; 32], &message),
            "e15cecc20555668b8f23e261159e379e8a9c34b144e425826e528a1a77eda5c5"
        );
    }
}
