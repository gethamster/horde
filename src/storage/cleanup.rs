//! Conservative retention of recoverable worker checkouts. Git refs and history survive.
use crate::{
    git, management, project_runtime,
    store::{Store, now},
};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    path::{Component, Path, PathBuf},
};

struct Candidate {
    task: String,
    worker: String,
    workspace: PathBuf,
    repo: PathBuf,
    branch: String,
    head: String,
}

impl Candidate {
    fn value(&self, state: &str) -> Value {
        json!({"task":self.task,"worker":self.worker,"workspace":self.workspace,"branch":self.branch,"head":self.head,"state":state})
    }
    fn key(&self) -> String {
        format!("storage.cleaned.{}", self.worker)
    }
}

/// Remove only clean, integrated worker worktrees belonging to cold successful tasks.
/// Dry runs perform no filesystem or database mutations. The limit bounds removals;
/// each call inspects at most 256 old registrations to bound maintenance work.
pub fn cleanup(db: &Store, retention_seconds: u64, limit: usize, dry_run: bool) -> Result<Value> {
    let cutoff = now().saturating_sub(i64::try_from(retention_seconds).unwrap_or(i64::MAX));
    let cursor = if dry_run {
        String::new()
    } else {
        management::value(db, "storage.cleanup.cursor")?.unwrap_or_default()
    };
    let query = |cursor: &str| {
        db.rows("SELECT w.* FROM workers w JOIN tasks t ON t.id=w.task WHERE w.id>?1 AND t.status='succeeded' AND w.workspace IS NOT NULL AND MAX(w.updated,t.created,COALESCE((SELECT MAX(created) FROM events WHERE task=t.id),0),COALESCE((SELECT MAX(MAX(a.started,COALESCE(a.finished,a.started))) FROM attempts a JOIN steps s ON s.id=a.step WHERE s.task=t.id),0))<=?2 ORDER BY w.id LIMIT 256", &[&cursor, &cutoff])
    };
    let first = query(&cursor)?;
    let rows = if first.is_empty() && !cursor.is_empty() {
        query("")?
    } else {
        first
    };
    let row_count = rows.len();
    let mut inspected = 0;
    let mut candidates = Vec::new();
    let mut removed = Vec::new();
    let mut skipped = Vec::new();
    let mut errors = Vec::new();
    for row in rows {
        if candidates.len() >= limit {
            break;
        }
        let worker = row["id"].as_str().context("worker identity")?;
        inspected += 1;
        if !dry_run {
            management::set(db, "storage.cleanup.cursor", worker)?;
        }
        if !Path::new(row["workspace"].as_str().context("worker workspace")?).exists()
            && management::value(db, &format!("storage.cleaned.{worker}"))?.is_some()
        {
            continue;
        }
        let candidate = match inspect(db, &row, cutoff) {
            Ok(candidate) => candidate,
            Err(error) => {
                skipped
                    .push(json!({"worker":worker,"task":row["task"],"reason":error.to_string()}));
                continue;
            }
        };
        candidates.push(candidate.value("candidate"));
        if dry_run {
            continue;
        }
        match remove(db, &candidate, cutoff) {
            Ok(()) => removed.push(candidate.value("removed")),
            Err(error) => errors
                .push(json!({"worker":worker,"task":candidate.task,"error":error.to_string()})),
        }
    }
    if !dry_run && limit > 0 && inspected == row_count && row_count < 256 {
        management::set(db, "storage.cleanup.cursor", "")?;
    }
    Ok(
        json!({"dry_run":dry_run,"candidates":candidates,"removed":removed,"skipped":skipped,"errors":errors}),
    )
}

