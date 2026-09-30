use super::*;
pub fn is_review(db: &Store, task: &str, row: &Value) -> Result<bool> {
    let project = crate::projects::task_project(db, task)?;
    let Some((p, _, _)) = policy(db, &project)? else {
        return Ok(false);
    };
    let name = row["name"].as_str().unwrap_or("");
    Ok(p.enabled
        && (name == p.review_step || name.starts_with(&format!("{}.preview-", p.review_step))))
}
/// Scheduler calls this immediately before dispatching a configured review agent.
pub fn prepare_review(db: &Store, task: &str, row: &Value) -> Result<()> {
    let project = crate::projects::task_project(db, task)?;
    let Some((p, generation, _)) = policy(db, &project)? else {
        return Ok(());
    };
    if !p.enabled {
        return Ok(());
    }
    let name = row["name"].as_str().context("step name")?;
    if name != p.review_step && !name.starts_with(&format!("{}.preview-", p.review_step)) {
        return Ok(());
    }
    let spec = Store::step(row)?;
    ensure!(
        spec.kind == "agent" && spec.environment.is_none(),
        "preview review requires an agent step"
    );
    let settings: crate::config::Settings =
        serde_json::from_str(db.task(task)?["settings"].as_str().context("settings")?)?;
    let settings = crate::execution_selection::apply(db, task, &settings)?;
    let executor = settings
        .executor(row["dispatch_role"].as_str().unwrap_or(&spec.role))
        .context("review executor")?;
    ensure!(
        executor.kind != "simulated"
            && spec.workspace != Some(crate::template::CommandWorkspace::Checkout),
        "preview review requires a real executor in a Run worktree"
    );
    let before = crate::decision::review::observed_head(db, task).context("Run head")?;
    let main = crate::run::main_head(db, task)?;
    let main = main["commit_sha"].as_str().context("main head")?;
    let integrated = crate::run::integrate_main(db, task, &before, main)?;
    db.conn.execute("INSERT INTO preview_reviews VALUES(?,?,?,?,?,?,NULL) ON CONFLICT(step) DO UPDATE SET head=excluded.head,tree=excluded.tree,main_head=excluded.main_head,generation=excluded.generation,attempt=NULL",params![row["id"].as_str(),task,integrated["head_sha"].as_str(),integrated["tree_sha"].as_str(),main,generation])?;
    db.event(task,"run.preview_review_started",json!({"step":row["id"],"commit_sha":integrated["head_sha"],"tree_sha":integrated["tree_sha"],"expected_main_head":main}))?;
    Ok(())
}
pub(super) fn reviewed(
    db: &Store,
    task: &str,
    head: &str,
    tree: &str,
    generation: &str,
) -> Result<Option<String>> {
    let rows=db.rows("SELECT r.main_head,s.result FROM preview_reviews r JOIN steps s ON s.id=r.step JOIN attempts a ON a.id=r.attempt AND a.step=s.id JOIN preview_review_execution x ON x.attempt=a.id AND x.head=r.head AND x.tree=r.tree AND x.executor!='simulated' WHERE r.task=? AND r.head=? AND r.tree=? AND r.generation=? AND s.state='succeeded' AND a.state='succeeded' ORDER BY s.rowid DESC LIMIT 1",&[&task,&head,&tree,&generation])?;
    let Some(r) = rows.first() else {
        return Ok(None);
    };
    let result: Value = serde_json::from_str(r["result"].as_str().unwrap_or("null"))?;
    ensure!(
        result["accepted"] == true,
        "review agent did not accept the current tree"
    );
    Ok(Some(r["main_head"].as_str().context("review main")?.into()))
}
pub fn bind_attempt(db: &Store, step: &str, attempt: &str) -> Result<()> {
    db.conn.execute(
        "UPDATE preview_reviews SET attempt=? WHERE step=?",
        params![attempt, step],
    )?;
    Ok(())
}
pub(super) fn schedule_review(db: &Store, task: &str, p: &Policy) -> Result<()> {
    let rows = db.rows(
        "SELECT * FROM steps WHERE task=? AND name=?",
        &[&task, &p.review_step],
    )?;
    let base = rows
        .first()
        .context("preview pipeline requires its configured review agent step")?;
    let mut spec = Store::step(base)?;
    ensure!(
        spec.kind == "agent",
        "preview review must use agent executor"
    );
    let count: i64 = db.conn.query_row(
        "SELECT COUNT(*) FROM steps WHERE task=? AND name LIKE ?",
        params![task, format!("{}.preview-%", p.review_step)],
        |r| r.get(0),
    )?;
    ensure!(
        count < 12,
        "preview review retry limit reached; operator reconciliation required"
    );
    spec.id = format!("{}.preview-{}", p.review_step, &crate::store::id()[..8]);
    spec.needs = db
        .steps(task)?
        .iter()
        .filter(|r| r["state"] == "succeeded")
        .filter_map(|r| r["name"].as_str().map(str::to_owned))
        .collect();
    spec.when = None;
    spec.attempts = 1;
    spec.instructions = format!(
        "Review the exact integrated Run tree after main integration and any feedback revision. Do not accept stale prior evidence. Verify the requested behavior and report accepted:true only when current work meets all acceptance criteria. {}",
        spec.instructions
    );
    crate::protocol::dispatch(db, "add_steps", json!({"task":task,"steps":[spec]}), None)?;
    db.event(task, "run.preview_review_queued", json!({"step":spec.id}))?;
    Ok(())
}

