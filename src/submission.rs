//! Caller receipts deduplicate submissions before live discovery or dispatch.
use crate::store::Store;
use anyhow::{Context, Result, ensure};
use serde_json::Value;

pub fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS submission_receipts(request_id TEXT PRIMARY KEY,request_hash TEXT NOT NULL,task TEXT NOT NULL REFERENCES tasks(id),response TEXT NOT NULL)")?;
    Ok(())
}

pub fn submit(db: &Store, args: &Value) -> Result<Value> {
    let request = args["request_id"]
        .as_str()
        .context("request_id must be a string")?;
    ensure!(
        !request.trim().is_empty() && request.len() <= 256,
        "request_id must contain 1..256 bytes"
    );
    let hash = crate::store::hash(&serde_json::to_vec(args)?);
    db.atomic(|| {
        if let Some(receipt) = db
            .rows(
                "SELECT request_hash,response FROM submission_receipts WHERE request_id=?",
                &[&request],
            )?
            .first()
        {
            ensure!(
                receipt["request_hash"] == hash,
                "submission request_id reused with different assignment"
            );
            return Ok(serde_json::from_str(
                receipt["response"].as_str().context("submission receipt")?,
            )?);
        }
        let mut input = args.clone();
        input
            .as_object_mut()
            .context("submission arguments")?
            .remove("request_id");
        let response = crate::protocol::dispatch(db, "submit_task", input, None)?;
        db.conn.execute(
            "INSERT INTO submission_receipts VALUES(?,?,?,?)",
            rusqlite::params![
                request,
                hash,
                response["id"].as_str().context("submitted task")?,
                response.to_string()
            ],
        )?;
        Ok(response)
    })
}
