//! Controller-owned root tasks executed by a selected runtime over federation.
use crate::{config::Settings, store::Store};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

pub fn submit(db: &Store, args: &Value) -> Result<Value> {
    let peer =
        crate::runtime_directory::resolve(db, args["on"].as_str().context("runtime required")?)?;
    let network = crate::federation::config(db)?;
    ensure!(
        network.delegate_peers.contains(&peer),
        "runtime is not approved for delegation"
    );
    let repo = Path::new(args["repo"].as_str().context("repository required")?).canonicalize()?;
    ensure!(
        crate::git::run(&repo, &["status", "--porcelain"])?.is_empty(),
        "remote work needs a clean repository; commit intended source changes and keep credential files outside the repository"
    );
    let snapshot = crate::federation::snapshot(&repo)?;
    let settings = Settings::load(&repo)?;
    let objective = args["objective"].as_str().context("objective required")?;
    let templates = crate::template::load_templates(&crate::branding::templates(&repo))?;
    let plan = crate::template::compile(
        args["template"]
            .as_str()
            .unwrap_or(&settings.default_template),
        &templates,
        BTreeMap::from([("task".into(), objective.into())]),
    )?;
    let execution = args
        .get("execution")
        .map(|input| crate::execution_selection::prepare(db, input, None))
        .transpose()?;
    db.atomic(|| {
        let id = db.submit(objective, &repo, &settings, &plan)?;
        if let Some(execution) = &execution {
            crate::execution_selection::pin(db, &id, execution)?;
            crate::execution_selection::validate_target(db, &id, Some(&peer))?;
        }
        for record in args["context"].as_array().into_iter().flatten() {
            crate::delegation::update_context(db, &id, record)?;
        }
        db.artifact(&id, None, "caller-snapshot", &serde_json::to_vec(&snapshot)?, &json!({}), false)?;
        db.conn.execute("INSERT INTO remote_links(task,peer,state,request) VALUES(?,?,'pending',?)", params![id,peer,id])?;
        if settings.autonomy {
            db.conn.execute("UPDATE tasks SET status='remote' WHERE id=?", [&id])?;
        }
        db.event(&id, "task.remote_queued", json!({"runtime":peer,"source_commit":snapshot["commit"]}))?;
        Ok(json!({"id":id,"runtime":peer,"status":if settings.autonomy {"remote"} else {"waiting"}}))
    })
}

/// Record remote execution results without representing them as a local merge.
/// Federation checks context version, child acceptance, and remote cleanup first.
pub fn complete(db: &Store, id: &str, reply: &Value, base: &Value) -> Result<()> {
    ensure!(
        crate::delegation::tree(db, id)?["parent"].is_null(),
        "remote root required"
    );
    let reported_status = reply["task"]["status"].as_str().context("remote status")?;
    ensure!(
        ["succeeded", "failed", "cancelled"].contains(&reported_status),
        "terminal result required"
    );
    db.atomic(|| {
        // Cancellation may commit while the remote status response is in flight.
        let status = if db.task(id)?["status"] == "cancelled" { "cancelled" } else { reported_status };
        let snapshot = if reported_status == "succeeded" {
            Some(db.artifact(id, None, "remote-snapshot", &serde_json::to_vec(&reply["snapshot"])?, &json!({"base":base}), false)?)
        } else { None };
        let result = json!({"status":status,"remote_status":reported_status,"remote_steps":reply["steps"],"snapshot_artifact":snapshot,"local_changes_applied":false,"result":"Remote execution finished. Any returned snapshot is retained for review; local files were not changed."});
        for step in db.steps(id)? {
            let remote = reply["steps"].as_array().and_then(|steps| steps.iter().find(|remote| remote["name"] == step["name"]));
            let state = if status == "cancelled" { "cancelled" } else { remote.and_then(|step| step["state"].as_str()).unwrap_or(if status == "succeeded" {"succeeded"} else {"failed"}) };
            db.conn.execute("UPDATE steps SET state=?,result=? WHERE id=?", params![state,remote.map(|step| step["result"].clone()).unwrap_or(result.clone()).as_str().map(str::to_owned).unwrap_or_else(|| result.to_string()),step["id"].as_str()])?;
        }
        db.conn.execute("UPDATE tasks SET status=? WHERE id=?", params![status,id])?;
        db.conn.execute("UPDATE remote_links SET state='done' WHERE task=?", [id])?;
        for (name, data) in [("federation.metrics", reply["metrics"].clone()), ("federation.result", result.clone())] {
            db.conn.execute("INSERT INTO external_ops VALUES(?,?,'done',?) ON CONFLICT(task,name) DO UPDATE SET data=excluded.data", params![id,name,data.to_string()])?;
        }
        if let Some(outputs) = reply.get("outputs") {
            db.conn.execute("INSERT OR REPLACE INTO workflow_outputs VALUES(?,?)", params![id,outputs.to_string()])?;
        }
        db.event(id, "task.finished", result)?;
        Ok(())
    })
}

