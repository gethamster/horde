use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
const BIN: &str = env!("CARGO_BIN_EXE_horde");
struct Daemon {
    child: Child,
    dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
}
impl Daemon {
    fn new() -> Self {
        // macOS Unix socket paths are limited to 104 bytes.
        let dir = tempfile::Builder::new()
            .prefix("task-test-")
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
    fn submit(&self, template: &str) -> String {
        self.call(
            "submit_task",
            json!({"objective":"process test","repo":self.repo,"template":template}),
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
    fn template(&self, name: &str, text: &str) {
        let p = self.repo.join(".horde/templates");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join(format!("{name}.toml")), text).unwrap();
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
#[test]
fn execution_outlives_cli_and_mcp_clients_and_survives_restart() {
    let mut d = Daemon::new();
    let output = Command::new(BIN)
        .arg("--data-dir")
        .arg(&d.root)
        .args([
            "submit",
            "finish disconnected",
            "--template",
            "simulated",
            "--repo",
        ])
        .arg(&d.repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oid = serde_json::from_slice::<Value>(&output.stdout).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let v = d.wait(&oid, "succeeded");
    assert_eq!(v["attempts"].as_array().unwrap().len(), 4);
    let mut mcp = Command::new(BIN)
        .arg("--data-dir")
        .arg(&d.root)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(mcp.stdin.take().unwrap(),"{}",json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"inspect","arguments":{"task":oid}}})).unwrap();
    let response: Value = serde_json::from_slice(&mcp.wait_with_output().unwrap().stdout).unwrap();
    let mcp_inspect: Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(mcp_inspect, v);
    d.restart();
    assert_eq!(d.inspect(&oid)["attempts"].as_array().unwrap().len(), 4);
}
#[test]
fn messaging_across_independent_cli_processes_recovers_after_restart() {
    let mut d = Daemon::new();
    let oid = d.submit("simulated");
    d.wait(&oid, "succeeded");
    let a = d.call("register_worker", json!({"task":oid}));
    let b = d.call("register_worker", json!({"task":oid}));
    let mid = horde::store::id();
    let args = json!({"task":oid,"worker":a["id"],"id":mid,"destination":b["id"],"body":"new interface","actionable":true});
    let status = Command::new(BIN)
        .arg("--data-dir")
        .arg(&d.root)
        .args(["call", "send_message", &args.to_string()])
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    d.restart();
    assert_eq!(
        d.call("read_messages", json!({"task":oid,"worker":b["id"]}))[0]["id"],
        mid
    );
    assert_eq!(d.call("send_message", args)["duplicate"], true);
    d.call(
        "acknowledge_messages",
        json!({"task":oid,"worker":b["id"],"ids":[mid]}),
    );
    d.restart();
    assert_eq!(
        d.call("read_messages", json!({"task":oid,"worker":b["id"]})),
        json!([])
    );
}
#[test]
fn conditional_repair_and_verification_loop_run_without_human_input() {
    let d = Daemon::new();
    d.template(
        "repair",
        r#"
name="repair"
version="1"
[[steps]]
id="check"
kind="command"
command=["sh","-c","test -f repaired"]
attempts=2
[[steps]]
id="repair"
kind="command"
needs=["check"]
command=["sh","-c","touch repaired"]
[steps.when]
step="check"
status="failed"
[[steps]]
id="verify"
kind="command"
needs=["repair"]
command=["test","-f","repaired"]
"#,
    );
    let oid = d.submit("repair");
    let v = d.wait(&oid, "succeeded");
    assert_eq!(v["attempts"].as_array().unwrap().len(), 4);
}
#[test]
fn crashed_command_is_not_replayed_until_reconciled() {
    let mut d = Daemon::new();
    d.template(
        "slow",
        r#"
name="slow"
version="1"
[[steps]]
id="slow"
kind="command"
command=["sleep","30"]
"#,
    );
    let oid = d.submit("slow");
    let start = Instant::now();
    let (pid, wid) = loop {
        let v = d.inspect(&oid);
        if let Some(a) = v["attempts"].as_array().unwrap().first()
            && let Some(pid) = a["pid"].as_i64()
        {
            break (pid, a["worker"].clone());
        }
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(30));
    };
    d.restart();
    let v = d.wait(&oid, "blocked");
    assert_eq!(v["attempts"].as_array().unwrap().len(), 1);
    assert!(d.call_result("resume", json!({"task":oid})).is_err());
    assert!(
        d.call_result("reconcile_worker", json!({"task":oid,"worker":wid}))
            .is_err()
    );
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    let start = Instant::now();
    loop {
        if d.call_result("reconcile_worker", json!({"task":oid,"worker":wid}))
            .is_ok()
        {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(30));
    }
    d.call("cancel", json!({"task":oid}));
}
#[test]
fn cancelling_stops_the_active_process_group() {
    let d = Daemon::new();
    d.template(
        "slow",
        r#"
name="slow"
version="1"
[[steps]]
id="slow"
kind="command"
command=["sleep","30"]
"#,
    );
    let oid = d.submit("slow");
    let start = Instant::now();
    let pid = loop {
        let v = d.inspect(&oid);
        if let Some(pid) = v["attempts"]
            .as_array()
            .unwrap()
            .first()
            .and_then(|a| a["pid"].as_i64())
        {
            break pid;
        }
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(30));
    };
    d.call("cancel", json!({"task":oid}));
    let start = Instant::now();
    while horde::executor::process_alive(pid as i32) {
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(30));
    }
    assert_eq!(d.inspect(&oid)["task"]["status"], "cancelled");
}
#[test]
fn idle_managed_worker_resumes_on_actionable_message_only() {
    let d = Daemon::new();
    let oid = d.submit("simulated");
    let v = d.wait(&oid, "succeeded");
    let target = v["workers"][0]["id"].clone();
    let sender = d.call("register_worker", json!({"task":oid}));
    d.call("send_message",json!({"task":oid,"worker":sender["id"],"id":horde::store::id(),"destination":target,"body":"presence","actionable":false}));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(d.inspect(&oid)["attempts"].as_array().unwrap().len(), 4);
    d.call("send_message",json!({"task":oid,"worker":sender["id"],"id":horde::store::id(),"destination":target,"body":"please check interface","actionable":true}));
    let start = Instant::now();
    loop {
        let v = d.inspect(&oid);
        if v["attempts"].as_array().unwrap().len() == 5 && v["task"]["status"] == "succeeded" {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "{v}");
        std::thread::sleep(Duration::from_millis(30));
    }
}
#[test]
fn mock_codex_adapter_runs_in_registered_worktree_and_integrates() {
    let d = Daemon::new();
    let script = d.dir.path().join("codex-mock");
    std::fs::write(&script,r#"#!/bin/sh
cat >/dev/null
printf 'hello\n' > hello.txt
git add hello.txt
git commit -qm test
printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"{\"result\":\"implemented\",\"accepted\":true}"}}'
printf '%s\n' '{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}}'
"#).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        d.repo.join(".horde.toml"),
        format!(
            "[executors.worker]\nprovider=\"codex\"\nprogram={:?}\n",
            script.to_str().unwrap()
        ),
    )
    .unwrap();
    d.template(
        "mock",
        r#"
name="mock"
version="1"
[[steps]]
id="code"
scope=["hello.txt"]
instructions="Implement hello"
"#,
    );
    let oid = d.submit("mock");
    let v = d.wait(&oid, "succeeded");
    let path = d
        .root
        .join("workspaces")
        .join(&oid)
        .join("integrated/hello.txt");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "hello\n");
    assert!(!d.repo.join("hello.txt").exists());
    assert_eq!(v["integrations"][0]["state"], "succeeded");
}

