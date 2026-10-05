//! Commit a live worker's claimed changes through the owning daemon.
use super::run;
use crate::store::{Store, hash};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::OpenOptions,
    path::{Path, PathBuf},
};

fn command(workspace: &Path, index: &Path, args: &[&str]) -> Result<std::process::Output> {
    crate::budget::command_output(
        crate::executor::clean_command("git")
            .current_dir(workspace)
            .args(args)
            .env("GIT_INDEX_FILE", index),
    )
}

fn output(workspace: &Path, index: &Path, args: &[&str]) -> Result<String> {
    let result = command(workspace, index, args)?;
    ensure!(result.status.success(), "worker Git operation failed");
    Ok(String::from_utf8(result.stdout)?)
}

fn workspace(db: &Store, worker: &Value) -> Result<PathBuf> {
    let stored = Path::new(
        worker["workspace"]
            .as_str()
            .context("registered workspace")?,
    );
    let root = stored.canonicalize()?;
    ensure!(
        root == stored,
        "registered workspace changed its canonical location"
    );
    ensure!(
        Path::new(&run(&root, &["rev-parse", "--show-toplevel"])?).canonicalize()? == root,
        "registered workspace is not a worktree root"
    );
    let task = db.task(worker["task"].as_str().context("worker task")?)?;
    let repo = Path::new(task["repo"].as_str().context("task repository")?);
    let common = |path: &Path| -> Result<PathBuf> {
        let directory = PathBuf::from(run(path, &["rev-parse", "--git-common-dir"])?);
        Ok(if directory.is_absolute() {
            directory
        } else {
            path.join(directory)
        }
        .canonicalize()?)
    };
    ensure!(
        root != repo.canonicalize()? && common(&root)? == common(repo)?,
        "worker workspace no longer belongs to its task repository"
    );
    ensure!(
        run(&root, &["symbolic-ref", "--short", "HEAD"])?
            == worker["branch"].as_str().context("registered branch")?,
        "worker branch changed"
    );
    run(
        &root,
        &[
            "merge-base",
            "--is-ancestor",
            worker["base"].as_str().context("registered base")?,
            "HEAD",
        ],
    )?;
    Ok(root)
}

fn live_attempt(db: &Store, worker: &Value) -> Result<String> {
    let current = db.worker(worker["id"].as_str().context("worker id")?)?;
    let task = worker["task"].as_str().context("task")?;
    let step = worker["step"]
        .as_str()
        .context("worker has no assigned step")?;
    ensure!(
        db.task(task)?["status"] == "running"
            && current["status"] == "working"
            && current["task"] == task
            && current["step"] == step,
        "worker is not running"
    );
    let attempts = db.rows("SELECT a.id,a.state,s.state AS step_state FROM attempts a JOIN steps s ON s.id=a.step WHERE a.worker=? AND a.step=? AND s.task=? ORDER BY a.started DESC,a.rowid DESC LIMIT 1",
        &[&worker["id"].as_str(),&step,&task])?;
    let attempt = attempts.first().context("worker has no current attempt")?;
    ensure!(
        attempt["state"] == "running" && attempt["step_state"] == "running",
        "worker attempt is not running"
    );
    Ok(attempt["id"].as_str().context("attempt id")?.to_owned())
}

