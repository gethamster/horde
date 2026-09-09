//! `[notify]` hooks: the daemon delivers step.finished and task.finished payloads
//! to a local command and a webhook exactly once, records failures without
//! blocking the task, and never repeats a delivery after a restart.
use horde::config::Settings;
use serde_json::{Value, json};
use std::{
    io::{BufRead, Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
const BIN: &str = env!("CARGO_BIN_EXE_horde");
/// How long a hook delivery may lag the event it reports.
const DELIVERY_WAIT: Duration = Duration::from_secs(10);
/// Long enough for several maintenance ticks to run and repeat nothing.
const QUIET: Duration = Duration::from_millis(2500);
struct Daemon {
    child: Child,
    #[allow(dead_code)]
    dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
}
impl Daemon {
    fn new() -> Self {
        // macOS Unix socket paths are limited to 104 bytes.
        let dir = tempfile::Builder::new()
            .prefix("notify-test-")
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
        let child = Self::spawn(&root);
        let d = Self {
            child,
            dir,
            root,
            repo,
        };
        d.wait_ready();
        d
    }
    fn spawn(root: &Path) -> Child {
        Command::new(BIN)
            .arg("--data-dir")
            .arg(root)
            .arg("daemon")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }
    fn wait_ready(&self) {
        let start = Instant::now();
        loop {
            if self.root.join("daemon.sock").exists()
                && self.call_result("list_tasks", json!({})).is_ok()
            {
                break;
            }
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
    fn configure(&self, notify: &str) {
        std::fs::write(
            self.repo.join(".horde.toml"),
            format!("[notify]\n{notify}\n"),
        )
        .unwrap();
    }
    fn submit(&self, template: &str) -> String {
        self.call(
            "submit_task",
            json!({"objective":"notify test","repo":self.repo,"template":template}),
        )["id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn inspect(&self, oid: &str) -> Value {
        self.call("inspect", json!({"task":oid}))
    }
    fn wait(&self, oid: &str, status: &str) -> Value {
        let start = Instant::now();
        loop {
            let v = self.inspect(oid);
            if v["task"]["status"] == status {
                return v;
            }
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "expected {status}: {v}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn events(&self, oid: &str, kind: &str) -> Vec<Value> {
        self.call("events", json!({"task":oid}))
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == kind)
            .cloned()
            .collect()
    }
    fn restart(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
        self.child = Self::spawn(&self.root);
        self.wait_ready();
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
/// Poll until `check` returns a value, or fail with the last observation.
fn eventually<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(
            start.elapsed() < DELIVERY_WAIT,
            "timed out waiting for {what}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn by_hook<'a>(payloads: &'a [Value], hook: &str) -> Vec<&'a Value> {
    payloads.iter().filter(|p| p["hook"] == hook).collect()
}
/// Wait until the task's terminal notification has arrived at `read`.
fn wait_for_finish(read: impl Fn() -> Vec<Value>) -> Vec<Value> {
    eventually("task.finished delivery", || {
        let payloads = read();
        (!by_hook(&payloads, "task.finished").is_empty()).then_some(payloads)
    })
}
/// The simulated template runs four steps; every one reports step.finished and
/// the task reports task.finished with a summary that says delivery was skipped.
fn assert_complete_delivery(payloads: &[Value], oid: &str, step_count: usize) {
    let steps = by_hook(payloads, "step.finished");
    assert_eq!(steps.len(), step_count, "{payloads:?}");
    for payload in &steps {
        assert_eq!(payload["task"], oid);
        assert_eq!(payload["event"], "step.finished");
        assert_eq!(payload["data"]["state"], "succeeded");
        assert_eq!(payload["objective"], "notify test");
        assert!(payload["seq"].is_i64());
        assert!(payload["created"].is_i64());
    }
    let finished = by_hook(payloads, "task.finished");
    assert_eq!(finished.len(), 1, "{payloads:?}");
    let finished = finished[0];
    assert_eq!(finished["task"], oid);
    assert_eq!(finished["event"], "task.finished");
    assert_eq!(finished["status"], "succeeded");
    assert_eq!(finished["data"]["status"], "succeeded");
    assert_eq!(finished["summary"]["task"], oid);
    assert_eq!(finished["summary"]["status"], "succeeded");
    assert_eq!(finished["summary"]["delivery"]["outcome"], "skipped");
    assert!(finished["summary"]["delivery_skipped"].is_string());
    // Deliveries arrive in event order, so the terminal report comes last.
    assert_eq!(payloads.last().unwrap()["hook"], "task.finished");
    assert!(
        payloads
            .iter()
            .all(|p| !p["event"].as_str().unwrap().starts_with("notify.")),
        "{payloads:?}"
    );
}
/// A minimal HTTP/1.1 server capturing every POST body it receives.
struct Webhook {
    url: String,
    bodies: Arc<Mutex<Vec<Value>>>,
}
impl Webhook {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        let bodies = Arc::new(Mutex::new(vec![]));
        let seen = bodies.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                seen.lock()
                    .unwrap()
                    .push(serde_json::from_slice(&body).unwrap());
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        Self { url, bodies }
    }
    fn received(&self) -> Vec<Value> {
        self.bodies.lock().unwrap().clone()
    }
}
#[test]
fn command_hook_receives_every_step_and_the_terminal_summary_on_stdin() {
    let d = Daemon::new();
    let log = d.repo.join("notify.log");
    d.configure(&format!(
        "command = [\"/bin/sh\", \"-c\", \"printf '%s %s ' \\\"$HORDE_HOOK\\\" \\\"$HORDE_EVENT\\\" >> {0}.env; cat >> {0}\"]",
        log.display()
    ));
    let oid = d.submit("simulated");
    d.wait(&oid, "succeeded");
    let payloads = wait_for_finish(|| lines(&log));
    assert_complete_delivery(&payloads, &oid, 4);
    // The command sees the hook and event kind in its environment as well.
    let env = std::fs::read_to_string(format!("{}.env", log.display())).unwrap();
    assert_eq!(env.matches("step.finished step.finished").count(), 4);
    assert_eq!(env.matches("task.finished task.finished").count(), 1);
    // Every delivery is recorded, and nothing failed.
    let delivered = d.events(&oid, "notify.delivered");
    assert_eq!(delivered.len(), 5, "{delivered:?}");
    assert!(
        delivered
            .iter()
            .all(|e| e["data"].as_str().unwrap().contains("\"command\""))
    );
    assert!(d.events(&oid, "notify.failed").is_empty());
}
#[test]
fn webhook_receives_json_payloads_for_every_hook() {
    let d = Daemon::new();
    let hook = Webhook::start();
    d.configure(&format!("webhook = \"{}\"", hook.url));
    let oid = d.submit("simulated");
    d.wait(&oid, "succeeded");
    let payloads = wait_for_finish(|| hook.received());
    assert_complete_delivery(&payloads, &oid, 4);
    assert_eq!(d.events(&oid, "notify.delivered").len(), 5);
    assert!(d.events(&oid, "notify.failed").is_empty());
}
#[test]
fn the_cursor_is_durable_so_a_restart_repeats_nothing() {
    let mut d = Daemon::new();
    let log = d.repo.join("notify.log");
    d.configure(&format!(
        "command = [\"/bin/sh\", \"-c\", \"cat >> {}\"]",
        log.display()
    ));
    let oid = d.submit("simulated");
    d.wait(&oid, "succeeded");
    let before = wait_for_finish(|| lines(&log));
    assert_complete_delivery(&before, &oid, 4);
    // Let the dispatcher observe its own receipts before the restart.
    std::thread::sleep(QUIET);
    d.restart();
    std::thread::sleep(QUIET);
    assert_eq!(lines(&log), before, "a restart repeated a delivery");
    assert_eq!(d.events(&oid, "notify.delivered").len(), 5);
    let cursor: i64 = d
        .call("events", json!({"task":oid,"consumer":"notify"}))
        .as_array()
        .unwrap()
        .len() as i64;
    assert_eq!(
        cursor, 0,
        "every event was acknowledged by the notify consumer"
    );
}
#[test]
fn an_unreachable_webhook_is_recorded_and_the_task_still_finishes() {
    let d = Daemon::new();
    // Bind and release a port so the address refuses connections promptly.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    d.configure(&format!(
        "webhook = \"http://127.0.0.1:{port}/hook\"\nevents = [\"task.finished\"]\ntimeout_seconds = 2"
    ));
    let oid = d.submit("simulated");
    d.wait(&oid, "succeeded");
    let failed = eventually("notify.failed", || {
        let failed = d.events(&oid, "notify.failed");
        (!failed.is_empty()).then_some(failed)
    });
    assert_eq!(failed.len(), 1, "{failed:?}");
    let data: Value = serde_json::from_str(failed[0]["data"].as_str().unwrap()).unwrap();
    assert_eq!(data["hook"], "task.finished");
    assert_eq!(data["target"], "webhook");
    assert!(
        data["error"].as_str().unwrap().contains("webhook"),
        "{data}"
    );
    assert!(d.events(&oid, "notify.delivered").is_empty());
    std::thread::sleep(QUIET);
    assert_eq!(
        d.events(&oid, "notify.failed").len(),
        1,
        "a failure was retried"
    );
    assert_eq!(d.inspect(&oid)["task"]["status"], "succeeded");
}
#[test]
fn notify_settings_load_validate_and_default_sanely() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join(".horde.toml");
    let defaults = Settings::default().notify;
    assert!(!defaults.enabled());
    assert_eq!(
        defaults.events,
        ["step.finished", "task.finished", "question.asked"]
    );
    assert_eq!(defaults.timeout_seconds, 15);
    assert!(!defaults.children);
    std::fs::write(
        &config,
        "[notify]\nwebhook_env = \"HORDE_WEBHOOK_URL\"\nevents = [\"task.finished\", \"task.blocked\"]\nchildren = true\n",
    )
    .unwrap();
    let loaded = Settings::load(dir.path()).unwrap().notify;
    assert!(loaded.enabled());
    assert_eq!(loaded.webhook_env.as_deref(), Some("HORDE_WEBHOOK_URL"));
    assert!(loaded.wants("task.blocked"));
    assert!(!loaded.wants("step.finished"));
    assert!(loaded.children);
    // The starter file documents [notify] without changing any default.
    std::fs::write(&config, horde::config::STARTER).unwrap();
    assert!(!Settings::load(dir.path()).unwrap().notify.enabled());
    for (body, expected) in [
        ("[notify]\nslack = \"x\"\n", "unknown field"),
        ("[notify]\nevents = [\"step.started\"]\n", "unknown hook"),
        ("[notify]\ntimeout_seconds = 0\n", "timeout_seconds"),
        ("[notify]\ncommand = [\"\"]\n", "command"),
        ("[notify]\nwebhook = \"\"\n", "webhook"),
    ] {
        std::fs::write(&config, body).unwrap();
        let error = Settings::load(dir.path()).unwrap_err().to_string();
        assert!(error.contains(expected), "{body}: {error}");
    }
}
