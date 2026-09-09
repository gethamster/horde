//! Terminal summary of a task: status, step outcomes, integrated head, and the
//! delivery outcome. Delivery is reported explicitly even when nothing ran, so
//! scripts can tell "no PR because delivery is disabled" from "no PR yet".
use crate::{config::Settings, store::Store};
use anyhow::{Context, Result};
use serde_json::{Value, json};

/// Longest step result echoed into the summary before truncation.
const RESULT_PREVIEW_CHARS: usize = 400;
const TERMINAL_STATUSES: &[&str] = &["succeeded", "failed", "cancelled"];

pub fn build(db: &Store, task: &str) -> Result<Value> {
    let row = db.task(task)?;
    let status = row["status"].as_str().unwrap_or("").to_owned();
    let settings: Settings =
        serde_json::from_str(row["settings"].as_str().context("task settings snapshot")?)?;
    let steps = db
        .steps(task)?
        .iter()
        .map(|step| step_summary(db, step))
        .collect::<Result<Vec<_>>>()?;
    let questions_pending: i64 = db.conn.query_row(
        "SELECT COUNT(*) FROM questions WHERE task=? AND answer IS NULL",
        [task],
        |r| r.get(0),
    )?;
    let delivery = delivery_outcome(db, task, &settings, &steps)?;
    let mut summary = json!({
        "task": task,
        "objective": row["objective"],
        "repo": row["repo"],
        "status": status,
        "terminal": TERMINAL_STATUSES.contains(&status.as_str()),
        "steps": steps,
        "branch": format!("horde/{task}"),
        "integrated_head": integrated_head(db, task)?,
        "questions_pending": questions_pending,
        "delivery": delivery,
    });
    if summary["delivery"]["outcome"] == "skipped" {
        summary["delivery_skipped"] = summary["delivery"]["reason"].clone();
    }
    Ok(summary)
}

fn step_summary(db: &Store, step: &Value) -> Result<Value> {
    let spec: Value = serde_json::from_str(step["spec"].as_str().context("step spec")?)?;
    let result: Value = step["result"]
        .as_str()
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or(Value::Null);
    let attempts: i64 = db.conn.query_row(
        "SELECT COUNT(*) FROM attempts WHERE step=?",
        [step["id"].as_str().context("step id")?],
        |r| r.get(0),
    )?;
    Ok(json!({
        "name": step["name"],
        "kind": spec["kind"],
        "state": step["state"],
        "attempts": attempts,
        "accepted": result["accepted"].as_bool(),
        "result": preview(&result["result"]),
        "error": result["error"].as_str(),
    }))
}

fn preview(value: &Value) -> Value {
    let text = match value {
        Value::Null => return Value::Null,
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    json!(text.chars().take(RESULT_PREVIEW_CHARS).collect::<String>())
}

/// The verified integrated commit: the live integrated worktree when present,
/// otherwise the last successful integration record.
fn integrated_head(db: &Store, task: &str) -> Result<Value> {
    let workspace = db.root.join("workspaces").join(task).join("integrated");
    if workspace.exists()
        && let Ok(head) = crate::git::run(&workspace, &["rev-parse", "HEAD"])
    {
        return Ok(json!(head));
    }
    let last = db.rows(
        "SELECT commit_id FROM integrations WHERE task=? AND state='succeeded' ORDER BY created DESC, rowid DESC LIMIT 1",
        &[&task],
    )?;
    Ok(last
        .first()
        .map(|row| row["commit_id"].clone())
        .unwrap_or(Value::Null))
}

fn outcome(outcome: &str, pr_url: Option<&str>, reason: Option<String>) -> Value {
    json!({"outcome": outcome, "pr_url": pr_url, "reason": reason})
}

fn delivery_outcome(db: &Store, task: &str, settings: &Settings, steps: &[Value]) -> Result<Value> {
    let enabled = settings.delivery.enabled;
    let Some(step) = steps.iter().rev().find(|s| s["kind"] == "delivery") else {
        let mut reason = "template has no delivery step".to_owned();
        if !enabled {
            reason.push_str("; delivery disabled in settings");
        }
        return Ok(outcome("skipped", None, Some(reason)));
    };
    if !enabled {
        return Ok(outcome(
            "skipped",
            None,
            Some("delivery disabled in settings ([delivery] enabled = false)".into()),
        ));
    }
    match step["state"].as_str().unwrap_or("") {
        "skipped" => {
            let failed: Vec<&str> = steps
                .iter()
                .filter(|s| s["state"] == "failed")
                .filter_map(|s| s["name"].as_str())
                .collect();
            let reason = if failed.is_empty() {
                "upstream step failed".to_owned()
            } else {
                format!("upstream step failed: {}", failed.join(", "))
            };
            Ok(outcome("skipped", None, Some(reason)))
        }
        "failed" => Ok(outcome(
            "failed",
            None,
            Some(
                step["error"]
                    .as_str()
                    .unwrap_or("delivery step failed")
                    .into(),
            ),
        )),
        "cancelled" => Ok(outcome(
            "failed",
            None,
            Some("delivery step cancelled".into()),
        )),
        "succeeded" => delivered(db, task),
        _ => Ok(outcome("pending", None, None)),
    }
}

fn delivered(db: &Store, task: &str) -> Result<Value> {
    let ops = db.rows(
        "SELECT name,state,data FROM external_ops WHERE task=? AND name IN ('pr','merge')",
        &[&task],
    )?;
    let succeeded = |name: &str| {
        ops.iter()
            .find(|op| op["name"] == name && op["state"] == "succeeded")
    };
    let pr_url = succeeded("pr")
        .and_then(|op| op["data"].as_str())
        .and_then(|data| serde_json::from_str::<Value>(data).ok())
        .and_then(|data| data["url"].as_str().map(str::to_owned));
    let merged = succeeded("merge").is_some();
    Ok(outcome(
        if merged { "merged" } else { "pr_ready" },
        pr_url.as_deref(),
        None,
    ))
}
