//! Push hooks: the daemon fans durable task events out to an optional webhook
//! and/or local command declared in `[notify]`. The cursor lives in
//! `event_receipts` under the `notify` consumer, so every event is delivered at
//! most once across restarts, and it advances after each event whether or not
//! delivery succeeded, so a dead endpoint can never stall the queue.
use crate::{
    config::{Notify, Settings},
    store::Store,
};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// Consumer name under which the cursor is stored in `event_receipts`.
pub const CONSUMER: &str = "notify";
/// Longest error text kept on a `notify.failed` event.
const ERROR_CHARS: usize = 400;

/// The hook an event kind maps to, if any. Events the dispatcher itself records
/// never map, so a delivery can never trigger another delivery.
pub fn hook_for(kind: &str) -> Option<&'static str> {
    match kind {
        "step.finished" => Some("step.finished"),
        "task.finished" | "task.cancelled" => Some("task.finished"),
        "question.pending" => Some("question.asked"),
        "task.blocked" => Some("task.blocked"),
        _ => None,
    }
}

/// One task with undelivered events.
struct Pending {
    task: String,
    status: String,
    objective: String,
    repo: String,
    settings: Settings,
    cursor: i64,
    latest: i64,
}

/// Deliver every event recorded since the last tick. Called once per maintenance
/// loop iteration; all awaiting happens here, never on the scheduler.
pub async fn tick(db: &Store) -> Result<()> {
    for pending in pending(db)? {
        let notify = &pending.settings.notify;
        if !notify.enabled() || (!notify.children && has_parent(db, &pending.task)?) {
            advance(db, &pending.task, pending.latest)?;
            continue;
        }
        let events = db.rows(
            "SELECT seq,kind,data,created FROM events WHERE task=? AND seq>? AND seq<=? ORDER BY seq",
            &[&pending.task, &pending.cursor, &pending.latest],
        )?;
        for event in events {
            let seq = event["seq"].as_i64().context("event seq")?;
            let kind = event["kind"].as_str().unwrap_or("");
            if let Some(hook) = hook_for(kind).filter(|hook| notify.wants(hook))
                && !kind.starts_with("notify.")
            {
                let payload = payload(db, &pending, hook, &event);
                deliver(db, &pending, notify, hook, seq, &payload).await;
            }
            advance(db, &pending.task, seq)?;
        }
    }
    Ok(())
}

fn pending(db: &Store) -> Result<Vec<Pending>> {
    db.rows(
        "SELECT * FROM (SELECT id,status,objective,repo,settings,\
         (SELECT MAX(seq) FROM events WHERE task=tasks.id) AS latest,\
         COALESCE((SELECT seq FROM event_receipts WHERE task=tasks.id AND consumer=?),0) AS cursor \
         FROM tasks) WHERE latest > cursor ORDER BY latest",
        &[&CONSUMER],
    )?
    .into_iter()
    .map(|row| {
        Ok(Pending {
            task: row["id"].as_str().context("task id")?.to_owned(),
            status: row["status"].as_str().unwrap_or("").to_owned(),
            objective: row["objective"].as_str().unwrap_or("").to_owned(),
            repo: row["repo"].as_str().unwrap_or("").to_owned(),
            settings: serde_json::from_str(row["settings"].as_str().context("task settings")?)?,
            cursor: row["cursor"].as_i64().unwrap_or(0),
            latest: row["latest"].as_i64().unwrap_or(0),
        })
    })
    .collect()
}

fn has_parent(db: &Store, task: &str) -> Result<bool> {
    Ok(db
        .rows("SELECT parent FROM task_tree WHERE task=?", &[&task])?
        .first()
        .is_some_and(|row| row["parent"].is_string()))
}

fn advance(db: &Store, task: &str, seq: i64) -> Result<()> {
    db.conn.execute(
        "INSERT INTO event_receipts VALUES(?,?,?) ON CONFLICT(task,consumer) DO UPDATE SET seq=MAX(seq,excluded.seq)",
        rusqlite::params![task, CONSUMER, seq],
    )?;
    Ok(())
}

