use horde::{config::Settings, provider_login, store::Store};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const KEY: &str = "tuara-fixture-replacement-key";
const OLD_KEY: &str = "previous-fixture-key";

fn call(db: &Store, args: Value) -> Value {
    provider_login::dispatch(db, &args).unwrap()
}

fn wait_for(db: &Store, session: &Value, predicate: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let report = call(
            db,
            json!({"action":"status","session_id":session["session_id"]}),
        );
        assert!(!report.to_string().contains(KEY));
        if predicate(&report) {
            return report;
        }
        assert!(
            Instant::now() < deadline,
            "session did not advance before the deadline"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn terminal(db: &Store, session: &Value) -> Value {
    wait_for(db, session, |r| {
        matches!(
            r["status"].as_str(),
            Some("succeeded" | "failed" | "expired" | "cancelled")
        )
    })
}

struct MockAccount {
    origin: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    response_released: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl MockAccount {
    fn new(scenario: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let response_released = Arc::new(AtomicBool::new(!scenario.starts_with("drift_")));
        let thread_released = response_released.clone();
        let thread_requests = requests.clone();
        let thread_stop = stop.clone();
        let scenario = scenario.to_owned();
        let redirect = format!("{origin}/must-not-follow");
        let worker = thread::spawn(move || {
            while !thread_stop.load(Ordering::SeqCst) {
                let (mut connection, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("mock accept: {error}"),
                };
                // macOS can inherit the listener's nonblocking flag on accept.
                connection.set_nonblocking(false).unwrap();
                connection
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut chunk = [0u8; 2048];
                while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                    let count = connection.read(&mut chunk).unwrap();
                    assert!(count > 0, "incomplete request");
                    request.extend_from_slice(&chunk[..count]);
                    assert!(request.len() < 16 * 1024);
                }
                thread_requests
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(request).unwrap());
                if matches!(scenario.as_str(), "cancel_verify" | "expire_verify") {
                    thread::sleep(Duration::from_millis(1400));
                }
                while !thread_released.load(Ordering::SeqCst) && !thread_stop.load(Ordering::SeqCst)
                {
                    thread::sleep(Duration::from_millis(5));
                }
                let payload = json!({"data":{"kind":"api","scopes":["router:invoke"],"tokenId":"fixture-token","organizationId":"fixture-org"}});
                let (code, body, extra) = match scenario.as_str() {
                    "unauthorized" => ("401 Unauthorized", json!({"error":KEY}).to_string(), String::new()),
                    "malformed" => ("200 OK", format!("not-json {KEY}"), String::new()),
                    "scope" => ("200 OK", json!({"data":{"kind":"api","scopes":["account:read"],"tokenId":"fixture-token","organizationId":"fixture-org"}}).to_string(), String::new()),
                    "oauth" => ("200 OK", json!({"data":{"kind":"oauth","scopes":["router:invoke"],"tokenId":"fixture-token","organizationId":"fixture-org"}}).to_string(), String::new()),
                    "redirect" => ("302 Found", payload.to_string(), format!("Location: {redirect}\r\n")),
                    "oversized" => ("200 OK", " ".repeat(1024 * 1024) + &payload.to_string(), String::new()),
                    _ => ("200 OK", payload.to_string(), String::new()),
                };
                let response = format!(
                    "HTTP/1.1 {code}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
                    body.len()
                );
                // Cancellation and bounded reads may close the socket before the body finishes.
                let _ = connection.write_all(response.as_bytes());
            }
        });
        Self {
            origin,
            requests,
            stop,
            response_released,
            worker: Some(worker),
        }
    }

    fn release_response(&self) {
        self.response_released.store(true, Ordering::SeqCst);
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Drop for MockAccount {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn fixture(scenario: &str) {
    let dir = tempfile::tempdir().unwrap();
    let result = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_tuara_login", "--nocapture"])
        .env("HORDE_TUARA_LOGIN_SCENARIO", scenario)
        .env("HORDE_TUARA_LOGIN_ROOT", dir.path().join("data"))
        .env("HOME", dir.path().join("home"))
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("PATH", dir.path().join("no-cli-installed"))
        .env("TUARA_TEST_KEY", "obsolete-inherited-fixture-key")
        .env_remove("HORDE_WORKER_TOKEN")
        .env_remove("TUARA_API_KEY")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn child_tuara_login() {
    let Ok(scenario) = std::env::var("HORDE_TUARA_LOGIN_SCENARIO") else {
        return;
    };
    if scenario.starts_with("preset") {
        preset_handoff(&scenario);
        return;
    }
    let server = MockAccount::new(&scenario);
    let config_dir = horde::branding::config_dir();
    std::fs::create_dir_all(&config_dir).unwrap();
    let endpoint = if scenario == "untrusted_host" {
        "https://untrusted.invalid/router/v1".to_owned()
    } else {
        format!("{}/router/v1", server.origin)
    };
    let config_text = format!(
        "# keep unrelated providers\n[providers.tuara-test]\nkind='tuara'\nauth_mode='api'\nbase_url='{endpoint}'\napi_key_env='TUARA_TEST_KEY'\nmodel='chosen-provider-model'\n[executors.worker]\nprovider='tuara-test'\n[executors.reviewer]\nprovider='tuara-test'\nmodel='chosen-reviewer-model'\n[providers.unrelated]\nkind='codex'\nauth_mode='login'\n"
    );
    std::fs::write(config_dir.join("config.toml"), &config_text).unwrap();
    let expected_config = match scenario.as_str() {
        "drift_base" => config_text.replace(&endpoint, "https://tuara.com/router/v1"),
        "drift_env" => config_text.replace("TUARA_TEST_KEY", "NEW_TUARA_TEST_KEY"),
        _ => config_text.clone(),
    };
    let credentials_text = format!(
        "# keep another account\nTUARA_TEST_KEY='{OLD_KEY}'\nOTHER_TEST_KEY='other-fixture-key'\n"
    );
    let credentials_path = Settings::credentials_path();
    std::fs::write(&credentials_path, &credentials_text).unwrap();
    std::fs::set_permissions(&credentials_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let root = std::env::var("HORDE_TUARA_LOGIN_ROOT").unwrap();
    let db = Store::open(Path::new(&root)).unwrap();
    let config = Settings::load_user()
        .unwrap()
        .provider("tuara-test")
        .unwrap();
    for source in ["provider", "local_budget"] {
        horde::capacity::observe(
            &db,
            &horde::capacity::Snapshot {
                account: horde::capacity::account(&config),
                provider: "tuara".into(),
                window: source.into(),
                used_percent: Some(100.0),
                reset_at: None,
                observed_at: horde::store::now(),
                source: source.into(),
            },
        )
        .unwrap();
    }
    let start = json!({"action":"start","provider":"tuara-test","request_id":"tuara-request","timeout_seconds":if scenario.starts_with("expire") { 1 } else { 10 }});
    if scenario == "untrusted_host" {
        assert!(provider_login::dispatch(&db, &start).is_err());
        assert_eq!(server.count(), 0);
        assert_eq!(
            std::fs::read_to_string(&credentials_path).unwrap(),
            credentials_text
        );
        return;
    }
    let session = call(&db, start.clone());
    let prompt = wait_for(&db, &session, |r| r["status"] == "awaiting_user");
    assert_eq!(prompt["method"], "api_key");
    assert!(
        prompt["output"]
            .as_str()
            .unwrap()
            .contains(&format!("{}/app/buy/keys", server.origin))
    );
    assert_eq!(
        server.count(),
        0,
        "start must not contact provider or consume model capacity"
    );
    for input in [
        String::new(),
        "   ".into(),
        "key\nsecond-line".into(),
        "x".repeat(4097),
    ] {
        assert!(
            provider_login::dispatch(
                &db,
                &json!({"action":"submit","session_id":session["session_id"],"input":input})
            )
            .is_err()
        );
    }
    assert_eq!(server.count(), 0, "invalid input must not contact provider");
    let repeated = call(&db, start.clone());
    assert_eq!(repeated["session_id"], session["session_id"]);
    assert_eq!(server.count(), 0);
    match scenario.as_str() {
        "cancel" => {
            call(
                &db,
                json!({"action":"cancel","session_id":session["session_id"]}),
            );
        }
        "expire" => (),
        _ => {
            let submitted = call(
                &db,
                json!({"action":"submit","session_id":session["session_id"],"input":KEY}),
            );
            assert!(!submitted.to_string().contains(KEY));
            if scenario.starts_with("drift_") {
                wait_for(&db, &session, |r| {
                    r["status"] == "verifying" && server.count() == 1
                });
                std::fs::write(config_dir.join("config.toml"), &expected_config).unwrap();
                server.release_response();
            }
            if scenario == "cancel_verify" {
                wait_for(&db, &session, |r| {
                    r["status"] == "verifying" && server.count() == 1
                });
                call(
                    &db,
                    json!({"action":"cancel","session_id":session["session_id"]}),
                );
            }
        }
    }
    let finished = terminal(&db, &session);
    let expected = match scenario.as_str() {
        "success" => "succeeded",
        "cancel" | "cancel_verify" => "cancelled",
        "expire" | "expire_verify" => "expired",
        _ => "failed",
    };
    assert_eq!(finished["status"], expected);
    if scenario.ends_with("_verify") {
        thread::sleep(Duration::from_millis(1500));
    }
    assert_eq!(call(&db, start)["status"], expected);
    let saved = std::fs::read_to_string(&credentials_path).unwrap();
    if expected == "succeeded" {
        assert_eq!(finished["provider_authentication"], "verified");
        assert_eq!(finished["credential_activation"], "next_invocation");
        assert_eq!(finished["capacity"], "unknown");
        assert_eq!(horde::config::credential("TUARA_TEST_KEY").unwrap(), KEY);
        let values = horde::secrets::parse(&saved).unwrap();
        assert_eq!(values["TUARA_TEST_KEY"], KEY);
        assert_eq!(values["OTHER_TEST_KEY"], "other-fixture-key");
        assert!(saved.contains("# keep another account"));
        assert_eq!(
            std::fs::metadata(&credentials_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let settings = Settings::load_user().unwrap();
        assert!(settings.provider("unrelated").is_some());
        let replacement = settings.provider("tuara-test").unwrap();
        assert_eq!(replacement.base_url, endpoint);
        assert_eq!(replacement.api_key_env, "TUARA_TEST_KEY");
        assert_eq!(replacement.model.as_deref(), Some("chosen-provider-model"));
        for role in ["worker", "reviewer"] {
            assert_eq!(settings.executors[role].provider(), "tuara-test");
            assert_eq!(
                settings.executor(role).unwrap().api_key_env,
                "TUARA_TEST_KEY"
            );
        }
        assert_eq!(
            settings.executor("worker").unwrap().model.as_deref(),
            Some("chosen-provider-model")
        );
        assert_eq!(
            settings.executor("reviewer").unwrap().model.as_deref(),
            Some("chosen-reviewer-model")
        );
        assert!(
            horde::capacity::credential_generation(&db, &config)
                .unwrap()
                .is_some()
        );
    } else {
        assert_ne!(finished["provider_authentication"], "verified");
        assert_eq!(saved, credentials_text);
        assert_eq!(
            std::fs::read_to_string(config_dir.join("config.toml")).unwrap(),
            expected_config
        );
        assert!(
            horde::capacity::credential_generation(&db, &config)
                .unwrap()
                .is_none()
        );
    }
    let capacity = db
        .rows("SELECT source FROM account_capacity ORDER BY source", &[])
        .unwrap();
    assert_eq!(capacity.len(), if expected == "succeeded" { 1 } else { 2 });
    assert!(capacity.iter().any(|row| row["source"] == "local_budget"));
    let requests = server.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        if matches!(scenario.as_str(), "cancel" | "expire") {
            0
        } else {
            1
        }
    );
    for request in requests.iter() {
        assert!(
            request.starts_with("GET /account/api/v1/auth/introspect HTTP/1.1\r\n"),
            "unexpected endpoint: {request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains(&format!("authorization: bearer {KEY}\r\n"))
        );
    }
}

#[test]
fn tuara_handoff_verifies_saves_and_activates_key_without_cli_or_model_call() {
    fixture("success");
}
#[test]
fn tuara_rejects_failed_or_unsuitable_introspection_without_replacing_credentials() {
    for scenario in [
        "unauthorized",
        "malformed",
        "scope",
        "oauth",
        "redirect",
        "oversized",
    ] {
        fixture(scenario);
    }
}
#[test]
fn tuara_cancel_and_expiration_preserve_credentials() {
    for scenario in ["cancel", "expire", "cancel_verify", "expire_verify"] {
        fixture(scenario);
    }
}
#[test]
fn tuara_refuses_to_send_keys_to_an_untrusted_endpoint() {
    fixture("untrusted_host");
}

fn preset_handoff(scenario: &str) {
    let config_dir = horde::branding::config_dir();
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("config.toml");
    let existing = if scenario == "preset_alias" {
        let config = "[providers.default]\nkind='tuara'\nauth_mode='api'\nbase_url='https://tuara.com/router/v1'\napi_key_env='EXISTING_TUARA_KEY'\nmodel='existing-model-choice'\n";
        std::fs::write(&config_path, config).unwrap();
        Some(config.to_owned())
    } else {
        None
    };
    let root = std::env::var("HORDE_TUARA_LOGIN_ROOT").unwrap();
    let db = Store::open(Path::new(&root)).unwrap();
    let session = call(
        &db,
        json!({"action":"start","provider":"tuara","request_id":"preset","timeout_seconds":10}),
    );
    let prompt = wait_for(&db, &session, |r| r["status"] == "awaiting_user");
    assert_eq!(prompt["method"], "api_key");
    assert_eq!(prompt["provider"], "tuara");
    assert!(
        prompt["output"]
            .as_str()
            .unwrap()
            .contains("https://tuara.com/app/buy/keys")
    );
    call(
        &db,
        json!({"action":"cancel","session_id":session["session_id"]}),
    );
    assert_eq!(terminal(&db, &session)["status"], "cancelled");
    assert_eq!(std::fs::read_to_string(config_path).ok(), existing);
    assert!(!Settings::credentials_path().exists());
}

#[test]
fn tuara_preset_offers_key_handoff_without_configuring_an_account() {
    fixture("preset");
}

#[test]
fn tuara_preset_accepts_an_existing_default_provider_without_changing_it() {
    fixture("preset_alias");
}

#[test]
fn tuara_configuration_drift_during_verification_preserves_existing_credentials() {
    for scenario in ["drift_base", "drift_env"] {
        fixture(scenario);
    }
}
