use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use horde::{config::Settings, protocol, provider_signup, store::Store};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

const KEY: &str = "sk_tuara_signup_fixture_private";
const OLD_KEY: &str = "sk_tuara_previous_fixture";
const TOKEN: &str = "spt_signup_fixture_private";
const REQUEST: &str = "fixture-signup";

#[derive(Clone)]
struct HttpRequest {
    first: String,
    headers: String,
    body: Vec<u8>,
}

struct MockTuara {
    origin: String,
    requests: Arc<Mutex<Vec<HttpRequest>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl MockTuara {
    fn new(scenario: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let recorded = requests.clone();
        let done = stop.clone();
        let scenario = scenario.to_owned();
        let worker = thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("mock accept failed: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 2048];
                let header_end = loop {
                    if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        break index + 4;
                    }
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buffer[..count]);
                    assert!(bytes.len() < 65536);
                };
                let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
                let size = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while bytes.len() < header_end + size {
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buffer[..count]);
                }
                let request = HttpRequest {
                    first: headers.lines().next().unwrap().to_owned(),
                    headers: headers.clone(),
                    body: bytes[header_end..header_end + size].to_vec(),
                };
                recorded.lock().unwrap().push(request.clone());
                let paid = headers
                    .to_ascii_lowercase()
                    .contains("authorization: payment ");
                if paid && scenario == "uncertain" {
                    continue; // Tuara may have charged; deliberately lose the response.
                }
                let (code, extra, payload) = if request
                    .first
                    .starts_with("GET /account/api/v1/auth/introspect ")
                {
                    assert!(
                        headers
                            .to_ascii_lowercase()
                            .contains(&format!("authorization: bearer {KEY}\r\n"))
                    );
                    (
                        "200 OK",
                        String::new(),
                        json!({"data":{"kind":"api","scopes":["router:invoke"],"tokenId":"fixture-token","organizationId":if scenario == "wrong_org" { "org_other" } else { "org_fixture" }}}),
                    )
                } else if request.first.starts_with("POST /v1/agents ") && paid {
                    (
                        "201 Created",
                        String::new(),
                        json!({
                            "organization":{"id":"org_fixture","name":"Fixture organization"},
                            "key":{"raw_key":KEY,"scopes":["router:invoke","agent:read"]},
                            "payment":{"charge_cents":2048,"credit_units":2_000_000_000_u64,"fee_units":48_000_000,"card":{"last4":"4242"}},
                            "terms":{"version":"2026-09"}
                        }),
                    )
                } else if request.first.starts_with("POST /v1/agents ") {
                    assert!(!headers.to_ascii_lowercase().contains("authorization:"));
                    let request = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({
                        "amount":"2048","currency":"usd","decimals":2,
                        "methodDetails":{"networkId":"profile_test","paymentMethodTypes":["card"]}
                    })).unwrap());
                    let header = format!(
                        "WWW-Authenticate: Payment id=\"mppch_test\", realm=\"tuara.com\", method=\"stripe\", intent=\"charge\", request=\"{request}\"\r\n"
                    );
                    (
                        "402 Payment Required",
                        header,
                        json!({
                            "challenge_id":"mppch_test","charge_cents":2048,"credit_units":2_000_000_000_u64,
                            "fee_units":48_000_000,"fee_basis_points":240,
                            "terms_version":if scenario == "challenge_terms" { "2026-10" } else { "2026-09" }
                        }),
                    )
                } else {
                    (
                        "404 Not Found",
                        String::new(),
                        json!({"error":"unexpected mock endpoint"}),
                    )
                };
                let body = payload.to_string();
                let response = format!(
                    "HTTP/1.1 {code}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            origin,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn snapshot(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn paid_count(&self) -> usize {
        self.snapshot()
            .iter()
            .filter(|row| {
                row.headers
                    .to_ascii_lowercase()
                    .contains("authorization: payment ")
            })
            .count()
    }
}

