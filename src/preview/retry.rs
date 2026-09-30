use super::*;
/// Operator retry is a durable request; it reconciles the same job and artifact.
pub fn retry(db: &Store, task: &str, head: &str, key: &str) -> Result<Value> {
    ensure!(
        !key.is_empty()
            && key.len() <= 200
            && key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid preview retry key"
    );
    let request = json!({"task":task,"expected_head":head});
    let previous = db.rows(
        "SELECT request,response FROM preview_retries WHERE task=? AND key=?",
        &[&task, &key],
    )?;
    if let Some(r) = previous.first() {
        ensure!(
            serde_json::from_str::<Value>(r["request"].as_str().context("retry request")?)?
                == request,
            "preview retry key reused with changed request"
        );
        return Ok(serde_json::from_str(
            r["response"].as_str().context("retry response")?,
        )?);
    }
    let rows = db.rows(
        "SELECT * FROM preview_jobs WHERE task=? ORDER BY rowid DESC LIMIT 1",
        &[&task],
    )?;
    let job = rows.first().context("Run has no preview job")?;
    ensure!(
        job["head"] == head && job["phase"] == "held",
        "retry requires exact held preview revision"
    );
    let project = crate::projects::task_project(db, task)?;
    let (policy, generation, _) = policy(db, &project)?.context("preview policy")?;
    pipeline::fresh(db, job, &policy, &generation)?;
    let response = json!({"run_id":task,"job_id":job["id"],"queued":true});
    db.atomic(|| {
        db.conn.execute(
            "INSERT INTO preview_retries VALUES(?,?,?,?)",
            params![task, key, request.to_string(), response.to_string()],
        )?;
        db.conn.execute(
            "UPDATE preview_jobs SET phase='queued',error=NULL WHERE id=? AND phase='held'",
            [job["id"].as_str()],
        )?;
        db.event(
            task,
            "run.preview_retry_queued",
            json!({"id":job["id"],"idempotency_key":key}),
        )?;
        Ok(())
    })?;
    Ok(response)
}