#[test]
fn delivery_reconciles_pr_creation_before_retrying_and_merges_verified_head() {
    let mut d = Daemon::new();
    let remote = d.dir.path().join("remote.git");
    std::fs::create_dir(&remote).unwrap();
    horde::git::run(&remote, &["init", "--bare"]).unwrap();
    horde::git::run(
        &d.repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    )
    .unwrap();
    let script = d.dir.path().join("gh-mock");
    let ledger = d.dir.path().join("github-state");
    let code=r#"#!/bin/sh
set -eu
ledger=LEDGER
printf '%s\n' "$1 $2" >> "$ledger.calls"
head=$(git rev-parse HEAD)
case "$1 $2" in
  'repo view') printf '{"nameWithOwner":"test/repo"}';;
  'pr list') if test -f "$ledger.pr"; then cat "$ledger.pr"; else printf '[]'; fi;;
  'pr create') printf '[{"number":1,"url":"https://example.invalid/pr/1","state":"OPEN","headRefOid":"%s"}]' "$head" > "$ledger.pr"; echo 'lost response after creating PR' >&2; exit 1;;
  'pr checks') echo 'checks passed';;
  'pr merge') printf '[{"number":1,"url":"https://example.invalid/pr/1","state":"MERGED","headRefOid":"%s"}]' "$head" > "$ledger.pr";;
  'pr view') printf '{"state":"MERGED","mergeCommit":{"oid":"%s"},"url":"https://example.invalid/pr/1"}' "$head";;
  'run list') printf '[{"databaseId":7,"status":"completed","conclusion":"success"}]';;
  'run watch') echo 'deployment passed';;
  *) echo 'unexpected args' >&2; exit 2;;