fn stage(db: &Store, worker: &Value, root: &Path, index: &Path) -> Result<String> {
    output(root, index, &["read-tree", "HEAD"])?;
    let base = worker["base"].as_str().context("base")?;
    let changed = output(
        root,
        index,
        &["diff", "--name-only", "--no-renames", "-z", base],
    )?;
    let untracked = output(
        root,
        index,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?;
    let paths: BTreeSet<_> = changed
        .split('\0')
        .chain(untracked.split('\0'))
        .filter(|p| !p.is_empty())
        .collect();
    ensure!(!paths.is_empty(), "worker has no claimed changes to commit");
    for path in &paths {
        db.check_write(worker["id"].as_str().context("worker id")?, path)?;
        let target = crate::native::safe_path(root, path)?;
        if let Ok(metadata) = std::fs::symlink_metadata(target) {
            ensure!(
                metadata.is_file(),
                "worker commit paths must be regular source files"
            );
        }
    }
    let list = index.with_extension("paths");
    let bytes: Vec<u8> = paths
        .into_iter()
        .flat_map(|path| path.bytes().chain([0]))
        .collect();
    std::fs::write(&list, bytes)?;
    output(
        root,
        index,
        &[
            "--literal-pathspecs",
            "add",
            "--all",
            "--pathspec-file-nul",
            &format!(
                "--pathspec-from-file={}",
                list.to_str().context("path list")?
            ),
        ],
    )?;
    output(root, index, &["diff", "--cached", "--check"])?;
    Ok(output(root, index, &["write-tree"])?.trim().to_owned())
}

fn verify_commit(root: &Path, commit: &str, record: &Value) -> Result<()> {
    ensure!(
        run(root, &["rev-parse", &format!("{commit}^1")])? == record["parent"],
        "worker commit parent does not match its receipt"
    );
    ensure!(
        run(root, &["rev-parse", &format!("{commit}^{{tree}}")])? == record["tree"],
        "worker commit tree does not match its receipt"
    );
    ensure!(
        run(root, &["show", "--no-patch", "--format=%B", commit])?
            == record["message"]
                .as_str()
                .context("receipt message")?
                .trim(),
        "worker commit provenance does not match its receipt"
    );
    Ok(())
}

pub fn commit_work(db: &Store, wid: &str, args: &Value, credential: &str) -> Result<Value> {
    let expected = args["expected_head"].as_str().context("expected_head")?;
    let message = args["message"].as_str().context("message")?;
    let key = args["idempotency_key"]
        .as_str()
        .context("idempotency_key")?;
    ensure!(
        (expected.len() == 40 || expected.len() == 64)
            && expected.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid expected_head"
    );
    ensure!(
        !message.trim().is_empty()
            && message.len() <= 4096
            && !message.chars().any(char::is_control),
        "invalid commit message"
    );
    ensure!(
        !key.is_empty() && key.len() <= 96 && key.bytes().all(|b| b.is_ascii_graphic()),
        "invalid idempotency_key"
    );
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(
            db.root
                .join(format!("worker-commit-{}.lock", hash(wid.as_bytes()))),
        )?;
    lock.lock_exclusive()?;
    let worker = db.worker(wid)?;
    let task = worker["task"].as_str().context("task")?;
    let root = workspace(db, &worker)?;
    let key_hash = hash(key.as_bytes());
    let name = format!("worker_commit/{wid}/{key_hash}");
    let request_hash = hash(
        json!({"expected_head":expected,"message":message,"key":key})
            .to_string()
            .as_bytes(),
    );
    let existing = db
        .rows(
            "SELECT state,data FROM external_ops WHERE task=? AND name=?",
            &[&task, &name],
        )?
        .into_iter()
        .next();
    let mut record = existing
        .as_ref()
        .map(|row| -> Result<Value> {
            Ok(serde_json::from_str(
                row["data"].as_str().context("commit receipt")?,
            )?)
        })
        .transpose()?;
    if let Some(receipt) = &record {
        ensure!(
            receipt["request_hash"] == request_hash && receipt["worker"] == wid,
            "idempotency key reused with another commit request"
        );
        if existing.as_ref().unwrap()["state"] == "succeeded" {
            let response = receipt["response"].clone();
            verify_commit(
                &root,
                response["commit_sha"].as_str().context("commit sha")?,
                receipt,
            )?;
            return Ok(response);
        }
        ensure!(
            existing.as_ref().unwrap()["state"] == "prepared",
            "unknown commit receipt state"
        );
    }
    let attempt = live_attempt(db, &worker)?;
    if let Some(receipt) = &record {
        ensure!(
            receipt["attempt"] == attempt,
            "commit belongs to another worker attempt"
        );
    }
    // Keep the staging index in daemon state, outside the worker's writable
    // workspace and shared temporary directory.
    let directory = tempfile::tempdir_in(&db.root)?;
    let index = directory.path().join("index");
    let current = run(&root, &["rev-parse", "HEAD"])?;
    if current == expected {
        let tree = stage(db, &worker, &root, &index)?;
        ensure!(
            tree != run(&root, &["rev-parse", "HEAD^{tree}"])?,
            "worker has no content changes to commit"
        );
        if let Some(receipt) = &record {
            ensure!(
                receipt["tree"] == tree,
                "worker changed since the prepared commit"
            );
        } else {
            let redacted =
                crate::secrets::redact(db, task, &json!(message.replace(credential, "[REDACTED]")));
            let body = format!(
                "{}\n\nHorde-Commit-Id: {key_hash}\nHorde-Worker-Id: {wid}\nHorde-Attempt-Id: {attempt}",
                redacted.as_str().context("redacted message")?
            );
            let receipt = json!({"request_hash":request_hash,"worker":wid,"attempt":attempt,
                "parent":expected,"tree":tree,"message":body});
            db.conn.execute(
                "INSERT INTO external_ops(task,name,state,data) VALUES(?,?,'prepared',?)",
                params![task, name, receipt.to_string()],
            )?;
            record = Some(receipt);
        }
        ensure!(
            workspace(db, &worker)? == root
                && live_attempt(db, &worker)? == attempt
                && run(&root, &["rev-parse", "HEAD"])? == expected,
            "worker changed before commit"
        );
        let repeated = stage(db, &worker, &root, &index)?;
        ensure!(
            record.as_ref().unwrap()["tree"] == repeated,
            "worker content changed before commit"
        );
        ensure!(
            live_attempt(db, &worker)? == attempt
                && run(&root, &["rev-parse", "HEAD"])? == expected,
            "worker changed before committing its staged tree"
        );
        output(
            &root,
            &index,
            &[
                "-c",
                "user.name=Horde Worker",
                "-c",
                "user.email=worker@horde.sh",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "commit.cleanup=verbatim",
                "commit",
                "-m",
                record.as_ref().unwrap()["message"]
                    .as_str()
                    .context("commit message")?,
            ],
        )?;
    } else {
        ensure!(record.is_some(), "worker head differs from expected_head");
    }
    let commit = run(&root, &["rev-parse", "HEAD"])?;
    let receipt = record.as_mut().context("prepared commit receipt")?;
    verify_commit(&root, &commit, receipt)?;
    ensure!(
        stage(db, &worker, &root, &index)? == receipt["tree"],
        "worker changed after committing"
    );
    ensure!(
        workspace(db, &worker)? == root
            && live_attempt(db, &worker)? == attempt
            && run(&root, &["rev-parse", "HEAD"])? == commit,
        "worker changed before commit completion"
    );
    // Refresh controller-owned Git metadata without modifying any worktree file.
    run(&root, &["read-tree", &commit])?;
    ensure!(
        run(&root, &["status", "--porcelain"])?.is_empty(),
        "worker changed during index reconciliation"
    );
    let response = json!({"committed":true,"worker":wid,"attempt":attempt,"commit_sha":commit,
        "tree_sha":receipt["tree"],"parent_sha":expected,"branch":worker["branch"]});
    receipt["response"] = response.clone();
    db.atomic(|| {
        let prior = db.rows("SELECT seq FROM events WHERE task=? AND kind='worker.committed' AND json_extract(data,'$.worker')=? AND json_extract(data,'$.attempt')=? AND json_extract(data,'$.commit_sha')=? LIMIT 1",
            &[&task,&wid,&attempt,&commit])?;
        db.conn.execute(
            "UPDATE external_ops SET state='succeeded',data=? WHERE task=? AND name=?",
            params![receipt.to_string(), task, name],
        )?;
        if prior.is_empty() {
            db.event(task, "worker.committed", response.clone())?;
        }
        Ok(())
    })?;
    Ok(response)
}
