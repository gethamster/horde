//! Caller receipts deduplicate submissions before live discovery or dispatch.
use crate::store::Store;
use anyhow::{Context, Result, ensure};
use serde_json::Value;

pub fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS submission_receipts(request_id TEXT PRIMARY KEY,request_hash TEXT NOT NULL,task TEXT NOT NULL REFERENCES tasks(id),response TEXT NOT NULL)")?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS project_submission_receipts(project TEXT NOT NULL REFERENCES projects(id),request_id TEXT NOT NULL,request_hash TEXT NOT NULL,task TEXT NOT NULL REFERENCES tasks(id),response TEXT NOT NULL,PRIMARY KEY(project,request_id))")?;
    Ok(())
}

/// Find a matching historical submission before touching its original checkout.
pub fn receipt_project(db: &Store, args: &Value) -> Result<Option<String>> {
    let Some(request) = args["request_id"].as_str() else {
        return Ok(None);
    };
    let candidates = db.rows("SELECT project,request_hash,0 AS legacy FROM project_submission_receipts WHERE request_id=? UNION ALL SELECT 'default' AS project,request_hash,1 AS legacy FROM submission_receipts WHERE request_id=?", &[&request, &request])?;
    let projects: std::collections::BTreeSet<String> = candidates
        .iter()
        .filter_map(|row| row["project"].as_str().map(str::to_owned))
        .collect();
    let mut matched: Option<String> = None;
    for row in candidates {
        let project = row["project"].as_str().context("receipt project")?;
        let mut input = args.clone();
        if row["legacy"] == 1 {
            input
                .as_object_mut()
                .context("submission arguments")?
                .remove("project");
        } else {
            input["project"] = serde_json::json!(project);
        }
        let hash = crate::store::hash(&serde_json::to_vec(&input)?);
        if row["request_hash"] == hash {
            ensure!(
                matched.as_deref().is_none_or(|old| old == project),
                "submission receipt is ambiguous; select a project explicitly"
            );
            matched = Some(project.to_owned());
        }
    }
    // With a unique receipt owner, reject changed retries using the durable
    // assignment even if its original checkout no longer exists.
    Ok(matched.or_else(|| {
        (projects.len() == 1)
            .then(|| projects.into_iter().next())
            .flatten()
    }))
}

pub fn existing(db: &Store, args: &Value) -> Result<Option<Value>> {
    let Some(request) = args["request_id"].as_str() else {
        return Ok(None);
    };
    let project = args["project"].as_str().context("submission project")?;
    let mut legacy_args = args.clone();
    legacy_args
        .as_object_mut()
        .context("submission arguments")?
        .remove("project");
    let legacy_hash = crate::store::hash(&serde_json::to_vec(&legacy_args)?);
    let hash = crate::store::hash(&serde_json::to_vec(args)?);
    let rows = db.rows("SELECT request_hash,response,0 AS legacy FROM project_submission_receipts WHERE project=? AND request_id=? UNION ALL SELECT request_hash,response,1 AS legacy FROM submission_receipts WHERE request_id=? AND ?='default' ORDER BY legacy LIMIT 1", &[&project,&request,&request,&project])?;
    let Some(receipt) = rows.first() else {
        return Ok(None);
    };
    ensure!(
        receipt["request_hash"] == hash
            || (receipt["legacy"] == 1 && receipt["request_hash"] == legacy_hash),
        "submission request_id reused with different assignment"
    );
    let response: Value =
        serde_json::from_str(receipt["response"].as_str().context("submission receipt")?)?;
    crate::projects::authorize_task(
        db,
        project,
        response["id"].as_str().context("receipt task")?,
    )?;
    Ok(Some(response))
}

pub fn submit(db: &Store, args: &Value) -> Result<Value> {
    let request = args["request_id"]
        .as_str()
        .context("request_id must be a string")?;
    ensure!(
        !request.trim().is_empty() && request.len() <= 256,
        "request_id must contain 1..256 bytes"
    );
    let project = crate::projects::resolve(
        db,
        args["project"]
            .as_str()
            .unwrap_or(crate::projects::DEFAULT_PROJECT),
    )?;
    let mut canonical = args.clone();
    canonical["project"] = serde_json::json!(project);
    let hash = crate::store::hash(&serde_json::to_vec(&canonical)?);
    db.atomic(|| {
        if let Some(receipt) = existing(db, &canonical)? {
            return Ok(receipt);
        }
        let mut input = canonical.clone();
        input
            .as_object_mut()
            .context("submission arguments")?
            .remove("request_id");
        let response = crate::protocol::dispatch(db, "submit_task", input, None)?;
        db.conn.execute(
            "INSERT INTO project_submission_receipts VALUES(?,?,?,?,?)",
            rusqlite::params![
                project,
                request,
                hash,
                response["id"].as_str().context("submitted task")?,
                response.to_string()
            ],
        )?;
        Ok(response)
    })
}