fn simple_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn owned_path(
    db: &Store,
    task: &str,
    worker: &str,
    registered: &Path,
    missing_leaf: bool,
) -> Result<PathBuf> {
    ensure!(
        simple_identifier(task) && simple_identifier(worker),
        "invalid workspace identity"
    );
    let expected = project_runtime::task_root(db, task)?
        .join("workspaces")
        .join(task)
        .join(worker);
    let relative = expected
        .strip_prefix(&db.root)
        .context("workspace outside Horde storage")?;
    let mut current = db.root.clone();
    ensure!(
        !std::fs::symlink_metadata(&current)?
            .file_type()
            .is_symlink(),
        "storage root is a symlink"
    );
    for component in relative.components() {
        ensure!(
            matches!(component, Component::Normal(_)),
            "invalid workspace path"
        );
        current.push(component);
        let metadata = match std::fs::symlink_metadata(&current) {
            Err(error)
                if missing_leaf
                    && current == expected
                    && error.kind() == std::io::ErrorKind::NotFound =>
            {
                continue;
            }
            result => result?,
        };
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "workspace path contains a symlink or non-directory"
        );
    }
    let expected = expected
        .parent()
        .context("workspace parent")?
        .canonicalize()?
        .join(worker);
    ensure!(
        registered == expected,
        "registered workspace is not the expected Horde-owned path"
    );
    Ok(expected)
}

fn protected(db: &Store, task: &str, cutoff: i64) -> Result<()> {
    let root: String =
        db.conn
            .query_row("SELECT root FROM task_tree WHERE task=?", [task], |r| {
                r.get(0)
            })?;
    let family = db.rows(
        "SELECT t.id,t.status FROM tasks t JOIN task_tree tree ON tree.task=t.id WHERE tree.root=?",
        &[&root],
    )?;
    ensure!(!family.is_empty(), "task family is unavailable");
    for member in family {
        ensure!(
            member["status"] == "succeeded",
            "task family has unfinished or unsuccessful work"
        );
        let id = member["id"].as_str().context("family task")?;
        let held: bool = db.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts a JOIN steps s ON s.id=a.step WHERE s.task=?1 AND a.state IN ('running','uncertain'))
             OR EXISTS(SELECT 1 FROM claims WHERE task=?1)
             OR EXISTS(SELECT 1 FROM workers WHERE task=?1 AND status IN ('working','unresponsive','notified'))
             OR EXISTS(SELECT 1 FROM app_environments WHERE task=?1 AND state!='removed')
             OR EXISTS(SELECT 1 FROM remote_environment_leases WHERE task=?1)
             OR EXISTS(SELECT 1 FROM integrations WHERE task=?1 AND state!='succeeded')
             OR EXISTS(SELECT 1 FROM project_remote_reservations WHERE task=?1 AND state!='released')
             OR EXISTS(SELECT 1 FROM account_remote_reservations WHERE task=?1 AND state!='released')
             OR EXISTS(SELECT 1 FROM account_reservations a JOIN steps s ON s.id=a.step WHERE s.task=?1 AND a.state!='released')
             OR EXISTS(SELECT 1 FROM remote_account_leases WHERE task=?1 AND state!='released')
             OR EXISTS(SELECT 1 FROM remote_links WHERE task=?1 AND state!='done')
             OR EXISTS(SELECT 1 FROM events WHERE task=?1 AND created>?2)
             OR EXISTS(SELECT 1 FROM workers WHERE task=?1 AND updated>?2)
             OR EXISTS(SELECT 1 FROM attempts a JOIN steps s ON s.id=a.step WHERE s.task=?1 AND MAX(a.started,COALESCE(a.finished,a.started))>?2)",
            rusqlite::params![id, cutoff], |r| r.get(0))?;
        ensure!(
            !held,
            "task family has active resources, ownership, uncertain effects, or recent activity"
        );
    }
    Ok(())
}