fn payload(db: &Store, pending: &Pending, hook: &str, event: &Value) -> Value {
    let data: Value = event["data"]
        .as_str()
        .and_then(|data| serde_json::from_str(data).ok())
        .unwrap_or_else(|| event["data"].clone());
    let mut payload = json!({
        "hook": hook,
        "task": pending.task,
        "seq": event["seq"],
        "created": event["created"],
        "event": event["kind"],
        "data": data,
        "objective": pending.objective,
        "status": pending.status,
    });
    if hook == "task.finished" {
        // A summary that cannot be built must not hold back the notification.
        match crate::summary::build(db, &pending.task) {
            Ok(summary) => payload["summary"] = summary,
            Err(error) => {
                payload["summary"] = Value::Null;
                payload["summary_error"] = json!(format!("{error:#}"));
            }
        }
    }
    payload
}

/// One attempt per target; the outcome is recorded as an event either way.
async fn deliver(
    db: &Store,
    pending: &Pending,
    notify: &Notify,
    hook: &str,
    seq: i64,
    payload: &Value,
) {
    let timeout = Duration::from_secs(notify.timeout_seconds);
    let mut outcomes = vec![];
    if notify.webhook.is_some() || notify.webhook_env.is_some() {
        outcomes.push(("webhook", webhook(notify, timeout, payload).await));
    }
    if !notify.command.is_empty() {
        outcomes.push(("command", command(pending, notify, timeout, payload).await));
    }
    for (target, outcome) in outcomes {
        let mut data =
            json!({"hook": hook, "seq": seq, "event": payload["event"], "target": target});
        let kind = match outcome {
            Ok(()) => "notify.delivered",
            Err(error) => {
                data["error"] = json!(
                    format!("{error:#}")
                        .chars()
                        .take(ERROR_CHARS)
                        .collect::<String>()
                );
                "notify.failed"
            }
        };
        if let Err(error) = db.event(&pending.task, kind, data) {
            eprintln!("Notify: recording {kind} for {}: {error:#}", pending.task);
        }
    }
}

/// The webhook URL, taken from the daemon's credentials when only a variable
/// name was configured.
fn webhook_url(notify: &Notify) -> Result<String> {
    if let Some(url) = &notify.webhook {
        return Ok(url.clone());
    }
    let name = notify
        .webhook_env
        .as_deref()
        .context("no webhook configured")?;
    crate::config::credential(name).with_context(|| format!("resolving notify.webhook_env {name}"))
}

async fn webhook(notify: &Notify, timeout: Duration, payload: &Value) -> Result<()> {
    let url = webhook_url(notify)?;
    let client = reqwest::Client::builder().timeout(timeout).build()?;
    let response = client
        .post(&url)
        .json(payload)
        .send()
        .await
        .map_err(|error| anyhow!("webhook request failed: {}", without_url(&error)))?;
    let status = response.status();
    if !status.is_success() {
        bail!("webhook returned HTTP {}", status.as_u16());
    }
    Ok(())
}

/// reqwest errors echo the URL, which may carry a secret when it came from
/// `webhook_env`; keep only the underlying cause.
fn without_url(error: &reqwest::Error) -> String {
    let mut source: Option<&dyn std::error::Error> = std::error::Error::source(error);
    let mut cause = None;
    while let Some(current) = source {
        cause = Some(current.to_string());
        source = current.source();
    }
    let summary = if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "connection failed"
    } else {
        "request error"
    };
    match cause {
        Some(cause) => format!("{summary}: {cause}"),
        None => summary.to_owned(),
    }
}

async fn command(
    pending: &Pending,
    notify: &Notify,
    timeout: Duration,
    payload: &Value,
) -> Result<()> {
    let (program, args) = notify
        .command
        .split_first()
        .context("empty notify.command")?;
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .env("HORDE_TASK", &pending.task)
        .env("HORDE_HOOK", payload["hook"].as_str().unwrap_or(""))
        .env("HORDE_EVENT", payload["event"].as_str().unwrap_or(""))
        .current_dir(&pending.repo)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("spawning notify command {program}"))?;
    let mut line = payload.to_string();
    line.push('\n');
    let run = async move {
        if let Some(mut stdin) = child.stdin.take() {
            // A hook that never reads stdin is still a delivery; ignore write errors.
            let _ = stdin.write_all(line.as_bytes()).await;
            drop(stdin);
        }
        child.wait_with_output().await
    };
    // Dropping the timed-out future drops the child, which kills it.
    let output = tokio::time::timeout(timeout, run)
        .await
        .map_err(|_| anyhow!("notify command timed out after {}s", timeout.as_secs()))??;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "notify command exited with {}: {}",
            output.status,
            stderr.trim()
        );
    }
    Ok(())
}
