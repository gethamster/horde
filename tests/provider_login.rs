use horde::{provider_login, store::Store};
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn call(db: &Store, arguments: Value) -> Value {
    provider_login::dispatch(db, &arguments).unwrap()
}

fn start(db: &Store, provider: &str, request_id: &str, timeout: u64) -> Value {
    call(
        db,
        json!({"action":"start","provider":provider,"request_id":request_id,"timeout_seconds":timeout}),
    )
}

fn status(db: &Store, session: &Value) -> Value {
    call(
        db,
        json!({"action":"status","session_id":session["session_id"]}),
    )
}

fn wait_for(db: &Store, session: &Value, predicate: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let value = status(db, session);
        if predicate(&value) {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "session did not advance before the deadline"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn terminal(db: &Store, session: &Value) -> Value {
    wait_for(db, session, |value| {
        matches!(
            value["status"].as_str(),
            Some("succeeded" | "failed" | "cancelled" | "expired")
        )
    })
}

fn mock(home: &Path, name: &str, body: &str, verified: bool) {
    let file = home.join(name);
    let verification = if verified { "exit 0" } else { "exit 1" };
    std::fs::write(&file, format!(
        "#!/bin/sh\ncase \"$*\" in\n'login status'|'auth status') {verification} ;;\n'login --device-auth'|'auth login') ;;\n*) exit 90 ;;\nesac\nprintf '%s' \"$$\" > \"$HOME/{name}.pid\"\nprintf 'x' >> \"$HOME/{name}.starts\"\n{body}\n"
    )).unwrap();
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn fixture(name: &str, worker: bool) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let config = dir.path().join("config/horde");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&config).unwrap();
    let body = match name {
        "output" => {
            "i=0; while [ \"$i\" -lt 3000 ]; do printf '0123456789abcdef'; i=$((i+1)); done; printf '\\033[31mready\\033[0m'; read answer; exit 0"
        }
        "split" => {
            "printf 'https://example.invalid/'; /bin/sleep 0.1; printf 'authorize'; read answer; printf '%s' \"$answer\"; exit 0"
        }
        "login_failure" => "printf 'https://example.invalid/authorize'; read answer; exit 4",
        _ => {
            "printf '\\033[32mhttps://example.invalid/authorize\\033[0m'; read answer; printf '%s' \"$answer\"; exit 0"
        }
    };
    mock(&home, "codex", body, name != "verify_failure");
    mock(&home, "claude", body, true);
    if name == "cancel_verify" {
        let program = home.join("codex");
        let script = std::fs::read_to_string(&program).unwrap().replace(
            "'login status'|'auth status') exit 0 ;;",
            "'login status'|'auth status') printf '%s' \"$$\" > \"$HOME/status.pid\"; /bin/sleep 30 & printf '%s' \"$!\" > \"$HOME/status-child.pid\"; wait ;;",
        );
        std::fs::write(program, script).unwrap();
    }
    std::fs::write(config.join("config.toml"), format!(
        "# Existing configuration must survive login.\n[providers.first]\nkind='codex'\nauth_mode='login'\nprogram='{}'\n[providers.second]\nkind='codex'\nauth_mode='login'\nprogram='{}'\n[providers.anthropic-login]\nkind='claude'\nauth_mode='login'\nprogram='{}'\n[providers.api-only]\nkind='codex'\nauth_mode='api'\nbase_url='https://example.invalid/v1'\napi_key_env='OPENAI_API_KEY'\n",
        home.join("codex").display(), home.join("codex").display(), home.join("claude").display()
    )).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "child_login", "--nocapture"])
        .env("HORDE_LOGIN_SCENARIO", name)
        .env("HORDE_LOGIN_ROOT", dir.path().join("data"))
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("PATH", format!("{}:/usr/bin:/bin", home.display()))
        .env_remove("HORDE_WORKER_TOKEN")
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY");
    if worker {
        command.env("HORDE_WORKER_TOKEN", "fixture-worker-token");
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_stopped(home: &Path, program: &str) {
    let pid: i32 = std::fs::read_to_string(home.join(format!("{program}.pid")))
        .unwrap()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while horde::executor::process_alive(pid) {
        assert!(
            Instant::now() < deadline,
            "login process {pid} was not terminated"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn child_login() {
    let Ok(scenario) = std::env::var("HORDE_LOGIN_SCENARIO") else {
        return;
    };
    let root = std::env::var("HORDE_LOGIN_ROOT").unwrap();
    let home = std::env::var("HOME").unwrap();
    let home = Path::new(&home);
    let db = Store::open(Path::new(&root)).unwrap();
    if scenario == "worker" {
        assert!(
            provider_login::dispatch(
                &db,
                &json!({"action":"start","provider":"first","request_id":"worker"})
            )
            .is_err()
        );
        return;
    }
    if scenario == "invalid" {
        for request in [
            json!({"action":"start","provider":"first"}),
            json!({"action":"start","provider":"first","request_id":"bad","timeout_seconds":0}),
            json!({"action":"start","provider":"first","request_id":"bad","timeout_seconds":1801}),
            json!({"action":"start","provider":"api-only","request_id":"bad"}),
            json!({"action":"start","provider":"first","request_id":"bad","credential":"not-accepted"}),
            json!({"action":"status","session_id":"missing"}),
            json!({"action":"submit","session_id":"missing","input":"code"}),
        ] {
            assert!(
                provider_login::dispatch(&db, &request).is_err(),
                "accepted {request}"
            );
        }
        assert!(!home.join("codex.starts").exists());
        return;
    }
    let provider = if scenario == "claude" {
        "anthropic-login"
    } else {
        "first"
    };
    let program = if scenario == "claude" {
        "claude"
    } else {
        "codex"
    };
    let before = std::fs::read(horde::branding::config_dir().join("config.toml")).unwrap();
    let beginning = Instant::now();
    let session = start(
        &db,
        provider,
        "first-request",
        if scenario == "expire" { 1 } else { 20 },
    );
    assert!(
        beginning.elapsed() < Duration::from_secs(2),
        "start blocked on login"
    );
    assert!(session["session_id"].as_str().is_some());
    let prompt = wait_for(&db, &session, |value| {
        value["output"].as_str().is_some_and(|text| {
            text.contains(if scenario == "output" {
                "ready"
            } else {
                "https://example.invalid/authorize"
            })
        })
    });
    assert!(!prompt["output"].as_str().unwrap().contains('\u{1b}'));
    assert_ne!(prompt["provider_authentication"], "verified");
    match scenario.as_str() {
        "duplicate" => {
            let repeated = start(&db, provider, "first-request", 20);
            assert_eq!(repeated["session_id"], session["session_id"]);
            assert!(
                provider_login::dispatch(
                    &db,
                    &json!({"action":"start","provider":"second","request_id":"other"})
                )
                .is_err()
            );
            assert!(provider_login::dispatch(&db, &json!({"action":"start","provider":"anthropic-login","request_id":"first-request"})).is_err());
            assert_eq!(
                std::fs::read_to_string(home.join("codex.starts")).unwrap(),
                "x"
            );
            call(
                &db,
                json!({"action":"cancel","session_id":session["session_id"]}),
            );
            assert_eq!(terminal(&db, &session)["status"], "cancelled");
        }
        "cancel" => {
            call(
                &db,
                json!({"action":"cancel","session_id":session["session_id"]}),
            );
            assert_eq!(terminal(&db, &session)["status"], "cancelled");
            assert_stopped(home, program);
        }
        "expire" => {
            assert_eq!(terminal(&db, &session)["status"], "expired");
            assert_stopped(home, program);
        }
        "shutdown" => {
            provider_login::shutdown(Path::new(&root));
            assert_eq!(terminal(&db, &session)["status"], "cancelled");
            assert_stopped(home, program);
        }
        "cancel_verify" => {
            call(
                &db,
                json!({"action":"submit","session_id":session["session_id"],"input":"code"}),
            );
            wait_for(&db, &session, |value| {
                value["status"] == "verifying" && home.join("status-child.pid").exists()
            });
            call(
                &db,
                json!({"action":"cancel","session_id":session["session_id"]}),
            );
            assert_eq!(terminal(&db, &session)["status"], "cancelled");
            assert_stopped(home, "status");
            assert_stopped(home, "status-child");
        }
        _ => {
            let secret = "fixture-one-time-code";
            let submitted = call(
                &db,
                json!({"action":"submit","session_id":session["session_id"],"input":secret}),
            );
            assert!(!submitted.to_string().contains(secret));
            let finished = terminal(&db, &session);
            assert!(
                !finished.to_string().contains(secret),
                "echoed login input reached response"
            );
            assert!(finished["output"].as_str().unwrap().len() <= 16 * 1024);
            if matches!(scenario.as_str(), "verify_failure" | "login_failure") {
                assert_eq!(finished["status"], "failed");
                assert_ne!(finished["provider_authentication"], "verified");
            } else {
                assert_eq!(finished["status"], "succeeded");
                assert_eq!(finished["provider_authentication"], "verified");
                assert_eq!(finished["capacity"], "unknown");
            }
            assert!(provider_login::dispatch(&db, &json!({"action":"submit","session_id":session["session_id"],"input":"late-code"})).is_err());
            let retried = start(&db, provider, "first-request", 20);
            assert_eq!(retried["session_id"], session["session_id"]);
            assert_eq!(retried["status"], finished["status"]);
            assert_eq!(
                std::fs::read_to_string(home.join(format!("{program}.starts"))).unwrap(),
                "x"
            );
        }
    }
    assert_eq!(
        std::fs::read(horde::branding::config_dir().join("config.toml")).unwrap(),
        before
    );
}

#[test]
fn codex_handoff_accepts_input_without_cli_access() {
    fixture("codex", false);
}
#[test]
fn claude_handoff_accepts_input_without_cli_access() {
    fixture("claude", false);
}
#[test]
fn prompts_are_visible_before_newline_and_across_split_writes() {
    fixture("split", false);
}
#[test]
fn session_request_ids_are_idempotent_and_shared_logins_are_serialized() {
    fixture("duplicate", false);
}
#[test]
fn cancel_terminates_the_login_process() {
    fixture("cancel", false);
}
#[test]
fn expiration_terminates_the_login_process() {
    fixture("expire", false);
}
#[test]
fn shutdown_terminates_the_login_process() {
    fixture("shutdown", false);
}
#[test]
fn login_success_requires_authentication_status_verification() {
    fixture("verify_failure", false);
}
#[test]
fn output_is_bounded_without_stalling_the_login_process() {
    fixture("output", false);
}
#[test]
fn invalid_requests_and_unknown_sessions_do_not_start_login() {
    fixture("invalid", false);
}
#[test]
fn worker_credentials_cannot_manage_provider_login() {
    fixture("worker", true);
}

#[test]
fn nonzero_login_exit_remains_failed_on_retried_start() {
    fixture("login_failure", false);
}

#[test]
fn cancellation_during_verification_terminates_the_status_process() {
    fixture("cancel_verify", false);
}

struct LoginDaemon {
    directory: std::sync::Arc<tempfile::TempDir>,
    root: PathBuf,
    child: Child,
}

impl LoginDaemon {
    fn new(crash: bool) -> Self {
        let directory = std::sync::Arc::new(tempfile::tempdir_in("/tmp").unwrap());
        let home = directory.path().join("home");
        let config = directory.path().join("config/horde");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&config).unwrap();
        mock(
            &home,
            "codex",
            if crash {
                "printf 'https://example.invalid/authorize'; while :; do /bin/sleep 1; done"
            } else {
                "printf 'https://example.invalid/authorize'; read answer; [ \"$answer\" = rpc-code ]"
            },
            true,
        );
        std::fs::write(
            config.join("config.toml"),
            format!(
                "[providers.first]\nkind='codex'\nauth_mode='login'\nprogram='{}'\n",
                home.join("codex").display()
            ),
        )
        .unwrap();
        let root = directory.path().join("data");
        let child = Self::spawn(directory.path(), &root);
        let daemon = Self {
            directory,
            root,
            child,
        };
        daemon.ready();
        daemon
    }

    fn sibling(&self) -> Self {
        let root = self.directory.path().join("data-other");
        let child = Self::spawn(self.directory.path(), &root);
        let daemon = Self {
            directory: self.directory.clone(),
            root,
            child,
        };
        daemon.ready();
        daemon
    }

    fn spawn(directory: &Path, root: &Path) -> Child {
        std::fs::create_dir_all(root).unwrap();
        // Independent Horde configurations can still share one CLI login store.
        std::fs::create_dir_all(root.join("config/horde")).unwrap();
        std::fs::copy(
            directory.join("config/horde/config.toml"),
            root.join("config/horde/config.toml"),
        )
        .unwrap();
        let log = std::fs::File::create(root.join("daemon.log")).unwrap();
        Command::new(env!("CARGO_BIN_EXE_horde"))
            .arg("--data-dir")
            .arg(root)
            .arg("daemon")
            .env_clear()
            .env("HOME", directory.join("home"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap()
    }

    fn rpc(&self, method: &str, arguments: Value) -> Result<Value, String> {
        let mut stream = std::os::unix::net::UnixStream::connect(self.root.join("daemon.sock"))
            .map_err(|error| error.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        writeln!(stream, "{}", json!({"method":method,"args":arguments}))
            .map_err(|error| error.to_string())?;
        let mut line = String::new();
        std::io::BufReader::new(stream)
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        let response: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
        if let Some(error) = response.get("error") {
            Err(error.to_string())
        } else {
            Ok(response["result"].clone())
        }
    }

    fn ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.rpc("runtime_status", json!({})).is_err() {
            assert!(
                Instant::now() < deadline,
                "daemon startup failed: {}",
                std::fs::read_to_string(self.root.join("daemon.log")).unwrap()
            );
            thread::sleep(Duration::from_millis(30));
        }
    }

    fn login(&self, arguments: Value) -> Value {
        self.rpc("provider_login", arguments).unwrap()
    }

    fn wait(&self, session: &Value, expected: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let value = self.login(json!({"action":"status","session_id":session["session_id"]}));
            if value["status"] == expected {
                return value;
            }
            assert!(Instant::now() < deadline, "expected {expected}: {value}");
            thread::sleep(Duration::from_millis(30));
        }
    }

    fn stop(&mut self) {
        let response = self.rpc("shutdown", json!({}));
        // The daemon may consume shutdown.request and exit before its detached
        // RPC task writes the acknowledgment. Only an empty reply is allowed;
        // the successful, bounded process exit below confirms shutdown.
        assert!(
            response.is_ok()
                || response.as_ref().err().is_some_and(|error| {
                    error == "EOF while parsing a value at line 1 column 0"
                }),
            "shutdown RPC failed: {response:?}"
        );
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "daemon shutdown failed: {status}");
                break;
            }
            assert!(Instant::now() < deadline, "daemon shutdown timed out");
            thread::sleep(Duration::from_millis(30));
        }
    }
}

impl Drop for LoginDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Ok(pid) = std::fs::read_to_string(self.directory.path().join("home/codex.pid"))
            && let Ok(pid) = pid.parse::<i32>()
            && horde::executor::process_alive(pid)
        {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
}

#[test]
fn daemon_handoff_survives_fresh_clients_without_blocking_other_requests() {
    let mut daemon = LoginDaemon::new(false);
    let session = daemon.login(
        json!({"action":"start","provider":"first","request_id":"rpc","timeout_seconds":20}),
    );
    let prompt = daemon.wait(&session, "awaiting_user");
    assert!(
        prompt["output"]
            .as_str()
            .unwrap()
            .contains("https://example.invalid/authorize")
    );
    let beginning = Instant::now();
    assert!(daemon.rpc("runtime_status", json!({})).is_ok());
    assert!(
        beginning.elapsed() < Duration::from_secs(2),
        "pending login blocked the daemon"
    );
    daemon.login(json!({"action":"submit","session_id":session["session_id"],"input":"rpc-code"}));
    assert_eq!(
        daemon.wait(&session, "succeeded")["provider_authentication"],
        "verified"
    );
    daemon.stop();
}

#[test]
fn daemon_restart_terminates_interrupted_login_and_invalidates_its_session() {
    let mut daemon = LoginDaemon::new(true);
    let session = daemon.login(json!({"action":"start","provider":"first","request_id":"interrupted","timeout_seconds":30}));
    daemon.wait(&session, "awaiting_user");
    let receipt = daemon
        .directory
        .path()
        .join("data/provider-logins")
        .join(format!("{}.json", session["session_id"].as_str().unwrap()));
    assert!(receipt.exists());
    daemon.child.kill().unwrap();
    daemon.child.wait().unwrap();
    let pid: i32 = std::fs::read_to_string(daemon.directory.path().join("home/codex.pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        horde::executor::process_alive(pid),
        "fixture must leave an interrupted login for recovery"
    );
    daemon.child = LoginDaemon::spawn(daemon.directory.path(), &daemon.root);
    daemon.ready();
    assert_stopped(&daemon.directory.path().join("home"), "codex");
    assert!(!receipt.exists());
    let error = daemon
        .rpc(
            "provider_login",
            json!({"action":"status","session_id":session["session_id"]}),
        )
        .unwrap_err();
    assert!(error.contains("start a new login"), "{error}");
    daemon.stop();
}

#[test]
fn daemons_sharing_provider_credentials_serialize_login_until_cancelled() {
    let mut first = LoginDaemon::new(false);
    let mut second = first.sibling();
    let session = first.login(
        json!({"action":"start","provider":"first","request_id":"owner","timeout_seconds":20}),
    );
    first.wait(&session, "awaiting_user");
    let rejected = second.rpc(
        "provider_login",
        json!({"action":"start","provider":"first","request_id":"contender","timeout_seconds":20}),
    );
    assert!(
        rejected.is_err(),
        "another daemon started login against the same credential store: {rejected:?}"
    );
    assert_eq!(
        std::fs::read_to_string(first.directory.path().join("home/codex.starts")).unwrap(),
        "x"
    );
    first.login(json!({"action":"cancel","session_id":session["session_id"]}));
    first.wait(&session, "cancelled");
    let next = second.login(
        json!({"action":"start","provider":"first","request_id":"contender","timeout_seconds":20}),
    );
    second.wait(&next, "awaiting_user");
    second.login(json!({"action":"submit","session_id":next["session_id"],"input":"rpc-code"}));
    second.wait(&next, "succeeded");
    second.stop();
    first.stop();
}
