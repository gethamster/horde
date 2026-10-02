use super::{Store, operator_id};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

impl Store {
    /// Recover acknowledged operator input consumed by this step, never another
    /// worker's conversation or feedback already consumed by an earlier step.
    pub(crate) fn retry_operator_feedback(
        &self,
        task: &str,
        step: &str,
        worker: &str,
        attempt: &str,
    ) -> Result<Value> {
        let owned: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts a JOIN steps s ON s.id=a.step \
             JOIN workers w ON w.id=a.worker WHERE a.id=?1 AND a.step=?2 \
             AND a.worker=?3 AND s.task=?4 AND w.task=?4)",
            rusqlite::params![attempt, step, worker, task],
            |row| row.get(0),
        )?;
        ensure!(owned, "retry feedback invocation ownership mismatch");
        let retry: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts WHERE step=?1 AND worker=?2 \
             AND id!=?3 AND state IN ('failed','interrupted','uncertain','cancelled','waiting'))",
            rusqlite::params![step, worker, attempt],
            |row| row.get(0),
        )?;
        let records = if retry {
            self.rows(
                "WITH first_start AS (SELECT MIN(seq) AS seq FROM events \
                   WHERE task=?1 AND kind='step.started' AND json_extract(data,'$.step')=?2), \
                 acknowledgements AS MATERIALIZED (SELECT json_extract(data,'$.message') AS message, \
                   MIN(seq) AS ack_seq FROM events WHERE task=?1 AND kind='message.acknowledged' \
                   AND json_extract(data,'$.worker')=?3 AND seq>(SELECT seq FROM first_start) \
                   GROUP BY json_extract(data,'$.message')) \
                 SELECT m.id,m.seq,m.sender,m.body,m.created,a.ack_seq \
                 FROM messages m JOIN receipts r ON r.message=m.id \
                 JOIN acknowledgements a ON a.message=m.id \
                 WHERE m.task=?1 AND r.worker=?3 AND r.ack=1 AND m.actionable=1 AND m.sender=?4 \
                 ORDER BY m.seq LIMIT 101",
                &[&task, &step, &worker, &operator_id(task)],
            )?
        } else {
            Vec::new()
        };
        ensure!(
            records.len() <= 100,
            "retry operator feedback exceeds 100 messages; consolidate feedback before retrying"
        );
        ensure!(
            serde_json::to_vec(&records)
                .context("serialize retry feedback")?
                .len()
                <= 256 * 1024,
            "retry operator feedback exceeds 256 KiB; consolidate feedback before retrying"
        );
        Ok(json!({
            "task":task,"worker":worker,"step":step,"records":records,
            "instruction":"These operator messages were already acknowledged by this worker during this step. Preserve their unfinished intent when recovering the attempt. Do not acknowledge or resend them, or repeat effects already verified complete. Later message sequences take precedence over conflicting earlier feedback."
        }))
    }
}
