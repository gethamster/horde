//! Explicit, audited recovery of agent-authored edits from a failed local step.
use crate::{
    config::Settings,
    git,
    store::{Store, now},
};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use rusqlite::params;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs::OpenOptions, path::Path};

fn sha(value: &str, field: &str) -> Result<()> {
    ensure!(
        (value.len() == 40 || value.len() == 64)
            && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid {field}"
    );
    Ok(())
}

fn head(workspace: &Path) -> Result<String> {
    git::run(workspace, &["rev-parse", "HEAD^{commit}"])
}

fn tree(workspace: &Path) -> Result<String> {
    git::run(workspace, &["write-tree"])
}

fn worktree<'a>(worker: &'a Value, task: &str) -> Result<&'a Path> {
    ensure!(worker["task"] == task, "worker belongs to another Run");
    let path = Path::new(worker["workspace"].as_str().context("worker workspace")?);
    ensure!(path.is_dir(), "worker workspace is missing");
    Ok(path)
}

fn latest_failed(db: &Store, step: &str) -> Result<Value> {
    let attempts = db.rows(
        "SELECT id,step,worker,state FROM attempts WHERE step=? ORDER BY started DESC,rowid DESC LIMIT 1",
        &[&step],
    )?;
    let attempt = attempts.into_iter().next().context("step has no attempt")?;
    ensure!(attempt["state"] == "failed", "latest attempt is not failed");
    ensure!(
        attempt["worker"].is_string(),
        "failed attempt has no worker"
    );
    Ok(attempt)
}

fn inactive(db: &Store, task: &str) -> Result<()> {
    let active = db.rows(
        "SELECT a.id FROM attempts a JOIN steps s ON s.id=a.step WHERE s.task=? AND a.state IN ('running','uncertain') LIMIT 1",
        &[&task],
    )?;
    ensure!(
        active.is_empty(),
        "reconcile active or uncertain attempts first"
    );
    ensure!(
        db.rows(
            "SELECT id FROM questions WHERE task=? AND answer IS NULL LIMIT 1",
            &[&task]
        )?
        .is_empty(),
        "answer pending questions first"
    );
    Ok(())
}

fn descendants(db: &Store, task: &str, root: &str) -> Result<Vec<Value>> {
    let steps = db.steps(task)?;
    let mut reached = BTreeSet::from([root.to_owned()]);
    loop {
        let prior = reached.len();
        for row in steps.iter().filter(|row| row["state"] == "skipped") {
            let spec = Store::step(row)?;
            if spec.needs.iter().any(|name| reached.contains(name)) {
                reached.insert(spec.id);
            }
        }
        if reached.len() == prior {
            break;
        }
    }
    Ok(steps
        .into_iter()
        .filter(|row| {
            row["state"] == "skipped"
                && row["name"]
                    .as_str()
                    .is_some_and(|name| reached.contains(name))
        })
        .collect())
}

fn checked_command(workspace: &Path, validation: &[String], timeout_seconds: u64) -> Result<()> {
    let mut command = crate::executor::clean_command(&validation[0]);
    command.args(&validation[1..]).current_dir(workspace);
    if let Some(host) = std::env::var_os("DOCKER_HOST") {
        command.env("DOCKER_HOST", host);
    }
    let out = crate::budget::command_output_with_timeout(
        &mut command,
        std::time::Duration::from_secs(timeout_seconds.clamp(1, 1800)),
    )?;
    ensure!(
        out.status.success(),
        "recovery validation failed; inspect the failed step and repair its worktree"
    );
    Ok(())
}

fn committed_recovery(
    workspace: &Path,
    key: &str,
    expected: &str,
    validated: &str,
) -> Result<bool> {
    let current = head(workspace)?;
    // An empty internal commit key records a preexisting agent commit. Operator
    // idempotency keys cannot be empty, so legacy recovery trailers stay distinct.
    if key.is_empty() {
        ensure!(current == expected, "agent commit changed during recovery");
        ensure!(
            git::run(workspace, &["rev-parse", "HEAD^{tree}"])? == validated,
            "agent tree changed during recovery"
        );
        return Ok(true);
    }
    if current == expected {
        return Ok(false);
    }
    ensure!(
        git::run(workspace, &["rev-parse", "HEAD^1"])
            .ok()
            .as_deref()
            == Some(expected),
        "worker head changed during recovery"
    );
    ensure!(
        git::run(workspace, &["rev-parse", "HEAD^{tree}"])?.as_str() == validated,
        "worker tree changed during recovery"
    );
    let message = git::run(workspace, &["log", "-1", "--format=%B"])?;
    ensure!(
        message
            .lines()
            .any(|line| line == format!("Horde-Recovery-Id: {key}")),
        "worker head is not this recovery's commit"
    );
    Ok(true)
}

