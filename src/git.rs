use crate::store::{Store, id, now};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub fn run(repo: &Path, args: &[&str]) -> Result<String> {
    let out = crate::budget::command_output(
        crate::executor::clean_command("git")
            .current_dir(repo)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0"),
    )?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}
pub fn task_workspace(db: &Store, oid: &str) -> Result<PathBuf> {
    let o = db.task(oid)?;
    let repo = Path::new(o["repo"].as_str().context("repo")?);
    let path = db.root.join("workspaces").join(oid).join("integrated");
    if !path.exists() {
        std::fs::create_dir_all(path.parent().context("parent")?)?;
        run(
            repo,
            &[
                "worktree",
                "add",
                "-b",
                &format!("horde/{oid}"),
                path.to_str().context("path")?,
                "HEAD",
            ],
        )?;
    }
    if run(&path, &["branch", "--show-current"])? != format!("horde/{oid}") {
        bail!("integrated workspace is on an unexpected branch");
    }
    Ok(path)
}
pub fn allocate(db: &Store, oid: &str, wid: &str) -> Result<PathBuf> {
    let worker = db.worker(wid)?;
    if worker["task"] != oid {
        bail!("wrong task");
    }
    if let Some(path) = worker["workspace"].as_str() {
        let path = PathBuf::from(path);
        if run(&path, &["branch", "--show-current"])?
            != worker["branch"].as_str().context("registered branch")?
        {
            bail!("worker workspace is on an unexpected branch");
        }
        return Ok(path);
    }
    let integrated = task_workspace(db, oid)?;
    let path = db.root.join("workspaces").join(oid).join(wid);
    let branch = format!("workers/{oid}/{wid}");
    if !path.exists() {
        let exists = run(
            &integrated,
            &["show-ref", "--verify", &format!("refs/heads/{branch}")],
        )
        .is_ok();
        if exists {
            run(
                &integrated,
                &["worktree", "add", path.to_str().context("path")?, &branch],
            )?;
        } else {
            run(
                &integrated,
                &[
                    "worktree",
                    "add",
                    "-b",
                    &branch,
                    path.to_str().context("path")?,
                    "HEAD",
                ],
            )?;
        }
    }
    let base = run(&path, &["rev-parse", "HEAD"])?;
    register(db, oid, wid, &path, &branch, &base)?;
    Ok(path)
}
pub fn register(
    db: &Store,
    oid: &str,
    wid: &str,
    path: &Path,
    branch: &str,
    base: &str,
) -> Result<()> {
    let worker = db.worker(wid)?;
    if worker["task"] != oid {
        bail!("wrong task");
    }
    let canonical = path.canonicalize()?;
    if let Some(existing) = worker["workspace"].as_str() {
        if Path::new(existing).canonicalize()? == canonical
            && worker["branch"] == branch
            && worker["base"] == base
        {
            return Ok(());
        }
        bail!("workspace already registered; reconcile before replacing");
    }
    let top = PathBuf::from(run(&canonical, &["rev-parse", "--show-toplevel"])?).canonicalize()?;
    if top != canonical {
        bail!("workspace must be worktree root");
    }
    let o = db.task(oid)?;
    let repo = Path::new(o["repo"].as_str().context("repo")?);
    let common = |p: &Path| -> Result<PathBuf> {
        let d = PathBuf::from(run(p, &["rev-parse", "--git-common-dir"])?);
        Ok(if d.is_absolute() { d } else { p.join(d) }.canonicalize()?)
    };
    if common(repo)? != common(&canonical)? {
        bail!("workspace is not part of the task repository");
    }
    if canonical == repo.canonicalize()? {
        bail!("workers need a separate worktree");
    }
    if !run(&canonical, &["status", "--porcelain"])?.is_empty() {
        bail!("workspace must be clean at registration");
    }
    if run(&canonical, &["branch", "--show-current"])? != branch
        || run(&canonical, &["rev-parse", "HEAD"])? != base
    {
        bail!("workspace branch/base do not match Git");
    }
    if !db
        .rows(
            "SELECT id FROM workers WHERE workspace=?",
            &[&canonical.to_str()],
        )?
        .is_empty()
    {
        bail!("workspace already owned");
    }
    db.conn.execute(
        "UPDATE workers SET workspace=?,branch=?,base=?,updated=? WHERE id=?",
        rusqlite::params![canonical.to_str(), branch, base, now(), wid],
    )?;
    db.event(
        oid,
        "workspace.registered",
        json!({"worker":wid,"path":canonical,"branch":branch,"base":base}),
    )?;
    Ok(())
}
pub fn validate_scope(db: &Store, wid: &str) -> Result<Vec<String>> {
    let w = db.worker(wid)?;
    let workspace = Path::new(w["workspace"].as_str().context("workspace")?);
    let registered_base = w["base"].as_str().context("base")?;
    run(
        workspace,
        &["merge-base", "--is-ancestor", registered_base, "HEAD"],
    )
    .context("worker history no longer contains its registered base")?;
    let base = run(
        workspace,
        &[
            "merge-base",
            "HEAD",
            &format!("horde/{}", w["task"].as_str().context("task")?),
        ],
    )?;
    let mut files = vec![];
    for args in [
        vec!["diff", "--name-only", "--no-renames", "-z", &base],
        vec!["ls-files", "--others", "--exclude-standard", "-z"],
    ] {
        let output = Command::new("git")
            .current_dir(workspace)
            .args(args)
            .output()?;
        if !output.status.success() {
            bail!("cannot inspect changed files");
        }
        for file in output.stdout.split(|x| *x == 0).filter(|x| !x.is_empty()) {
            let file = String::from_utf8(file.to_vec())?;
            db.check_write(wid, &file)?;
            files.push(file);
        }
    }
    Ok(files)
}
pub fn integrate(
    db: &Store,
    oid: &str,
    wid: &str,
    validation: &[String],
) -> Result<serde_json::Value> {
    // A single daemon serializes this function; file lock also protects explicit integration calls.
    use fs2::FileExt;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(db.root.join(format!("integration-{oid}.lock")))?;
    lock.lock_exclusive()?;
    let w = db.worker(wid)?;
    let workspace = Path::new(w["workspace"].as_str().context("workspace")?);
    validate_scope(db, wid)?;
    if !run(workspace, &["status", "--porcelain"])?.is_empty() {
        bail!("worker has uncommitted changes; commit before integration");
    }
    let commit = run(workspace, &["rev-parse", "HEAD"])?;
    let target = task_workspace(db, oid)?;
    let previous = db.rows(
        "SELECT evidence FROM integrations WHERE task=? AND worker=? AND commit_id=?",
        &[&oid, &wid, &commit],
    )?;
    let remembered: Vec<String> = previous
        .first()
        .and_then(|p| p["evidence"].as_str())
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| serde_json::from_value(v["validation"].clone()).ok())
        .unwrap_or_default();
    let validation = if validation.is_empty() {
        remembered.as_slice()
    } else {
        validation
    };
    let iid = id();
    db.conn.execute(
        "INSERT OR IGNORE INTO integrations VALUES(?,?,?,?,'queued',NULL,?)",
        rusqlite::params![iid, oid, wid, commit, now()],
    )?;
    let before = run(&target, &["rev-parse", "HEAD"])?;
    if !run(&target, &["status", "--porcelain"])?.is_empty() {
        bail!("integrated workspace is dirty; reconciliation required");
    }
    let already = crate::budget::command_output(Command::new("git").current_dir(&target).args([
        "merge-base",
        "--is-ancestor",
        &commit,
        "HEAD",
    ]))?
    .status
    .success();
    if !already {
        db.conn.execute("UPDATE integrations SET state='running',evidence=? WHERE task=? AND worker=? AND commit_id=?",rusqlite::params![json!({"before":before,"validation":validation}).to_string(),oid,wid,commit])?;
        if let Err(e) = run(&target, &["merge", "--no-ff", "--no-edit", &commit]) {
            let conflicts =
                run(&target, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
            let evidence = json!({"error":e.to_string(),"conflicts":conflicts,"before":before,"validation":validation});
            // Abort only the merge initiated above; preserves the integrated branch.
            run(&target, &["merge", "--abort"])?;
            db.conn.execute("UPDATE integrations SET state='conflict',evidence=? WHERE task=? AND worker=? AND commit_id=?",rusqlite::params![evidence.to_string(),oid,wid,commit])?;
            db.event(oid, "integration.conflict", evidence.clone())?;
            let _=db.send(oid,wid,&id(),wid,&format!("Integration conflict: {evidence}. Merge the integrated branch into your worktree, resolve and verify."),&json!({"commit":commit}),true);
            bail!("integration conflict: {evidence}");
        }
    }
    if !validation.is_empty() {
        let values = crate::secrets::values(db, oid)?;
        let out = crate::budget::command_output(
            crate::executor::clean_command(&validation[0])
                .args(&validation[1..])
                .current_dir(&target)
                .envs(&values),
        )?;
        if !out.status.success() {
            let evidence = crate::secrets::redact_json(
                &json!({"stdout":String::from_utf8_lossy(&out.stdout),"stderr":String::from_utf8_lossy(&out.stderr),"before":before,"validation":validation}),
                &values,
            );
            db.conn.execute("UPDATE integrations SET state='validation_failed',evidence=? WHERE task=? AND worker=? AND commit_id=?",rusqlite::params![evidence.to_string(),oid,wid,commit])?;
            db.event(oid, "integration.validation_failed", evidence.clone())?;
            bail!("combined validation failed: {evidence}");
        }
    }
    db.conn.execute(
        "UPDATE integrations SET state='succeeded' WHERE task=? AND worker=? AND commit_id=?",
        rusqlite::params![oid, wid, commit],
    )?;
    db.event(
        oid,
        "integration.succeeded",
        json!({"worker":wid,"commit":commit}),
    )?;
    Ok(json!({"commit":commit,"integrated_head":run(&target,&["rev-parse","HEAD"])?}))
}