impl Drop for MockTuara {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

fn public(value: &Value) {
    let text = value.to_string();
    for secret in [KEY, OLD_KEY, TOKEN, "4242"] {
        assert!(
            !text.contains(secret),
            "public signup output exposed private data"
        );
    }
}

fn call(db: &Store, args: Value) -> Value {
    let result = provider_signup::dispatch(db, &args).unwrap();
    public(&result);
    result
}

fn action(db: &Store, name: &str) -> Value {
    call(db, json!({"action":name,"request_id":REQUEST}))
}

fn attempt_resume(db: &Store) {
    match provider_signup::dispatch(db, &json!({"action":"resume","request_id":REQUEST})) {
        Ok(value) => public(&value),
        Err(error) => public(&json!({"error":error.to_string()})),
    }
}

fn start() -> Value {
    json!({"action":"start","request_id":REQUEST,"provider":"tuara-test",
        "organization_name":"Fixture organization","agent_name":"fixture-agent",
        "amount_cents":2000,"max_charge_cents":2048,"terms_version":"2026-09",
        "accept_terms":true,"replace_existing":true})
}

fn fixture(scenario: &str) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("home")).unwrap();
    let bin = directory.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let script = bin.join("link-cli");
    std::fs::write(&script, format!(r##"#!/bin/sh
printf '%s\n' "$*" >> "$HOME/wallet.log"
case " $* " in
  *' spend-request create '*) printf '%s' '[{{"id":"lsrq_test","amount":2048,"currency":"usd","network_id":"profile_test","credential_type":"shared_payment_token","status":"pending_approval","approval_url":"https://app.link.com/approve/lsrq_test"}}]' ;;
  *' spend-request retrieve '*)
    if [ -f "$HOME/requires-action" ]; then
      printf '%s' '[{{"id":"lsrq_test","amount":2048,"currency":"usd","network_id":"profile_test","credential_type":"shared_payment_token","status":"requires_action"}}]'
    elif [ -f "$HOME/pending" ]; then
      printf '%s' '[{{"id":"lsrq_test","amount":2048,"currency":"usd","network_id":"profile_test","credential_type":"shared_payment_token","status":"pending_approval","approval_url":"https://app.link.com/approve/lsrq_test"}}]'
    else
      printf '%s' '[{{"id":"lsrq_test","amount":2048,"currency":"usd","network_id":"profile_test","credential_type":"shared_payment_token","status":"approved","shared_payment_token":{{"id":"{TOKEN}"}}}}]'
    fi ;;
  *' spend-request cancel '*) printf '%s' '[{{"id":"lsrq_test","amount":2048,"currency":"usd","network_id":"profile_test","credential_type":"shared_payment_token","status":"canceled"}}]' ;;
  *) exit 2 ;;
esac
"##)).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_provider_signup", "--nocapture"])
        .env("HORDE_SIGNUP_SCENARIO", scenario)
        .env("HORDE_SIGNUP_ROOT", directory.path().join("data"))
        .env(
            "HORDE_SIGNUP_WALLET_LOG",
            directory.path().join("home/wallet.log"),
        )
        .env(
            "HORDE_SIGNUP_PENDING_FILE",
            directory.path().join("home/pending"),
        )
        .env("HOME", directory.path().join("home"))
        .env("XDG_CONFIG_HOME", directory.path().join("config"))
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env_remove("HORDE_WORKER_TOKEN")
        .env_remove("TUARA_API_KEY")
        .env_remove("LINK_ACCESS_TOKEN")
        .env_remove("TUARA_SIGNUP_TEST_KEY")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "scenario {scenario}:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn resume_in_new_process() -> Value {
    let result = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_resume_signup_receipt", "--nocapture"])
        .env("HORDE_SIGNUP_RESUME_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "receipt recovery subprocess failed: {} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8(result.stdout).unwrap();
    let value: Value = serde_json::from_str(
        stdout
            .lines()
            .find_map(|line| line.strip_prefix("SIGNUP_RESULT="))
            .unwrap(),
    )
    .unwrap();
    public(&value);
    value
}

#[test]
fn child_resume_signup_receipt() {
    if std::env::var("HORDE_SIGNUP_RESUME_CHILD").is_err() {
        return;
    }
    let root = PathBuf::from(std::env::var("HORDE_SIGNUP_ROOT").unwrap());
    let db = Store::open(&root).unwrap();
    assert_eq!(action(&db, "status")["status"], "credential_received");
    println!("SIGNUP_RESULT={}", action(&db, "resume"));
}

fn wallet_log() -> String {
    std::fs::read_to_string(std::env::var("HORDE_SIGNUP_WALLET_LOG").unwrap()).unwrap_or_default()
}

fn private_files(path: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert!(!metadata.file_type().is_symlink());
        assert_eq!(metadata.permissions().mode() & 0o077, 0);
        if metadata.is_dir() {
            files.extend(private_files(&path));
        } else {
            files.push(path);
        }
    }
    files
}

