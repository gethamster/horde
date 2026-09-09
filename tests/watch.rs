//! `horde watch` is the push surface any script or agent toolkit follows, so its
//! stream must be complete NDJSON, end in the summary, and exit by outcome.
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};
const BIN: &str = env!("CARGO_BIN_EXE_horde");
const SLOW_TEMPLATE: &str = r#"
name = "slow"
version = "1"
[[steps]]
id = "slow"
kind = "command"
command = ["sleep", "30"]
"#;
const FAILING_TEMPLATE: &str = r#"
name = "failing"
version = "1"
[[steps]]
id = "prepare"
kind = "simulated"
[[steps]]
id = "verify"
kind = "command"
needs = ["prepare"]
command = ["sh", "-c", "echo broken >&2; exit 7"]
"#;

struct Daemon {
    child: Child,
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
}
impl Daemon {
    fn new() -> Self {
        // macOS Unix socket paths are limited to 104 bytes.
        let dir = tempfile::Builder::new()
            .prefix("watch-test-")
            .tempdir_in("/tmp")
            .unwrap();
        let root = dir.path().join("data");
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
            vec!["commit", "--allow-empty", "-m", "initial"],
        ] {
            horde::git::run(&repo, &args).unwrap();
        }
        let child = Command::new(BIN)
            .arg("--data-dir")
            .arg(&root)
            .arg("daemon")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let d = Self {
            child,
            _dir: dir,
            root,
            repo,
        };
        d.wait_ready();
        d
    }
    fn wait_ready(&self) {
        let start = Instant::now();
        while self.call_result("list_tasks", json!({})).is_err() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "daemon startup timeout"
            );
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    fn call_result(&self, method: &str, args: Value) -> Result<Value, String> {
        let mut s = std::os::unix::net::UnixStream::connect(self.root.join("daemon.sock"))
            .map_err(|e| e.to_string())?;
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        writeln!(s, "{}", json!({"method":method,"args":args})).unwrap();
        let mut line = String::new();
        std::io::BufReader::new(s)
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        if v.get("error").is_some() {
            Err(v["error"].to_string())
        } else {
            Ok(v["result"].clone())
        }
    }
    fn call(&self, method: &str, args: Value) -> Value {
        self.call_result(method, args).unwrap()
    }
    fn template(&self, name: &str, text: &str) {
        let p = self.repo.join(".horde/templates");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join(format!("{name}.toml")), text).unwrap();
    }
    fn submit(&self, template: &str) -> String {
        self.call(
            "submit_task",
            json!({"objective":"watch test","repo":self.repo,"template":template}),
        )["id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn cli(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.arg("--data-dir").arg(&self.root).args(args);
        cmd
    }
    fn wait_for_attempt(&self, oid: &str) {
        let start = Instant::now();
        while self.call("inspect", json!({"task":oid}))["attempts"]
            .as_array()
            .unwrap()
            .is_empty()
        {
            assert!(start.elapsed() < Duration::from_secs(10), "no attempt");
            std::thread::sleep(Duration::from_millis(30));
        }
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Parsed NDJSON stream plus the exit status.
struct Stream {
    lines: Vec<Value>,
    code: i32,
    stderr: String,
}
impl Stream {
    fn from(output: Output) -> Self {
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines = stdout
            .lines()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
            .collect();
        Self {
            lines,
            code: output.status.code().unwrap(),
            stderr: String::from_utf8_lossy(&output.stderr).into(),
        }
    }
    fn kinds(&self) -> Vec<&str> {
        self.lines
            .iter()
            .map(|line| line["kind"].as_str().unwrap())
            .collect()
    }
    fn count(&self, kind: &str) -> usize {
        self.kinds().iter().filter(|k| **k == kind).count()
    }
    fn summary(&self) -> &Value {
        let last = self.lines.last().expect("stream is empty");
        assert_eq!(last["kind"], "task.summary", "{last}");
        &last["data"]
    }
}

fn watch(root: &Path, args: &[&str]) -> Stream {
    let output = Command::new(BIN)
        .arg("--data-dir")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    Stream::from(output)
}

fn assert_event_lines_are_well_formed(stream: &Stream) {
    let events: Vec<&Value> = stream
        .lines
        .iter()
        .filter(|line| line.get("seq").is_some())
        .collect();
    assert!(!events.is_empty());
    let seqs: Vec<i64> = events.iter().map(|e| e["seq"].as_i64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
    for event in events {
        assert!(event["created"].is_i64(), "{event}");
        assert!(event["data"].is_object(), "{event}");
    }
}

#[test]
fn watch_streams_every_event_and_ends_with_summary() {
    let d = Daemon::new();
    let oid = d.submit("simulated");
    let stream = watch(&d.root, &["watch", &oid, "--summary", "text"]);
    assert_eq!(stream.code, 0, "{}", stream.stderr);
    assert_event_lines_are_well_formed(&stream);
    let kinds = stream.kinds();
    assert_eq!(kinds[0], "task.submitted", "{kinds:?}");
    let steps = d.call("inspect", json!({"task":oid}))["steps"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(stream.count("step.finished"), steps, "{kinds:?}");
    assert_eq!(stream.count("task.finished"), 1, "{kinds:?}");
    assert!(
        kinds.iter().position(|k| *k == "task.finished")
            > kinds.iter().rposition(|k| *k == "step.finished"),
        "{kinds:?}"
    );
    let summary = stream.summary();
    assert_eq!(summary["task"], oid);
    assert_eq!(summary["status"], "succeeded");
    assert_eq!(summary["terminal"], true);
    assert_eq!(summary["delivery"]["outcome"], "skipped");
    let reason = summary["delivery"]["reason"].as_str().unwrap();
    assert!(reason.contains("template has no delivery step"), "{reason}");
    assert_eq!(summary["delivery_skipped"], reason);
    assert_eq!(summary, &d.call("summary", json!({"task":oid})));
    // The human block goes to stderr only, leaving stdout pure NDJSON.
    assert!(
        stream.stderr.contains(&format!("task {oid} succeeded")),
        "{}",
        stream.stderr
    );
    assert!(
        stream.stderr.contains("delivery_skipped: "),
        "{}",
        stream.stderr
    );
    let human = watch(&d.root, &["watch", &oid, "--summary", "json"]);
    assert_eq!(human.code, 0);
    assert!(human.stderr.is_empty(), "{}", human.stderr);
}

#[test]
fn cancelling_a_watched_task_exits_with_cancelled_status() {
    let d = Daemon::new();
    d.template("slow", SLOW_TEMPLATE);
    let oid = d.submit("slow");
    let child = d
        .cli(&["watch", &oid])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    d.wait_for_attempt(&oid);
    d.call("cancel", json!({"task":oid}));
    let stream = Stream::from(child.wait_with_output().unwrap());
    assert_eq!(stream.code, 2, "{}", stream.stderr);
    assert_event_lines_are_well_formed(&stream);
    assert!(
        stream.kinds().contains(&"task.cancelled"),
        "{:?}",
        stream.kinds()
    );
    let summary = stream.summary();
    assert_eq!(summary["status"], "cancelled");
    assert_eq!(summary["terminal"], true);
}

#[test]
fn failing_step_exits_one_with_failed_summary() {
    let d = Daemon::new();
    d.template("failing", FAILING_TEMPLATE);
    let oid = d.submit("failing");
    let stream = watch(&d.root, &["watch", &oid, "--timeout-secs", "30"]);
    assert_eq!(stream.code, 1, "{}", stream.stderr);
    assert_event_lines_are_well_formed(&stream);
    assert_eq!(stream.count("step.finished"), 2, "{:?}", stream.kinds());
    let failed = stream
        .lines
        .iter()
        .find(|line| line["kind"] == "step.finished" && line["data"]["state"] == "failed")
        .expect("failed step event");
    assert!(
        failed["data"]["result"]["error"]
            .as_str()
            .unwrap()
            .contains("verification command failed"),
        "{failed}"
    );
    let summary = stream.summary();
    assert_eq!(summary["status"], "failed");
    let verify = summary["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "verify")
        .unwrap();
    assert_eq!(verify["state"], "failed");
    assert_eq!(summary["delivery"]["outcome"], "skipped");
}

#[test]
fn events_follow_matches_watch_and_summary_command_prints_json() {
    let d = Daemon::new();
    let oid = d.submit("simulated");
    let followed = watch(&d.root, &["events", &oid, "--follow", "--summary", "json"]);
    assert_eq!(followed.code, 0, "{}", followed.stderr);
    let watched = watch(&d.root, &["watch", &oid, "--summary", "json"]);
    assert_eq!(watched.code, 0, "{}", watched.stderr);
    assert_eq!(followed.lines, watched.lines);
    assert_eq!(followed.summary()["status"], "succeeded");
    // Without --follow, events is the plain one-shot listing.
    let output = d.cli(&["events", &oid, "--after", "0"]).output().unwrap();
    assert!(output.status.success());
    let listing: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        listing.as_array().unwrap().len(),
        followed.lines.len() - 1,
        "{listing}"
    );
    // --after resumes mid-stream without replaying earlier events.
    let last_seq = followed.lines[followed.lines.len() - 2]["seq"]
        .as_i64()
        .unwrap();
    let resumed = watch(
        &d.root,
        &[
            "watch",
            &oid,
            "--after",
            &last_seq.to_string(),
            "--summary",
            "json",
        ],
    );
    assert_eq!(resumed.code, 0);
    assert_eq!(resumed.kinds(), vec!["task.summary"]);
    let output = d.cli(&["summary", &oid]).output().unwrap();
    assert!(output.status.success());
    let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(summary, *followed.summary());
}

#[test]
fn watch_reports_timeout_and_missing_daemon_with_distinct_codes() {
    let mut d = Daemon::new();
    d.template("slow", SLOW_TEMPLATE);
    let oid = d.submit("slow");
    let timed_out = watch(
        &d.root,
        &["watch", &oid, "--timeout-secs", "1", "--interval-ms", "50"],
    );
    assert_eq!(timed_out.code, 3, "{}", timed_out.stderr);
    let last = timed_out.lines.last().unwrap();
    assert_eq!(last["kind"], "watch.error", "{last}");
    assert_eq!(last["data"]["error"], "timeout");
    let child = d
        .cli(&["watch", &oid, "--interval-ms", "50"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    d.wait_for_attempt(&oid);
    d.child.kill().unwrap();
    d.child.wait().unwrap();
    let _ = std::fs::remove_file(d.root.join("daemon.sock"));
    let stream = Stream::from(child.wait_with_output().unwrap());
    assert_eq!(stream.code, 4, "{}", stream.stderr);
    let last = stream.lines.last().unwrap();
    assert_eq!(last["kind"], "watch.error", "{last}");
    assert_eq!(last["data"]["error"], "daemon unavailable");
}
