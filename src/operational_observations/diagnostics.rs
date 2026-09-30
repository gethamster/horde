//! Bounded read-only structural diagnostics. Never returns transcript or secret material.
use super::*;

pub fn diagnostics(
    db: &Store,
    project: &str,
    task: &str,
    cursor: i64,
    limit: usize,
) -> Result<Value> {
    ensure!(
        cursor >= 0 && (1..=50).contains(&limit),
        "diagnostics cursor/limit out of bounds"
    );
    ensure!(
        crate::projects::task_project(db, task)? == project,
        "diagnostic project scope violation"
    );
    let context = crate::run::run_context(db, task)?;
    let attempts=db.rows("SELECT a.id AS attempt_id,a.step AS step_id,a.state,a.started,a.finished,CASE WHEN EXISTS(SELECT 1 FROM events e WHERE e.task=s.task AND e.kind='step.finished' AND json_extract(e.data,'$.attempt')=a.id AND json_extract(e.data,'$.failure_code')='credential_refresh_required') THEN 'credential_refresh_required' WHEN a.state='uncertain' THEN 'worker_interrupted' WHEN a.state='failed' THEN 'attempt_failed' ELSE NULL END AS failure_code,CASE WHEN instr(json_extract(a.result,'$.error'),'access token renewal requires the controller refresh owner')>0 THEN 'suspected_credential_refresh_required' ELSE NULL END AS historical_hint FROM attempts a JOIN steps s ON s.id=a.step WHERE s.task=? ORDER BY a.started DESC,a.id LIMIT 50",&[&task])?;
    let credentials=db.rows("SELECT a.id AS account_id,a.state,a.authenticated,p.kind,p.credential_version,p.expires_at FROM accounts a JOIN account_grants g ON g.account=a.id LEFT JOIN auth_profiles p ON p.account=a.id WHERE g.project=? ORDER BY a.id LIMIT 50",&[&project])?;
    let reservations = db.rows(
        "SELECT state,count(*) AS count FROM account_reservations WHERE project=? GROUP BY state",
        &[&project],
    )?;
    let previews=db.rows("SELECT id,head AS commit_sha,tree AS tree_sha,main_head,recipe,generation,phase,json_extract(receipt,'$.image') AS artifact_digest FROM preview_jobs WHERE task=? ORDER BY created DESC LIMIT 10",&[&task])?;
    let checkpoints=db.rows("SELECT seq,json_extract(data,'$.commit_sha') AS commit_sha,json_extract(data,'$.tree_sha') AS tree_sha,json_extract(data,'$.artifact_digest') AS artifact_digest,json_extract(data,'$.build_id') AS build_id,json_extract(data,'$.validation_id') AS validation_id FROM events WHERE task=? AND kind='run.checkpoint_verified' ORDER BY seq DESC LIMIT 1",&[&task])?;
    let events = db.rows(
        "SELECT seq,kind,created FROM events WHERE task=? AND seq>? ORDER BY seq LIMIT ?",
        &[&task, &cursor, &(limit as i64)],
    )?;
    let current_head=db.rows("SELECT commit_id AS commit_sha FROM integrations WHERE task=? AND state='succeeded' ORDER BY created DESC,rowid DESC LIMIT 1",&[&task])?;
    Ok(
        json!({"schema_version":1,"task_id":task,"project_id":project,"tenant_id":context["tenant_id"],"thread_id":context["thread_id"],"brief_id":context["brief_id"],"attempts":attempts,"credential_metadata":credentials,"reservation_counts":reservations,"previews":previews,"checkpoint":checkpoints.first(),"integrated_head":current_head,"events":events,"next_cursor":events.last().map(|v|v["seq"].clone()).unwrap_or(json!(cursor)),"guidance":{"credential_refresh_required":"Renew credentials through the controller credential owner; reconcile the same Run using the existing eligible account pool.","worker_interrupted":"Inspect recorded external effects and reconcile the same Run before retrying.","preview_held":"Inspect reviewed head, validation and publication provenance; retry the same artifact only after reconciliation."}}),
    )
}