esac
"#.replace("LEDGER",&format!("'{}'",ledger.display()));
    std::fs::write(&script, code).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(d.repo.join(".horde.toml"),format!("[delivery]\nenabled=true\nrepository=\"test/repo\"\nbase=\"main\"\nmerge=true\ndeploy_workflow=\"deploy.yml\"\nprogram={:?}\n",script.to_str().unwrap())).unwrap();
    d.template(
        "delivery",
        r#"
name="delivery"
version="1"
[[steps]]
id="deliver"
kind="delivery"
instructions="Test delivery"
"#,
    );
    let oid = d.submit("delivery");
    d.wait(&oid, "failed");
    d.restart();
    d.call("resume", json!({"task":oid}));
    let v = d.wait(&oid, "succeeded");
    let calls = std::fs::read_to_string(ledger.with_extension("calls")).unwrap();
    assert_eq!(calls.lines().filter(|x| *x == "pr create").count(), 1);
    assert_eq!(calls.lines().filter(|x| *x == "pr merge").count(), 1);
    assert_eq!(v["attempts"].as_array().unwrap().len(), 2);
    assert!(
        v["external_ops"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["name"] == "deployment" && o["state"] == "succeeded")
    );
}

#[test]
fn explicit_fallback_is_recorded_and_used_only_after_failure() {
    let d = Daemon::new();
    std::fs::write(d.repo.join(".horde.toml"),"[fallbacks]\nworker=\"simulated\"\n[executors.worker]\nprovider=\"codex\"\nprogram=\"/definitely/missing/executor\"\n").unwrap();
    d.template(
        "fallback",
        r#"
name="fallback"
version="1"
[[steps]]
id="step"
attempts=2
"#,
    );
    let oid = d.submit("fallback");
    let v = d.wait(&oid, "succeeded");
    assert_eq!(v["attempts"].as_array().unwrap().len(), 2);
    assert!(
        d.call("events", json!({"task":oid}))
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "executor.escalated")
    );
}

#[test]
fn maintenance_errors_do_not_terminate_local_daemon() {
    let d = Daemon::new();
    let finished = d.submit("simulated");
    d.wait(&finished, "succeeded");
    let db = horde::store::Store::open(&d.root).unwrap();
    db.conn
        .execute(
            "INSERT INTO remote_origins VALUES(?,'offline-peer','remote-owner')",
            [&finished],
        )
        .unwrap();
    drop(db);
    std::fs::write(d.root.join("network-runtime.toml"), "invalid = [").unwrap();
    std::thread::sleep(Duration::from_millis(1400));
    assert!(d.call_result("list_tasks", json!({})).is_ok());
    let oid = d.submit("simulated");
    d.wait(&oid, "succeeded");
}