fn existing_agent_commit(
    db: &Store,
    worker: &Value,
    workspace: &Path,
    expected: &str,
) -> Result<bool> {
    let base = worker["base"].as_str().context("registered worker base")?;
    if base == expected {
        return Ok(false);
    }
    ensure!(
        head(workspace)? == expected,
        "worker head changed during recovery"
    );
    ensure!(
        git::run(workspace, &["symbolic-ref", "--short", "HEAD"])?
            == worker["branch"]
                .as_str()
                .context("registered worker branch")?,
        "worker branch changed during recovery"
    );
    git::run(workspace, &["merge-base", "--is-ancestor", base, expected])?;
    let paths = crate::budget::command_output(
        crate::executor::clean_command("git")
            .current_dir(workspace)
            .args(["diff", "--name-only", "--no-renames", "-z", base, expected]),
    )?;
    ensure!(paths.status.success(), "cannot inspect agent commit paths");
    let paths = String::from_utf8(paths.stdout)?;
    ensure!(
        !paths.is_empty(),
        "agent commit has no content changes from its registered base"
    );
    for path in paths.split('\0').filter(|path| !path.is_empty()) {
        db.check_write(worker["id"].as_str().context("worker id")?, path)?;
    }
    // A stale index may classify committed files as untracked. Compare physical
    // work against the pinned commit using a private temporary index instead.
    let directory = tempfile::tempdir()?;
    let index = directory.path().join("index");
    let inspect = |args: &[&str]| -> Result<std::process::Output> {
        crate::budget::command_output(
            crate::executor::clean_command("git")
                .current_dir(workspace)
                .args(args)
                .env("GIT_INDEX_FILE", &index),
        )
    };
    ensure!(
        inspect(&["read-tree", expected])?.status.success(),
        "cannot inspect agent commit"
    );
    let refreshed = inspect(&["update-index", "--refresh"])?;
    let changed = inspect(&["diff-files", "--quiet"])?;
    let untracked = inspect(&["ls-files", "--others", "--exclude-standard", "-z"])?;
    ensure!(untracked.status.success(), "cannot inspect agent files");
    ensure!(
        head(workspace)? == expected,
        "worker head changed during recovery"
    );
    Ok(refreshed.status.success() && changed.status.success() && untracked.stdout.is_empty())
}

fn exact_prior_integration(
    db: &Store,
    task: &str,
    worker: &str,
    commit: &str,
    run_workspace: &Path,
    expected_run_head: &str,
) -> Result<bool> {
    let rows = db.rows(
        "SELECT state FROM integrations WHERE task=? AND worker=? AND commit_id=?",
        &[&task, &worker, &commit],
    )?;
    Ok(rows.first().is_some_and(|row| {
        matches!(
            row["state"].as_str(),
            Some("running" | "validation_failed" | "succeeded")
        )
    }) && git::run(run_workspace, &["rev-parse", "HEAD^1"])
        .ok()
        .as_deref()
        == Some(expected_run_head)
        && git::run(run_workspace, &["rev-parse", "HEAD^2"])
            .ok()
            .as_deref()
            == Some(commit))
}

/// Recover only a stopped, failed agent step in an explicitly trusted local installation.
/// Worker credentials cannot call this operation; a project-scoped operator can.
pub fn recover_step(
    db: &Store,
    task: &str,
    step_name: &str,
    expected_worker_head: &str,
    expected_run_head: &str,
    validation: &[String],
    key: &str,
) -> Result<Value> {
    ensure!(
        std::env::var("HORDE_TRUSTED_LOCAL_RECOVERY").as_deref() == Ok("1"),
        "failed-step recovery is disabled for this installation"
    );
    recover_with_policy(
        db,
        task,
        step_name,
        expected_worker_head,
        expected_run_head,
        validation,
        key,
    )
}