/// Verify the selected executor actually reviews its registered worktree at the
/// post-integration head. Stale clean retry worktrees may only fast-forward.
pub fn verify_execution(
    db: &Store,
    row: &Value,
    attempt: &str,
    worker: &str,
    settings: &crate::config::Settings,
    role: &str,
    workspace: &Path,
) -> Result<()> {
    let task = row["task"].as_str().context("task")?;
    if !is_review(db, task, row)? {
        return Ok(());
    }
    let spec = Store::step(row)?;
    let effective = settings
        .executor(role)
        .context("review executor unavailable")?;
    ensure!(
        effective.kind != "simulated"
            && spec.environment.is_none()
            && !effective.kind.is_empty()
            && spec.workspace != Some(crate::template::CommandWorkspace::Checkout),
        "preview review requires real executor in its Run worktree"
    );
    let r = db.rows(
        "SELECT * FROM preview_reviews WHERE step=? AND attempt=?",
        &[&row["id"].as_str(), &attempt],
    )?;
    let r = r.first().context("review attempt has no exact binding")?;
    let w = db.worker(worker)?;
    ensure!(w["step"] == row["id"], "review worker step mismatch");
    let registered = Path::new(w["workspace"].as_str().context("review worker workspace")?);
    ensure!(
        registered.canonicalize()? == workspace.canonicalize()?
            && crate::git::run(workspace, &["branch", "--show-current"])?
                == w["branch"].as_str().context("worker branch")?,
        "review must use its registered worktree"
    );
    ensure!(
        crate::git::run(workspace, &["status", "--porcelain"])?.is_empty(),
        "review retry worktree has unreviewed changes"
    );
    let head = r["head"].as_str().context("review head")?;
    if crate::git::run(workspace, &["rev-parse", "HEAD"])? != head {
        crate::git::run(workspace, &["merge", "--ff-only", head])
            .context("review retry workspace diverged; reconcile its provenance")?;
    }
    ensure!(
        crate::git::run(workspace, &["rev-parse", "HEAD"])? == head
            && crate::git::run(workspace, &["rev-parse", "HEAD^{tree}"])?
                == r["tree"].as_str().context("review tree")?,
        "review workspace does not match integrated tree"
    );
    db.conn.execute(
        "INSERT OR REPLACE INTO preview_review_execution VALUES(?,?,?,?,?,?)",
        params![
            attempt,
            row["id"].as_str(),
            head,
            r["tree"].as_str(),
            effective.kind,
            workspace.to_str()
        ],
    )?;
    db.event(task,"run.preview_review_execution",json!({"step":row["id"],"attempt":attempt,"commit_sha":head,"tree_sha":r["tree"],"executor":effective.kind}))?;
    Ok(())
}
