//! Merge imported GitHub contributions only from the operator-owned WALGIT host.
use crate::{git, store::Store};
use anyhow::{Context, Result, bail, ensure};
use serde_json::json;
use std::path::Path;

pub(crate) fn remote(path: &Path) -> &'static str {
    if git::run(path, &["config", "--get", "remote.walgit.url"]).is_ok() {
        "walgit"
    } else {
        "origin"
    }
}

/// Caller owns the integration lock. Aborted conflicts preserve the Run head.
pub(crate) fn integrate(db: &Store, oid: &str, path: &Path) -> Result<()> {
    let task = db.task(oid)?;
    let settings: crate::config::Settings =
        serde_json::from_str(task["settings"].as_str().context("Run settings")?)?;
    if !settings.github.enabled {
        return Ok(());
    }
    ensure!(
        git::run(path, &["status", "--porcelain"])?.is_empty(),
        "Run workspace is dirty"
    );
    let remote = remote(path);
    // Always combine current WALGIT main before admitting the GitHub base.
    let main = crate::run::main_head(db, oid)?;
    let main_ref = main["branch_ref"].as_str().context("WALGIT base ref")?;
    git::run(path, &["fetch", "--no-tags", remote, main_ref])?;
    let main_sha = main["commit_sha"].as_str().context("WALGIT base SHA")?;
    let fetched_main = git::run(path, &["rev-parse", "FETCH_HEAD^{commit}"])?;
    ensure!(
        fetched_main == main_sha,
        "WALGIT main moved during contribution preparation; retry against its current head"
    );
    merge(db, oid, path, main_sha, "main_merge_conflict")?;
    let source = format!(
        "refs/heads/upstreams/github/base/{}",
        settings.github.base_branch
    );
    git::run(path, &["check-ref-format", &source])?;
    let listing = git::run(path, &["ls-remote", "--heads", remote, &source])?;
    ensure!(
        !listing.is_empty(),
        "GitHub base has not been imported into WALGIT"
    );
    let tracking = format!(
        "refs/remotes/{remote}/upstreams/github/base/{}",
        settings.github.base_branch
    );
    git::run(
        path,
        &[
            "fetch",
            "--no-tags",
            remote,
            &format!("+{source}:{tracking}"),
        ],
    )?;
    let imported = git::run(
        path,
        &["rev-parse", "--verify", &format!("{tracking}^{{commit}}")],
    )?;
    let before = git::run(path, &["rev-parse", "HEAD"])?;
    let first = db
        .rows(
            "SELECT seq FROM events WHERE task=? AND kind='run.github_integrated' LIMIT 1",
            &[&oid],
        )?
        .is_empty();
    if imported == before
        || git::run(path, &["merge-base", "--is-ancestor", &imported, &before]).is_ok()
    {
        if !first {
            let latest = db.rows("SELECT kind FROM events WHERE task=? AND kind IN ('run.branch_conflict','run.reconciliation_healthy') ORDER BY seq DESC LIMIT 1", &[&oid])?;
            if latest
                .first()
                .is_some_and(|row| row["kind"] == "run.branch_conflict")
            {
                db.event(oid, "run.reconciliation_healthy", json!({"state":"healthy","branch_ref":format!("refs/heads/horde/{oid}"),"local_head_sha":before}))?;
            }
            return Ok(());
        }
    } else {
        merge(db, oid, path, &imported, "github_merge_conflict")?;
    }
    let head = git::run(path, &["rev-parse", "HEAD"])?;
    db.event(oid, "run.github_integrated", json!({"repository":settings.github.repository,"source_ref":source,"source_sha":imported,"previous_head_sha":before,"head_sha":head}))?;
    db.event(oid, "run.reconciliation_healthy", json!({"state":"healthy","branch_ref":format!("refs/heads/horde/{oid}"),"local_head_sha":head}))?;
    Ok(())
}

fn merge(db: &Store, oid: &str, path: &Path, imported: &str, reason: &str) -> Result<()> {
    let before = git::run(path, &["rev-parse", "HEAD"])?;
    let ancestor = crate::budget::command_output(
        crate::executor::clean_command("git")
            .current_dir(path)
            .args(["merge-base", "--is-ancestor", imported, &before]),
    )?;
    ensure!(
        matches!(ancestor.status.code(), Some(0 | 1)),
        "cannot inspect imported ancestry"
    );
    if ancestor.status.success() {
        return Ok(());
    }
    if git::run(path, &["merge", "--no-edit", imported]).is_err() {
        let conflicts = git::run(path, &["diff", "--name-only", "--diff-filter=U"])?;
        // A failed merge need not have created MERGE_HEAD (for example unrelated history).
        if git::run(path, &["rev-parse", "--verify", "MERGE_HEAD"]).is_ok() {
            git::run(path, &["merge", "--abort"])?;
        }
        db.event(oid, "run.branch_conflict", json!({"state":"repair_required","reason":reason,"branch_ref":format!("refs/heads/horde/{oid}"),"local_head_sha":before,"remote_head_sha":imported,"conflicts":conflicts,"repair":"Merge the upstream into a worker branch, resolve conflicts, and integrate it into the Run"}))?;
        bail!("upstream contribution merge conflict; resolve through the Run");
    }
    Ok(())
}