fn recover_with_policy(
    db: &Store,
    task: &str,
    step_name: &str,
    expected_worker_head: &str,
    expected_run_head: &str,
    validation: &[String],
    key: &str,
) -> Result<Value> {
    sha(expected_worker_head, "expected_worker_head")?;
    sha(expected_run_head, "expected_run_head")?;
    ensure!(
        !step_name.is_empty() && step_name.len() <= 128 && !step_name.chars().any(char::is_control),
        "invalid step name"
    );
    ensure!(
        !key.is_empty() && key.len() <= 128 && key.bytes().all(|b| b.is_ascii_graphic()),
        "invalid idempotency_key"
    );
    ensure!(
        !validation.is_empty()
            && validation.len() <= 32
            && validation
                .iter()
                .all(|arg| !arg.is_empty() && arg.len() <= 4096 && !arg.contains('\0')),
        "validation must be a bounded nonempty argv"
    );
    let request = json!({"step":step_name,"expected_worker_head":expected_worker_head,
        "expected_run_head":expected_run_head,"validation":validation});
    // The recovery lock is separate from the integration lock, which is taken
    // later by git::integrate_expected. No model attempt can start on a failed task.
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(db.root.join(format!("recovery-{task}.lock")))?;
    lock.lock_exclusive()?;
    let existing = db
        .rows(
            "SELECT * FROM run_step_recoveries WHERE task=? AND idempotency_key=?",
            &[&task, &key],
        )?
        .into_iter()
        .next();
    if let Some(row) = &existing {
        let recorded: Value =
            serde_json::from_str(row["request"].as_str().context("recovery request")?)?;
        ensure!(
            recorded == request,
            "idempotency key reused with different recovery request"
        );
        if let Some(response) = row["response"].as_str() {
            return Ok(serde_json::from_str(response)?);
        }
    }
    let task_row = db.task(task)?;
    let settings: Settings =
        serde_json::from_str(task_row["settings"].as_str().context("task settings")?)?;
    ensure!(
        settings.allow_commands,
        "task does not allow validation commands"
    );
    ensure!(
        task_row["status"] == "failed" || task_row["status"] == "blocked",
        "Run is not stopped after a failure"
    );
    inactive(db, task)?;
    let step = db
        .rows(
            "SELECT * FROM steps WHERE task=? AND name=?",
            &[&task, &step_name],
        )?
        .into_iter()
        .next()
        .context("unknown step")?;
    ensure!(step["state"] == "failed", "step is not failed");
    ensure!(
        Store::step(&step)?.kind == "agent",
        "only a failed agent step can be recovered"
    );
    let sid = step["id"].as_str().context("step id")?;
    let attempt = latest_failed(db, sid)?;
    let aid = attempt["id"].as_str().context("attempt id")?;
    let wid = attempt["worker"].as_str().context("worker id")?;
    let worker = db.worker(wid)?;
    ensure!(worker["step"] == sid, "worker step changed");
    ensure!(
        worker["status"] == "failed" || worker["status"] == "stopped",
        "worker is not stopped after failure"
    );
    let workspace = worktree(&worker, task)?;
    let run_workspace = git::task_workspace(db, task)?;
    let worker_head = head(workspace)?;
    let run_head = head(&run_workspace)?;
    let prior_merged = existing
        .as_ref()
        .is_some_and(|row| row["phase"] == "committed" && row["worker_commit"] == worker_head)
        && exact_prior_integration(
            db,
            task,
            wid,
            &worker_head,
            &run_workspace,
            expected_run_head,
        )?;
    ensure!(
        run_head == expected_run_head || prior_merged,
        "Run head changed; revalidate before recovery"
    );
    let receipt_tree = existing
        .as_ref()
        .and_then(|row| row["validated_tree"].as_str());
    let was_committed = if let Some(validated) = receipt_tree {
        let commit_key = existing.as_ref().unwrap()["commit_key"]
            .as_str()
            .context("commit key")?;
        let verified = committed_recovery(
            workspace,
            existing.as_ref().unwrap()["commit_key"]
                .as_str()
                .context("commit key")?,
            existing.as_ref().unwrap()["commit_parent"]
                .as_str()
                .context("commit parent")?,
            validated,
        )?;
        verified && !(commit_key.is_empty() && existing.as_ref().unwrap()["phase"] == "prepared")
    } else {
        ensure!(
            worker_head == expected_worker_head,
            "worker head changed; revalidate before recovery"
        );
        false
    };
    let agent_commit = existing_agent_commit(db, &worker, workspace, &worker_head)?;
    ensure!(
        existing
            .as_ref()
            .is_none_or(|row| row["step"] == sid && row["attempt"] == aid && row["worker"] == wid),
        "failed attempt changed during recovery"
    );
    let prior = if existing.is_none()
        && worker_head == expected_worker_head
        && git::run(workspace, &["status", "--porcelain"])?.is_empty()
    {
        db.rows("SELECT * FROM run_step_recoveries WHERE task=? AND step=? AND worker=? AND worker_commit=? AND phase='committed' ORDER BY created,rowid LIMIT 1",
            &[&task,&sid,&wid,&worker_head])?.into_iter().next()
    } else {
        None
    };
    let (validated_tree, commit_key, commit_parent) = if was_committed {
        let validated = receipt_tree.context("recovery receipt tree")?;
        ensure!(
            git::run(workspace, &["status", "--porcelain"])?.is_empty(),
            "worker changed after recovery commit"
        );
        (
            validated.to_owned(),
            existing.as_ref().unwrap()["commit_key"]
                .as_str()
                .context("commit key")?
                .to_owned(),
            existing.as_ref().unwrap()["commit_parent"]
                .as_str()
                .context("commit parent")?
                .to_owned(),
        )
    } else if let Some(previous) = &prior {
        // The first request committed the checked agent tree, but an authorized
        // Run push changed the expected head before integration. A new operator
        // request pins that current head and re-runs its checks on the same
        // provenance-bearing worker commit.
        let source_key = previous["commit_key"]
            .as_str()
            .context("prior commit key")?;
        let source_parent = previous["commit_parent"]
            .as_str()
            .context("prior commit parent")?;
        let pinned_tree = previous["validated_tree"].as_str().context("prior tree")?;
        ensure!(
            committed_recovery(workspace, source_key, source_parent, pinned_tree)?,
            "prior recovery commit is missing"
        );
        git::validate_scope(db, wid)?;
        checked_command(workspace, validation, settings.timeout_seconds)?;
        ensure!(
            git::run(workspace, &["status", "--porcelain"])?.is_empty()
                && git::run(workspace, &["rev-parse", "HEAD^{tree}"])? == pinned_tree,
            "validation changed the committed agent tree"
        );
        db.conn.execute("INSERT INTO run_step_recoveries(task,idempotency_key,request,step,attempt,worker,validated_tree,commit_key,commit_parent,phase,worker_commit,created) VALUES(?,?,?,?,?,?,?,?,?,'committed',?,?)",
            params![task,key,request.to_string(),sid,aid,wid,pinned_tree,source_key,source_parent,worker_head,now()])?;
        (
            pinned_tree.to_owned(),
            source_key.to_owned(),
            source_parent.to_owned(),
        )
    } else if agent_commit && existing.as_ref().is_none_or(|row| row["commit_key"] == "") {
        git::validate_scope(db, wid)?;
        let pinned = git::run(workspace, &["rev-parse", "HEAD^{tree}"])?;
        if let Some(recorded) = receipt_tree {
            ensure!(
                recorded == pinned,
                "agent commit changed since the recovery request"
            );
        } else {
            db.conn.execute("INSERT INTO run_step_recoveries(task,idempotency_key,request,step,attempt,worker,validated_tree,commit_key,commit_parent,phase,created) VALUES(?,?,?,?,?,?,?,'',?,'prepared',?)",
                params![task,key,request.to_string(),sid,aid,wid,pinned,expected_worker_head,now()])?;
        }
        ensure!(
            existing_agent_commit(db, &worker, workspace, expected_worker_head)?,
            "agent work changed before index reconciliation"
        );
        git::run(workspace, &["add", "--all"])?;
        ensure!(
            tree(workspace)? == pinned,
            "index reconciliation changed the agent tree"
        );
        checked_command(workspace, validation, settings.timeout_seconds)?;
        git::validate_scope(db, wid)?;
        ensure!(
            existing_agent_commit(db, &worker, workspace, expected_worker_head)?
                && git::run(workspace, &["status", "--porcelain"])?.is_empty()
                && tree(workspace)? == pinned,
            "validation changed the committed agent work"
        );
        db.conn.execute(
            "UPDATE run_step_recoveries SET phase='validated' WHERE task=? AND idempotency_key=?",
            params![task, key],
        )?;
        (pinned, String::new(), expected_worker_head.to_owned())
    } else {
        git::validate_scope(db, wid)?;
        ensure!(
            !git::run(workspace, &["status", "--porcelain"])?.is_empty(),
            "agent worktree has no changes to recover"
        );
        git::run(workspace, &["add", "--all"])?;
        git::run(workspace, &["diff", "--cached", "--check"])?;
        let staged = tree(workspace)?;
        ensure!(
            staged != git::run(workspace, &["rev-parse", "HEAD^{tree}"])?,
            "agent worktree has no content changes"
        );
        if let Some(pinned) = receipt_tree {
            ensure!(
                pinned == staged,
                "agent worktree changed since the recovery request; use a new key"
            );
        } else {
            db.conn.execute("INSERT INTO run_step_recoveries(task,idempotency_key,request,step,attempt,worker,validated_tree,commit_key,commit_parent,phase,created) VALUES(?,?,?,?,?,?,?,?,?,'prepared',?)",
                params![task,key,request.to_string(),sid,aid,wid,staged,key,expected_worker_head,now()])?;
        }
        checked_command(workspace, validation, settings.timeout_seconds)?;
        git::validate_scope(db, wid)?;
        ensure!(
            git::run(workspace, &["diff", "--quiet"]).is_ok()
                && git::run(workspace, &["ls-files", "--others", "--exclude-standard"])?.is_empty()
                && tree(workspace)? == staged,
            "validation changed the agent worktree; inspect and retry with a new key"
        );
        db.conn.execute(
            "UPDATE run_step_recoveries SET phase='validated' WHERE task=? AND idempotency_key=?",
            params![task, key],
        )?;
        ensure!(
            head(workspace)? == expected_worker_head,
            "worker head changed during validation; inspect before committing"
        );
        let message = format!(
            "Recover agent-authored work for {step_name}\n\nHorde-Recovery-Id: {key}\nHorde-Worker-Id: {wid}\nHorde-Attempt-Id: {aid}"
        );
        git::run(
            workspace,
            &[
                "-c",
                "user.name=Horde Recovery",
                "-c",
                "user.email=recovery@horde.sh",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                &message,
            ],
        )?;
        ensure!(
            git::run(workspace, &["rev-parse", "HEAD^{tree}"])? == staged,
            "committed tree changed after validation"
        );
        (staged, key.to_owned(), expected_worker_head.to_owned())
    };
    let committed = head(workspace)?;
    ensure!(
        committed_recovery(workspace, &commit_key, &commit_parent, &validated_tree)?,
        "recovery commit missing"
    );
    db.conn.execute("UPDATE run_step_recoveries SET phase='committed',worker_commit=? WHERE task=? AND idempotency_key=?",
        params![committed,task,key])?;
    let integration = git::integrate_expected(db, task, wid, validation, Some(expected_run_head))?;
    let integrated_head = integration["integrated_head"]
        .as_str()
        .context("integrated head")?;
    ensure!(
        git::run(&run_workspace, &["status", "--porcelain"])?.is_empty(),
        "combined validation left the Run workspace dirty; repair before acceptance"
    );
    let actual_tree = git::run(&run_workspace, &["rev-parse", "HEAD^{tree}"])?;
    let response = json!({"recovered":true,"task":task,"step":step_name,
        "worker_commit_sha":committed,"run_head_sha":integrated_head,
        "tree_sha":actual_tree,"branch_ref":format!("refs/heads/horde/{task}"),
        "validation":validation,"idempotency_key":key});
    let skipped = descendants(db, task, step_name)?;
    db.atomic(|| {
        ensure!(db.task(task)?["status"] == task_row["status"], "task changed during recovery");
        ensure!(db.worker(wid)?["status"] == worker["status"], "worker changed during recovery");
        let updated = db.conn.execute("UPDATE steps SET state='succeeded',result=? WHERE id=? AND task=? AND state='failed'",
            params![json!({"accepted":true,"recovered":true,"agent_authored":true,"attempt":aid,"worker":wid,"worker_commit_sha":committed,"run_head_sha":integrated_head,"validation":validation}).to_string(),sid,task])?;
        ensure!(updated == 1, "step changed during recovery");
        for child in &skipped {
            let changed = db.conn.execute("UPDATE steps SET state='pending',result=NULL WHERE task=? AND id=? AND state='skipped'",
                params![task,child["id"].as_str().context("child step")?])?;
            ensure!(changed == 1, "dependent step changed during recovery");
        }
        db.conn.execute("UPDATE workers SET status='idle',updated=? WHERE id=?", params![now(),wid])?;
        db.conn.execute("DELETE FROM claims WHERE worker=?", [wid])?;
        db.conn.execute("UPDATE tasks SET status='running' WHERE id=?", [task])?;
        db.conn.execute("UPDATE run_step_recoveries SET phase='succeeded',response=? WHERE task=? AND idempotency_key=?",
            params![response.to_string(),task,key])?;
        db.event(task,"run.step_recovered",response.clone())?;
        Ok(())
    })?;
    Ok(response)
}

#[cfg(test)]
mod tests;
