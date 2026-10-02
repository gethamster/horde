//! Bounded operator-feedback acknowledgement evidence without message contents.
use crate::store::{Store, operator_id};
use anyhow::{Context, Result};
use serde_json::{Value, json};

pub(super) fn feedback_receipts(db: &Store, task: &str) -> Result<Vec<Value>> {
    db.rows(
        "WITH acks AS MATERIALIZED (
           SELECT seq,json_extract(data,'$.message') AS message,json_extract(data,'$.worker') AS worker
           FROM events WHERE task=? AND kind='message.acknowledged'
         )
         SELECT m.id AS message_id,r.worker AS worker_id,r.ack,m.created,MAX(a.seq) AS ack_seq
         FROM messages m JOIN receipts r ON r.message=m.id
         JOIN workers w ON w.id=r.worker AND w.task=m.task
         LEFT JOIN acks a ON a.message=m.id AND a.worker=r.worker
         WHERE m.task=? AND m.sender=? AND m.actionable=1
         GROUP BY m.id,r.worker ORDER BY m.seq DESC,r.worker LIMIT 1000",
        &[&task, &task, &operator_id(task)],
    )
}

pub(super) fn checkpoint_event(db: &Store, task: &str) -> Result<Value> {
    let rows = db.rows("SELECT seq,data FROM events WHERE task=? AND kind='run.checkpoint_verified' ORDER BY seq DESC LIMIT 1", &[&task])?;
    let Some(row) = rows.first() else {
        return Ok(Value::Null);
    };
    let payload: Value =
        serde_json::from_str(row["data"].as_str().context("checkpoint event data")?)?;
    Ok(
        json!({"validation_id":payload["validation_id"],"commit_sha":payload["commit_sha"],"event_seq":row["seq"]}),
    )
}
