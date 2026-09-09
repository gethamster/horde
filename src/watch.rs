//! `horde watch`: a client-side NDJSON stream of a task's durable events that
//! ends with the terminal summary. The daemon answers one request per
//! connection, so the stream is assembled by polling `events`, `inspect`, and
//! finally `summary`; every line is a complete JSON object so any script or
//! agent toolkit can follow a task without binding to a particular UI.
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

/// Exit status once the task is terminal, or the watch itself gave up.
pub const EXIT_SUCCEEDED: i32 = 0;
pub const EXIT_FAILED: i32 = 1;
pub const EXIT_CANCELLED: i32 = 2;
pub const EXIT_TIMEOUT: i32 = 3;
pub const EXIT_DAEMON_GONE: i32 = 4;

/// The `events` operation caps a page at this many rows; a full page means
/// more may be waiting and is fetched before the status check.
const EVENT_PAGE: usize = 1000;
const ATTENTION_STATUSES: &[&str] = &["waiting", "blocked"];

#[derive(Clone, Debug, clap::Args)]
pub struct Options {
    /// Start after this event sequence number instead of from the beginning.
    #[arg(long, default_value_t = 0)]
    pub after: i64,
    /// Poll interval while the task is still running.
    #[arg(long, default_value_t = 250)]
    pub interval_ms: u64,
    /// Give up with exit status 3 after this many seconds.
    #[arg(long)]
    pub timeout_secs: Option<u64>,
    /// Human summary on stderr: auto (when stderr is a terminal), text, or json.
    #[arg(long, default_value = "auto", value_parser = ["auto", "text", "json"])]
    pub summary: String,
}

impl Options {
    fn interval(&self) -> Duration {
        Duration::from_millis(self.interval_ms)
    }
    fn timeout(&self) -> Option<Duration> {
        self.timeout_secs.map(Duration::from_secs)
    }
    fn summary_text(&self) -> bool {
        use std::io::IsTerminal;
        match self.summary.as_str() {
            "text" => true,
            "json" => false,
            _ => std::io::stderr().is_terminal(),
        }
    }
}

/// Stream events for `task` until it reaches a terminal status. Returns the
/// process exit status; lines have already been written and flushed to stdout.
pub async fn run(root: &Path, task: &str, options: &Options) -> Result<i32> {
    let mut watch = Watch {
        root,
        task,
        cursor: options.after,
        out: std::io::stdout().lock(),
        attention: None,
    };
    let started = Instant::now();
    loop {
        let status = match watch.poll().await {
            Ok(Some(status)) => status,
            Ok(None) => {
                if let Some(limit) = options.timeout()
                    && started.elapsed() >= limit
                {
                    watch.line(
                        "watch.error",
                        json!({"error":"timeout","timeout_secs":limit.as_secs()}),
                    )?;
                    return Ok(EXIT_TIMEOUT);
                }
                tokio::time::sleep(options.interval()).await;
                continue;
            }
            Err(error) if daemon_gone(root) => {
                watch.line(
                    "watch.error",
                    json!({"error":"daemon unavailable","detail":error.to_string()}),
                )?;
                return Ok(EXIT_DAEMON_GONE);
            }
            Err(error) => return Err(error),
        };
        let summary = watch.finish()?;
        if options.summary_text() {
            eprint!("{}", human(&summary));
        }
        return Ok(match status.as_str() {
            "succeeded" => EXIT_SUCCEEDED,
            "cancelled" => EXIT_CANCELLED,
            _ => EXIT_FAILED,
        });
    }
}

struct Watch<'a> {
    root: &'a Path,
    task: &'a str,
    cursor: i64,
    out: std::io::StdoutLock<'static>,
    /// Last emitted `watch.status` payload, so waiting/blocked prints once per change.
    attention: Option<Value>,
}