#[test]
fn child_provider_signup() {
    let Ok(scenario) = std::env::var("HORDE_SIGNUP_SCENARIO") else {
        return;
    };
    let server = MockTuara::new(&scenario);
    let config_dir = horde::branding::config_dir();
    std::fs::create_dir_all(&config_dir).unwrap();
    let endpoint = format!("{}/router/v1", server.origin);
    let config = format!(
        "# Preserve unrelated provider and role choices\n[providers.tuara-test]\nkind='tuara'\nauth_mode='api'\nbase_url='{endpoint}'\napi_key_env='TUARA_SIGNUP_TEST_KEY'\nmodel='chosen-model'\n[executors.worker]\nprovider='tuara-test'\n[executors.reviewer]\nprovider='tuara-test'\nmodel='review-model'\n[providers.unrelated]\nkind='codex'\nauth_mode='login'\n"
    );
    let config_path = config_dir.join("config.toml");
    std::fs::write(&config_path, &config).unwrap();
    let credentials =
        format!("TUARA_SIGNUP_TEST_KEY='{OLD_KEY}'\nOTHER_FIXTURE_KEY='unrelated-key'\n");
    let credentials_path = Settings::credentials_path();
    std::fs::write(&credentials_path, &credentials).unwrap();
    std::fs::set_permissions(&credentials_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let root = PathBuf::from(std::env::var("HORDE_SIGNUP_ROOT").unwrap());
    let db = Store::open(&root).unwrap();
    if scenario == "invalid" {
        for (field, value) in [
            ("accept_terms", json!(false)),
            ("terms_version", json!("")),
            ("amount_cents", json!(499)),
            ("amount_cents", json!(50_001)),
            ("amount_cents", json!(2000.5)),
            ("max_charge_cents", json!(1999)),
            ("replace_existing", json!(false)),
        ] {
            let mut request = start();
            request[field] = value;
            assert!(
                provider_signup::dispatch(&db, &request).is_err(),
                "accepted invalid {field}"
            );
        }
        assert_eq!(server.snapshot().len(), 0);
        assert!(wallet_log().is_empty());
        assert_eq!(
            std::fs::read_to_string(credentials_path).unwrap(),
            credentials
        );
        return;
    }
    if matches!(
        scenario.as_str(),
        "cap" | "challenge_terms" | "request_terms"
    ) {
        let mut request = start();
        if scenario == "cap" {
            request["max_charge_cents"] = json!(2047);
        }
        if scenario == "request_terms" {
            request["terms_version"] = json!("2026-08");
        }
        let result = provider_signup::dispatch(&db, &request);
        if let Ok(value) = result {
            assert_eq!(value["status"], "failed");
            public(&value);
        }
        assert_eq!(server.snapshot().len(), 1);
        assert!(wallet_log().is_empty());
        assert_eq!(
            std::fs::read_to_string(credentials_path).unwrap(),
            credentials
        );
        return;
    }
    let started = protocol::dispatch(&db, "provider_signup", start(), None).unwrap();
    public(&started);
    assert_eq!(started["request_id"], REQUEST);
    assert_eq!(started["status"], "awaiting_wallet");
    assert_eq!(started["quote"]["charge_cents"], 2048);
    assert_eq!(server.snapshot().len(), 1);
    assert!(wallet_log().is_empty());
    let repeated = call(&db, start());
    assert_eq!(repeated, action(&db, "status"));
    assert_eq!(server.snapshot().len(), 1);
    assert!(
        wallet_log().is_empty(),
        "status must not contact the wallet"
    );
    let mut mismatch = start();
    mismatch["agent_name"] = json!("another-agent");
    assert!(provider_signup::dispatch(&db, &mismatch).is_err());
    let other_db = Store::open(&root.with_file_name("other-daemon-data")).unwrap();
    let mut parallel = start();
    parallel["request_id"] = json!("another-signup");
    assert!(provider_signup::dispatch(&other_db, &parallel).is_err());
    assert_eq!(server.snapshot().len(), 1);
    if scenario == "cancel" {
        assert_eq!(action(&db, "cancel")["status"], "cancelled");
        attempt_resume(&db);
        assert_eq!(server.snapshot().len(), 1);
        assert!(wallet_log().is_empty());
        return;
    }
    let approved = action(&db, "resume");
    assert_eq!(approved["status"], "awaiting_approval");
    assert_eq!(
        approved["approval_url"],
        "https://app.link.com/approve/lsrq_test"
    );
    assert_eq!(wallet_log().lines().count(), 1);
    assert!(wallet_log().contains("--idempotency-key"));
    assert_eq!(server.paid_count(), 0);
    if scenario == "cancel_after_wallet" {
        assert_eq!(action(&db, "cancel")["status"], "cancelled");
        assert_eq!(server.paid_count(), 0);
        assert_eq!(
            wallet_log()
                .lines()
                .filter(|line| line.contains("spend-request cancel"))
                .count(),
            1,
            "cancel must revoke the unpaid Link authorization"
        );
        return;
    }
    let pending_path = PathBuf::from(std::env::var("HORDE_SIGNUP_PENDING_FILE").unwrap());
    if scenario == "requires_action" {
        std::fs::write(pending_path.with_file_name("requires-action"), "required").unwrap();
        let report = action(&db, "resume");
        assert_eq!(report["status"], "awaiting_approval");
        assert_eq!(report["wallet_action_required"], true);
        assert!(report["approval_url"].is_null());
        assert!(report["message"].as_str().unwrap().contains("Link"));
        assert_eq!(server.paid_count(), 0);
        assert_eq!(
            wallet_log()
                .lines()
                .filter(|line| line.contains("spend-request create"))
                .count(),
            1
        );
        return;
    }
    if scenario == "pending" {
        std::fs::write(&pending_path, "pending").unwrap();
        assert_eq!(action(&db, "resume")["status"], "awaiting_approval");
        assert_eq!(server.paid_count(), 0);
        std::fs::remove_file(pending_path).unwrap();
    }
    drop(db);
    let db = Store::open(&root).unwrap();
    assert_eq!(action(&db, "status")["status"], "awaiting_approval");
    if scenario == "uncertain" {
        attempt_resume(&db);
        assert_eq!(action(&db, "status")["status"], "uncertain");
        for _ in 0..3 {
            attempt_resume(&db);
        }
        assert_eq!(
            server.paid_count(),
            1,
            "an ambiguous charge must never be retried"
        );
        assert_eq!(
            std::fs::read_to_string(credentials_path).unwrap(),
            credentials
        );
        return;
    }
    assert_eq!(action(&db, "resume")["status"], "credential_received");
    assert_eq!(server.paid_count(), 1);
    assert_eq!(
        std::fs::read_to_string(&credentials_path).unwrap(),
        credentials
    );
    let files = private_files(&config_dir.join("provider-signups"));
    assert!(
        files
            .iter()
            .any(|path| std::fs::read_to_string(path).is_ok_and(|body| body.contains(KEY))),
        "received key must survive a restart in a private receipt"
    );
    if scenario == "response_before_receipt" {
        let path = files
            .iter()
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .unwrap();
        let mut receipt: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(receipt["status"], "credential_received");
        // Simulate a crash after fsync of the one-time response but before its state update.
        receipt["status"] = json!("submitting");
        std::fs::write(path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        drop(db);
        assert_eq!(resume_in_new_process()["status"], "succeeded");
        assert_eq!(server.paid_count(), 1);
        return;
    }
    drop(db);
    let db = Store::open(&root).unwrap();
    let requests_before_status = server.snapshot().len();
    let wallet_before_status = wallet_log();
    assert_eq!(action(&db, "status")["status"], "credential_received");
    assert_eq!(
        server.snapshot().len(),
        requests_before_status,
        "status must not verify or repay"
    );
    assert_eq!(wallet_log(), wallet_before_status);
    if scenario == "wrong_org" {
        assert_eq!(action(&db, "resume")["status"], "credential_received");
        assert_eq!(
            std::fs::read_to_string(&credentials_path).unwrap(),
            credentials
        );
        assert_eq!(server.paid_count(), 1);
        assert_eq!(action(&db, "resume")["status"], "credential_received");
        assert_eq!(
            server.paid_count(),
            1,
            "a mismatched organization must not cause another payment"
        );
        return;
    }
    if scenario.starts_with("drift_") {
        let expected_credentials = if scenario == "drift_credential" {
            credentials.replace(OLD_KEY, "newer-fixture-key")
        } else {
            credentials.clone()
        };
        let expected_config = if scenario == "drift_config" {
            config.replace("TUARA_SIGNUP_TEST_KEY", "NEW_SIGNUP_TEST_KEY")
        } else {
            config.clone()
        };
        std::fs::write(&credentials_path, &expected_credentials).unwrap();
        std::fs::write(&config_path, &expected_config).unwrap();
        attempt_resume(&db);
        assert_ne!(action(&db, "status")["status"], "succeeded");
        assert_eq!(
            std::fs::read_to_string(&credentials_path).unwrap(),
            expected_credentials
        );
        assert_eq!(
            std::fs::read_to_string(&config_path).unwrap(),
            expected_config
        );
        assert_eq!(server.paid_count(), 1);
        std::fs::write(&credentials_path, &credentials).unwrap();
        std::fs::write(&config_path, &config).unwrap();
        assert_eq!(resume_in_new_process()["status"], "succeeded");
        assert_eq!(
            server.paid_count(),
            1,
            "retrying credential import must not repeat payment"
        );
        assert_eq!(
            horde::secrets::parse(&std::fs::read_to_string(&credentials_path).unwrap()).unwrap()["TUARA_SIGNUP_TEST_KEY"],
            KEY
        );
        return;
    }
    assert_eq!(resume_in_new_process()["status"], "succeeded");
    let count = server.snapshot().len();
    assert_eq!(call(&db, start())["status"], "succeeded");
    assert_eq!(action(&db, "resume")["status"], "succeeded");
    assert_eq!(server.snapshot().len(), count);
    assert_eq!(
        wallet_log()
            .lines()
            .filter(|line| line.contains("spend-request create"))
            .count(),
        1
    );
    assert!(wallet_log().contains("--include shared_payment_token"));
    assert!(wallet_log().contains("--format json"));
    let rows = server.snapshot();
    let posts = rows
        .iter()
        .filter(|row| row.first.starts_with("POST "))
        .collect::<Vec<_>>();
    assert_eq!(posts.len(), 2);
    assert_eq!(
        posts[0].body, posts[1].body,
        "payment retry must preserve the quoted body exactly"
    );
    let paid_header = posts[1]
        .headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("authorization")
                .then(|| value.trim().strip_prefix("Payment ").unwrap())
        })
        .unwrap();
    let credential: Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(paid_header).unwrap()).unwrap();
    assert!(credential.to_string().contains(TOKEN));
    let settings = Settings::load_user().unwrap();
    assert_eq!(
        settings.provider("tuara-test").unwrap().model.as_deref(),
        Some("chosen-model")
    );
    assert_eq!(
        settings.executor("reviewer").unwrap().model.as_deref(),
        Some("review-model")
    );
    assert_eq!(settings.executors["worker"].provider(), "tuara-test");
    assert!(settings.provider("unrelated").is_some());
    let saved =
        horde::secrets::parse(&std::fs::read_to_string(&credentials_path).unwrap()).unwrap();
    assert_eq!(saved["TUARA_SIGNUP_TEST_KEY"], KEY);
    assert_eq!(saved["OTHER_FIXTURE_KEY"], "unrelated-key");
    assert_eq!(
        std::fs::metadata(credentials_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn signup_success_is_durable_deduplicated_private_and_preserves_provider_choices() {
    fixture("success");
}
#[test]
fn signup_recovers_a_durable_response_before_receipt_transition_in_a_new_process() {
    fixture("response_before_receipt");
}
#[test]
fn signup_rejects_a_verified_key_for_a_different_organization_without_repaying() {
    fixture("wrong_org");
}
#[test]
fn signup_reports_required_wallet_action_without_an_approval_url_or_payment() {
    fixture("requires_action");
}
#[test]
fn signup_waits_for_wallet_approval_without_recreating_payment() {
    fixture("pending");
}
#[test]
fn signup_rejects_unaccepted_terms_invalid_amount_and_unapproved_replacement() {
    fixture("invalid");
}
#[test]
fn signup_checks_charge_cap_and_challenge_terms_before_wallet() {
    fixture("cap");
    fixture("challenge_terms");
    fixture("request_terms");
}
#[test]
fn signup_never_repeats_an_ambiguous_paid_request() {
    fixture("uncertain");
}
#[test]
fn signup_preserves_newer_credentials_and_configuration_after_receiving_key() {
    fixture("drift_credential");
    fixture("drift_config");
}
#[test]
fn signup_cancellation_before_payment_does_not_call_wallet() {
    fixture("cancel");
}
#[test]
fn signup_cancellation_after_wallet_revokes_unpaid_authorization() {
    fixture("cancel_after_wallet");
}
#[test]
fn signup_is_hidden_from_workers_and_project_bound_connections() {
    let directory = tempfile::tempdir().unwrap();
    let db = Store::open(directory.path()).unwrap();
    assert!(!protocol::worker_allowed("provider_signup"));
    assert!(!protocol::project_allowed("provider_signup"));
    let args = json!({"action":"status","request_id":"unknown"});
    let error =
        protocol::dispatch_scoped(&db, "provider_signup", args.clone(), None, Some("default"))
            .unwrap_err()
            .to_string();
    assert!(error.contains("project-bound credentials"), "{error}");
    assert!(
        protocol::dispatch(&db, "provider_signup", args, Some("invalid-worker-token")).is_err()
    );
    let project = protocol::dispatch(
        &db,
        "project_create",
        json!({"name":"signup-isolated"}),
        None,
    )
    .unwrap()["id"]
        .clone();
    assert!(
        protocol::dispatch(
            &db,
            "provider_signup",
            json!({"action":"status","request_id":"unknown","project":project}),
            None
        )
        .is_err()
    );
}