#[test]
fn slow_peer_discovery_does_not_block_cli_or_shutdown() {
    use std::os::unix::fs::PermissionsExt;
    let mut d = Daemon::new();
    let oid = d.submit("simulated");
    d.wait(&oid, "succeeded");
    let marker = d.dir.path().join("discovery-started");
    let script = d.dir.path().join("slow-tailscale");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n: > '{}'\nexec /bin/sleep 10\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = horde::network::NetworkConfig {
        provider: horde::network::Provider::Tailscale,
        tailscale_program: script,
        ..Default::default()
    };
    std::fs::write(
        d.root.join("network-runtime.toml"),
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
    let db = horde::store::Store::open(&d.root).unwrap();
    db.atomic(|| {
        db.conn
            .execute("UPDATE tasks SET status='waiting' WHERE id=?", [&oid])?;
        db.conn.execute(
            "INSERT INTO remote_origins VALUES(?,'slow-peer','remote-owner')",
            [&oid],
        )?;
        Ok(())
    })
    .unwrap();
    drop(db);
    let start = Instant::now();
    while !marker.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "maintenance never started"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let start = Instant::now();
    assert!(d.call_result("list_tasks", json!({})).is_ok());
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "CLI waited for peer"
    );
    std::fs::write(d.root.join("shutdown.request"), "").unwrap();
    while d.child.try_wait().unwrap().is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "shutdown waited for peer"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn daemon_enforces_step_budget_and_retries_with_a_fresh_budget() {
    let d = Daemon::new();
    std::fs::write(
        d.repo.join(".horde.toml"),
        "step_budget_seconds = 8\n[executors.worker]\nstep_budget_seconds = 3\n",
    )
    .unwrap();
    d.template(
        "budgeted",
        r#"name = "budgeted"
version = "1"
[[steps]]
id = "stall"
kind = "command"
attempts = 2
step_budget_seconds = 1
command = ["sh", "-c", "sleep 30"]
"#,
    );
    let task = d.submit("budgeted");
    let started = Instant::now();
    loop {
        let inspect = d.inspect(&task);
        if inspect["attempts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["timing"]["budget_s"] == 1)
        {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(20));
    }
    let result = d.wait(&task, "failed");
    assert_eq!(result["attempts"].as_array().unwrap().len(), 2);
    for attempt in result["attempts"].as_array().unwrap() {
        assert_eq!(attempt["state"], "failed");
        let value: Value = serde_json::from_str(attempt["result"].as_str().unwrap()).unwrap();
        assert_eq!(value["error"], "step budget exhausted");
        assert_eq!(value["budget_s"], 1);
        assert_eq!(attempt["timing"]["remaining_s"], 0.0);
    }
    let events = d.call("events", json!({"task":task}));
    assert_eq!(
        events
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["kind"] == "step.budget_exhausted")
            .count(),
        2
    );
    let metrics = d.call("metrics", json!({"task":task}));
    assert_eq!(metrics["steps"][0]["attempts"].as_array().unwrap().len(), 2);
    assert!(metrics["steps"][0]["elapsed_seconds"].as_i64().unwrap() >= 2);
}

#[test]
fn daemon_ends_a_streaming_planner_loop_despite_valid_different_calls() {
    use std::io::Read;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let d = Daemon::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let server = std::thread::spawn(move || {
        let mut turn = 0;
        while !stopped.load(Ordering::Relaxed) {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut header = vec![];
            let mut byte = [0; 1];
            while !header.ends_with(b"\r\n\r\n") {
                if stream.read_exact(&mut byte).is_err() {
                    return;
                }
                header.push(byte[0]);
            }
            let header = String::from_utf8(header).unwrap();
            let length = header
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|s| s.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let mut request = vec![0; length];
            if stream.read_exact(&mut request).is_err() {
                continue;
            }
            let body = if header.starts_with("GET ") {
                json!({"data":[{"id":"test-model"}]}).to_string()
            } else {
                turn += 1;
                std::thread::sleep(Duration::from_millis(120));
                let call = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":format!("read-{turn}"),"type":"function","function":{"name":"read_context","arguments":json!({"after":turn}).to_string()}}]}}]});
                let usage =
                    json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2}});
                format!("data: {call}\n\ndata: {usage}\n\ndata: [DONE]\n\n")
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                if body.starts_with("data:") {
                    "text/event-stream"
                } else {
                    "application/json"
                },
                body
            );
        }
    });
    std::fs::write(
        d.repo.join(".horde.toml"),
        format!(
            r#"
max_tool_rounds = 10000
[providers.default]
kind = "tuara"
auth_mode = "api"
base_url = "http://{address}/v1"
api_key_env = "PATH"
model = "test-model"
stream = true
[executors.planner]
step_budget_seconds = 1
"#
        ),
    )
    .unwrap();
    d.template(
        "loop",
        "name=\"loop\"\nversion=\"1\"\n[[steps]]\nid=\"plan\"\nrole=\"planner\"\nattempts=1\n",
    );
    let task = d.submit("loop");
    let inspect = d.wait(&task, "failed");
    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
    let result: Value =
        serde_json::from_str(inspect["attempts"][0]["result"].as_str().unwrap()).unwrap();
    assert_eq!(result["error"], "step budget exhausted", "{result}");
    let events = d.call("events", json!({"task":task}));
    let calls: Vec<_> = events
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "tool.completed")
        .collect();
    assert!(calls.len() >= 2, "{events}");
    for call in calls {
        let data: Value = serde_json::from_str(call["data"].as_str().unwrap()).unwrap();
        assert_eq!(data["success"], true, "{data}");
    }
    let metrics = d.call("metrics", json!({"task":task}));
    assert!(
        metrics["steps"][0]["reported_input_tokens"]
            .as_u64()
            .unwrap()
            > 0,
        "{metrics}"
    );
}