fn inspect(db: &Store, worker: &Value, cutoff: i64) -> Result<Candidate> {
    let task = worker["task"].as_str().context("task")?;
    let wid = worker["id"].as_str().context("worker")?;
    protected(db, task, cutoff)?;
    let registered = Path::new(worker["workspace"].as_str().context("workspace")?);
    let workspace = owned_path(db, task, wid, registered, false)?;
    let branch = format!("workers/{task}/{wid}");
    ensure!(
        worker["branch"] == branch,
        "registered worker branch is unexpected"
    );
    ensure!(
        git::run(&workspace, &["branch", "--show-current"])? == branch,
        "workspace branch changed"
    );
    let task_row = db.task(task)?;
    let repo = PathBuf::from(task_row["repo"].as_str().context("repository")?);
    let head = git::run(&workspace, &["rev-parse", "HEAD"])?;
    ensure!(
        git::run(&repo, &["rev-parse", &format!("refs/heads/{branch}")])? == head,
        "retained branch does not match workspace HEAD"
    );
    registered_in_git(&repo, &workspace, &branch, &head)?;
    let index = git::run(&workspace, &["ls-files", "-v", "-z"])?;
    ensure!(
        !index
            .split('\0')
            .filter_map(|entry| entry.as_bytes().first())
            .any(|flag| flag.is_ascii_lowercase() || *flag == b'S'),
        "workspace index hides tracked changes with assume-unchanged or skip-worktree"
    );
    ensure!(
        git::run(
            &workspace,
            &[
                "--no-optional-locks",
                "status",
                "--porcelain",
                "--untracked-files=all",
                "--ignored"
            ]
        )?
        .is_empty(),
        "workspace contains modified, untracked, or ignored files"
    );
    let integrated = format!("refs/heads/horde/{task}");
    let integrated_evidence: bool = db.conn.query_row("SELECT EXISTS(SELECT 1 FROM integrations WHERE task=? AND worker=? AND commit_id=? AND state='succeeded')", rusqlite::params![task,wid,head], |r| r.get(0))?;
    ensure!(
        integrated_evidence
            || git::run(&repo, &["merge-base", "--is-ancestor", &head, &integrated]).is_ok(),
        "worker HEAD has no successful integration evidence"
    );
    Ok(Candidate {
        task: task.into(),
        worker: wid.into(),
        workspace,
        repo,
        branch,
        head,
    })
}

fn registered_in_git(repo: &Path, workspace: &Path, branch: &str, head: &str) -> Result<()> {
    let output = git::run(repo, &["worktree", "list", "--porcelain", "-z"])?;
    let path = format!("worktree {}", workspace.to_str().context("workspace path")?);
    let branch = format!("branch refs/heads/{branch}");
    let head = format!("HEAD {head}");
    let found = output.split("\0\0").any(|entry| {
        let lines = entry.split('\0').collect::<Vec<_>>();
        lines.contains(&path.as_str())
            && lines.contains(&branch.as_str())
            && lines.contains(&head.as_str())
            && !lines
                .iter()
                .any(|line| line.starts_with("locked") || line.starts_with("prunable"))
    });
    ensure!(
        found,
        "Git worktree registration is missing, locked, or unexpected"
    );
    Ok(())
}

struct WorkspaceLock(File);

impl Drop for WorkspaceLock {
    fn drop(&mut self) {
        // Explicit unlock also releases transient copies inherited by a concurrent
        // child between fork and exec; merely closing our descriptor can retain it.
        let _ = FileExt::unlock(&self.0);
    }
}

fn lock(path: &Path) -> Result<WorkspaceLock> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "lock path is not a regular file"
        );
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.try_lock_exclusive()
        .with_context(|| format!("workspace operation is in progress: {}", path.display()))?;
    Ok(WorkspaceLock(file))
}