impl Watch<'_> {
    fn call(&self, method: &str, args: Value) -> Result<Value> {
        crate::request(self.root, method, args)
    }

    /// One pass: drain new events, then report the task status when terminal.
    async fn poll(&mut self) -> Result<Option<String>> {
        self.drain_events()?;
        let inspect = self.call("inspect", json!({"task":self.task}))?;
        let status = inspect["task"]["status"].as_str().unwrap_or("").to_owned();
        if is_terminal(&status) {
            // Events written alongside the final status update land after the
            // page above; fetch them so the stream is complete before the summary.
            self.drain_events()?;
            return Ok(Some(status));
        }
        if ATTENTION_STATUSES.contains(&status.as_str()) {
            let payload = json!({"status":status,"questions":pending_questions(&inspect)});
            if self.attention.as_ref() != Some(&payload) {
                self.line("watch.status", payload.clone())?;
                self.attention = Some(payload);
            }
        } else {
            self.attention = None;
        }
        Ok(None)
    }

    fn drain_events(&mut self) -> Result<()> {
        loop {
            let page = self.call("events", json!({"task":self.task,"after":self.cursor}))?;
            let rows = page.as_array().context("events response")?;
            for row in rows {
                let seq = row["seq"].as_i64().context("event seq")?;
                // Event payloads are stored as JSON text; anything unparsable
                // is passed through verbatim rather than dropped.
                let data = match row["data"].as_str() {
                    Some(text) => serde_json::from_str(text).unwrap_or_else(|_| json!(text)),
                    None => row["data"].clone(),
                };
                self.write(
                    &json!({"seq":seq,"kind":row["kind"],"created":row["created"],"data":data}),
                )?;
                self.cursor = seq;
            }
            if rows.len() < EVENT_PAGE {
                return Ok(());
            }
        }
    }

    fn finish(&mut self) -> Result<Value> {
        let summary = self.call("summary", json!({"task":self.task}))?;
        self.line("task.summary", summary.clone())?;
        Ok(summary)
    }

    fn line(&mut self, kind: &str, data: Value) -> Result<()> {
        self.write(&json!({"kind":kind,"data":data}))
    }

    fn write(&mut self, value: &Value) -> Result<()> {
        writeln!(self.out, "{value}")?;
        self.out.flush()?;
        Ok(())
    }
}

fn is_terminal(status: &str) -> bool {
    ["succeeded", "failed", "cancelled"].contains(&status)
}

fn daemon_gone(root: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(root.join("daemon.sock")).is_err()
}

fn pending_questions(inspect: &Value) -> Vec<Value> {
    inspect["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|q| q["answer"].is_null())
        .map(|q| json!({"id":q["id"],"question":q["question"]}))
        .collect()
}

/// Short operator-facing report; mirrors the `task.summary` line for humans.
fn human(summary: &Value) -> String {
    let text = |v: &Value| v.as_str().unwrap_or("-").to_owned();
    let mut lines = vec![
        format!(
            "task {} {}",
            text(&summary["task"]),
            text(&summary["status"])
        ),
        "steps:".to_owned(),
    ];
    for step in summary["steps"].as_array().into_iter().flatten() {
        let accepted = match step["accepted"].as_bool() {
            Some(true) => "accepted",
            Some(false) => "rejected",
            None => "-",
        };
        lines.push(format!(
            "  {} {} {accepted}",
            text(&step["name"]),
            text(&step["state"])
        ));
    }
    lines.push(format!(
        "branch {} integrated_head {}",
        text(&summary["branch"]),
        text(&summary["integrated_head"])
    ));
    let delivery = &summary["delivery"];
    lines.push(match delivery["outcome"].as_str() {
        Some("skipped") => format!("delivery_skipped: {}", text(&delivery["reason"])),
        Some(outcome) if !delivery["pr_url"].is_null() => {
            format!("delivery {outcome} pr_url {}", text(&delivery["pr_url"]))
        }
        Some(outcome) => format!("delivery {outcome} {}", text(&delivery["reason"])),
        None => "delivery -".to_owned(),
    });
    let mut report = lines.join("\n");
    report.push('\n');
    report
}