/// Materialize a completed root's result in an isolated review repository.
pub fn result(db: &Store, id: &str) -> Result<Value> {
    ensure!(
        db.task(id)?["status"] == "succeeded",
        "remote task has not succeeded"
    );
    ensure!(
        crate::delegation::tree(db, id)?["parent"].is_null(),
        "use child integration for delegated results"
    );
    ensure!(
        !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
        "invalid task identity"
    );
    let rows = db.rows("SELECT a.hash FROM artifact_links a JOIN remote_links r ON r.task=a.task WHERE a.task=? AND a.name='remote-snapshot' AND r.state='done' ORDER BY a.rowid DESC LIMIT 1", &[&id])?;
    let hash = rows
        .first()
        .and_then(|row| row["hash"].as_str())
        .context("remote result snapshot is unavailable")?;
    let bytes = std::fs::read(db.root.join("artifacts").join(hash))?;
    ensure!(
        crate::store::hash(&bytes) == hash,
        "remote result artifact is corrupt"
    );
    let snapshot: Value = serde_json::from_slice(&bytes)?;
    let parent = db.root.join("remote-results");
    std::fs::create_dir_all(&parent)?;
    let destination = parent.join(id);
    if !destination.try_exists()? {
        let temporary = parent.join(format!(".{}", crate::store::id()));
        std::fs::create_dir(&temporary)?;
        let prepared = prepare_result(&temporary, id, hash, &snapshot);
        if let Err(error) = prepared {
            std::fs::remove_dir_all(&temporary)?;
            return Err(error);
        }
        if let Err(error) = std::fs::rename(&temporary, &destination) {
            std::fs::remove_dir_all(&temporary)?;
            // A concurrent reader may already have materialized the same result.
            ensure!(
                destination.is_dir(),
                "cannot install remote result: {error}"
            );
        }
    }
    verify_result(&destination, id, hash)?;
    Ok(
        json!({"task":id,"result_workspace":destination.join("repo"),"snapshot_artifact":hash,"local_changes_applied":false}),
    )
}

fn prepare_result(path: &Path, id: &str, hash: &str, snapshot: &Value) -> Result<()> {
    let repo = path.join("repo");
    crate::federation::unpack(snapshot, &repo)?;
    let head = result_git(&repo, &["rev-parse", "HEAD"])?;
    crate::secrets::write_private(
        &path.join("result.json"),
        &serde_json::to_vec(&json!({"task":id,"artifact":hash,"head":head}))?,
    )?;
    Ok(())
}

fn verify_result(path: &Path, id: &str, hash: &str) -> Result<()> {
    ensure!(
        !std::fs::symlink_metadata(path)?.file_type().is_symlink(),
        "result directory cannot be a symlink"
    );
    let metadata: Value = serde_json::from_slice(&std::fs::read(path.join("result.json"))?)?;
    ensure!(
        metadata["task"] == id && metadata["artifact"] == hash,
        "existing result belongs to another task or snapshot"
    );
    let repo = path.join("repo");
    ensure!(
        !std::fs::symlink_metadata(&repo)?.file_type().is_symlink(),
        "result repository cannot be a symlink"
    );
    ensure!(
        metadata["head"] == result_git(&repo, &["rev-parse", "HEAD"])?,
        "result checkout has changed; existing files were preserved"
    );
    ensure!(
        result_git(&repo, &["status", "--porcelain"])?.is_empty(),
        "result checkout has edits; existing files were preserved"
    );
    Ok(())
}

fn result_git(repo: &Path, args: &[&str]) -> Result<String> {
    Ok(
        String::from_utf8(crate::federation::import_git(repo, args)?)?
            .trim()
            .to_owned(),
    )
}