fn remove(db: &Store, candidate: &Candidate, cutoff: i64) -> Result<()> {
    // Match integration's lock order. Hold both through the intent and deletion.
    let _integration = lock(&db.root.join(format!("integration-{}.lock", candidate.task)))?;
    let _allocation = lock(
        &candidate
            .workspace
            .parent()
            .context("workspace parent")?
            .join(".allocate.lock"),
    )?;
    let marker = candidate.value("removing");
    db.atomic(|| {
        let current = inspect(db, &db.worker(&candidate.worker)?, cutoff)?;
        ensure!(
            current.head == candidate.head,
            "worker HEAD changed before cleanup intent"
        );
        management::set(db, &candidate.key(), &marker.to_string())
    })?;
    // A committed intent survives a crash between Git deletion and its receipt.
    // The write transaction prevents an administrator/scheduler from activating
    // this family or acquiring claims while Git is removing the checkout.
    db.atomic(|| {
        let current = inspect(db, &db.worker(&candidate.worker)?, cutoff)?;
        ensure!(
            current.head == candidate.head,
            "worker HEAD changed before cleanup"
        );
        git::run(
            &candidate.repo,
            &[
                "worktree",
                "remove",
                candidate.workspace.to_str().context("workspace path")?,
            ],
        )?;
        management::set(
            db,
            &candidate.key(),
            &candidate.value("removed").to_string(),
        )?;
        management::event(db, "storage.worktree_removed", candidate.value("removed"))
    })
}

/// Recreate only a checkout whose cleanup intent retained its exact Git branch.
/// Missing unmarked workspaces remain an ordinary reconciliation error in allocate.
pub fn restore(db: &Store, task: &str, worker: &str, path: &Path) -> Result<()> {
    let key = format!("storage.cleaned.{worker}");
    let Some(raw) = management::value(db, &key)? else {
        return Ok(());
    };
    let marker: Value = serde_json::from_str(&raw).context("invalid workspace cleanup receipt")?;
    let branch = format!("workers/{task}/{worker}");
    ensure!(
        marker["task"] == task && marker["worker"] == worker && marker["branch"] == branch,
        "workspace cleanup receipt identity mismatch"
    );
    ensure!(
        matches!(marker["state"].as_str(), Some("removing" | "removed")),
        "workspace cleanup receipt state mismatch"
    );
    let expected = owned_path(db, task, worker, path, true)?;
    ensure!(
        marker["workspace"].as_str() == expected.to_str(),
        "workspace cleanup receipt path mismatch"
    );
    let _integration = lock(&db.root.join(format!("integration-{task}.lock")))?;
    let _allocation = lock(
        &expected
            .parent()
            .context("workspace parent")?
            .join(".allocate.lock"),
    )?;
    db.atomic(|| restore_locked(db, task, worker, &expected, &branch, &key, &marker))
}

fn restore_locked(
    db: &Store,
    task: &str,
    worker: &str,
    path: &Path,
    branch: &str,
    key: &str,
    marker: &Value,
) -> Result<()> {
    let registration = db.worker(worker)?;
    ensure!(
        registration["task"] == task
            && registration["branch"] == branch
            && registration["workspace"].as_str() == path.to_str(),
        "registered workspace changed after cleanup"
    );
    owned_path(db, task, worker, path, true)?;
    let task_row = db.task(task)?;
    let repo = Path::new(task_row["repo"].as_str().context("repository")?);
    let recorded_head = marker["head"].as_str().context("cleanup receipt head")?;
    let retained_head = git::run(repo, &["rev-parse", &format!("refs/heads/{branch}")])?;
    // A failed deletion leaves the original valid checkout in place. A worker
    // may have used it since that intent, so do not pin intact work to an old HEAD.
    let intact_intent = path.try_exists()? && marker["state"] == "removing";
    let head = if intact_intent {
        retained_head.as_str()
    } else {
        recorded_head
    };
    ensure!(
        retained_head == head,
        "retained workspace branch changed; reconciliation required"
    );
    if !path.try_exists()? {
        git::run(
            repo,
            &[
                "worktree",
                "add",
                path.to_str().context("workspace path")?,
                branch,
            ],
        )?;
    }
    ensure!(
        git::run(path, &["branch", "--show-current"])? == branch
            && git::run(path, &["rev-parse", "HEAD"])? == head,
        "restored workspace branch or HEAD mismatch"
    );
    registered_in_git(repo, path, branch, head)?;
    db.conn
        .execute("DELETE FROM runtime_settings WHERE key=?", [key])?;
    management::event(
        db,
        "storage.worktree_restored",
        json!({"task":task,"worker":worker,"workspace":path,"head":head}),
    )
}
