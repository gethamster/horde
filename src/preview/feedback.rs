//! Read-only review criteria from authenticated operator browser feedback.
use super::*;

const MAX_FEEDBACK: usize = 32;
const MAX_BYTES: usize = 64 * 1024;

pub(super) fn snapshot(db: &Store, task: &str) -> Result<Value> {
    let binding = crate::run::run_context(db, task)?;
    if binding["thread_id"].as_str().is_none() || binding["brief_id"].as_str().is_none() {
        return Ok(json!([]));
    }
    let rows = db.rows(
        "SELECT m.seq,m.id,m.body,m.refs FROM messages m JOIN workers w ON w.id=m.sender WHERE m.task=? AND m.sender=? AND w.task=m.task AND w.status=? AND m.id LIKE 'adam-feedback:%' AND m.actionable=1 AND json_extract(m.refs,'$.run_id')=? AND json_extract(m.refs,'$.project_id')=? AND json_extract(m.refs,'$.tenant_id')=? AND json_extract(m.refs,'$.thread_id')=? AND json_extract(m.refs,'$.brief_id')=? ORDER BY m.seq LIMIT ?",
        &[&task, &crate::store::operator_id(task), &crate::store::OPERATOR_STATUS, &task, &binding["project"].as_str(), &binding["tenant_id"].as_str(), &binding["thread_id"].as_str(), &binding["brief_id"].as_str(), &((MAX_FEEDBACK + 1) as i64)],
    )?;
    ensure!(
        rows.len() <= MAX_FEEDBACK,
        "preview feedback exceeds review context limit; reconcile criteria"
    );
    let feedback = rows.iter().map(|row| -> Result<Value> {
        let refs: Value = serde_json::from_str(row["refs"].as_str().context("feedback refs")?)?;
        let body = row["body"].as_str().context("feedback body")?;
        let id = refs["feedback_id"].as_str().context("feedback identity")?;
        let target = refs["target_commit_sha"].as_str().context("feedback target")?;
        ensure!(!id.is_empty() && id.len() <= 256 && !body.trim().is_empty() && body.len() <= MAX_BYTES && target.len() == 40 && target.bytes().all(|b| b.is_ascii_hexdigit()), "invalid authorized preview feedback; reconcile criteria");
        Ok(json!({"sequence":row["seq"],"message_id":row["id"],"feedback_id":id,"target_commit_sha":target,"body":body,"path":refs["path"],"line":refs["line"]}))
    }).collect::<Result<Vec<_>>>()?;
    let value = json!(feedback);
    ensure!(
        serde_json::to_vec(&value)?.len() <= MAX_BYTES,
        "preview feedback exceeds review context limit; reconcile criteria"
    );
    Ok(value)
}

pub(super) fn fingerprint(value: &Value) -> Result<String> {
    Ok(crate::store::hash(&serde_json::to_vec(value)?))
}

pub(super) fn current(db: &Store, task: &str, step: &str, value: &Value) -> Result<bool> {
    let rows = db.rows("SELECT json_extract(data,'$.feedback_fingerprint') AS fingerprint FROM events WHERE task=? AND kind='run.preview_review_started' AND json_extract(data,'$.step')=? ORDER BY seq DESC LIMIT 1", &[&task, &step])?;
    let saved = rows.first().and_then(|r| r["fingerprint"].as_str());
    // Pre-upgrade reviews remain valid only when there is no browser feedback.
    Ok(match saved {
        Some(saved) => saved == fingerprint(value)?,
        None => value.as_array().is_some_and(Vec::is_empty),
    })
}
