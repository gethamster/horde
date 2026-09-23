use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use horde::{config::Settings, protocol, provider_signup::topup, store::Store};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

const KEY: &str = "sk_tuara_topup_fixture_private";
const TOKEN: &str = "spt_topup_fixture_private";
const PROVIDER: &str = "tuara-test";

#[derive(Clone)]
struct Request {
    first: String,
    headers: String,
    body: Vec<u8>,
}
#[derive(Clone)]
struct Account {
    available_cents: Option<i64>,
    organization: String,
}
struct MockTuara {
    origin: String,
    requests: Arc<Mutex<Vec<Request>>>,
    account: Arc<Mutex<Account>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

fn read_request(stream: &mut std::net::TcpStream) -> Request {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 2048];
    let offset = loop {
        if let Some(index) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break index + 4;
        }
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() < 65536);
    };
    let headers = String::from_utf8(bytes[..offset].to_vec()).unwrap();
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    while bytes.len() < offset + length {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
    }
    Request {
        first: headers.lines().next().unwrap().to_owned(),
        headers,
        body: bytes[offset..offset + length].to_vec(),
    }
}

impl MockTuara {
    fn new(scenario: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let account = Arc::new(Mutex::new(Account {
            available_cents: Some(if scenario == "high_balance" {
                10000
            } else {
                100
            }),
            organization: "org_fixture".into(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (recorded, current, done) = (requests.clone(), account.clone(), stop.clone());
        let scenario = scenario.to_owned();
        let worker = thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("mock accept: {error}"),
                };
                let request = read_request(&mut stream);
                recorded.lock().unwrap().push(request.clone());
                let headers = request.headers.to_ascii_lowercase();
                assert!(
                    headers.contains(&format!("bearer {KEY}")),
                    "every topup/account request must authenticate"
                );
                let paid = headers.contains("payment ");
                if paid && scenario.starts_with("uncertain") {
                    continue;
                }
                let account = current.lock().unwrap().clone();
                let (code, extra, body) = if request
                    .first
                    .starts_with("GET /account/api/v1/auth/introspect ")
                {
                    (
                        "200 OK",
                        String::new(),
                        json!({"data":{"kind":"api","scopes":["agent:manage","router:invoke"],"organizationId":account.organization,"tokenId":"token_fixture"}}),
                    )
                } else if request.first.starts_with("GET /api/v1/buy/account ") {
                    let payload = if let Some(cents) = account.available_cents {
                        json!({"buyer_id":account.organization,"balance_units":cents * 1_000_000,"pending_units":0,"available_units":cents * 1_000_000})
                    } else {
                        json!({"buyer_id":account.organization,"balance_units":null,"pending_units":0,"available_units":null})
                    };
                    ("200 OK", String::new(), payload)
                } else if request.first.starts_with("POST /v1/account/topup ")
                    && paid
                    && !bound_to_challenge(&request.headers, "mppch_topup")
                {
                    (
                        "402 Payment Required",
                        String::new(),
                        json!({"code":"payment_declined"}),
                    )
                } else if request.first.starts_with("POST /v1/account/topup ") && paid {
                    current.lock().unwrap().available_cents = Some(2100);
                    (
                        "200 OK",
                        String::new(),
                        json!({
                            "payment":{"payment_intent_id":"pi_fixture","charge_cents":2048,"credit_units":2_000_000_000_u64,"fee_units":48_000_000,"card":{"last4":"4242"}},
                            "balance":{"balance_units":2_100_000_000_u64,"available_units":2_100_000_000_u64},
                            "funding_event":{"credit_units":2_000_000_000_u64,"fee_units":48_000_000,"charge_units":2_048_000_000_u64,"payment_intent_id":"pi_fixture","channel":"machine","rail":"card","provider_namespace":"fixture_namespace"}
                        }),
                    )
                } else if request.first.starts_with("POST /v1/account/topup ") {
                    let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"amount":"2048","currency":"usd","externalId":"mppch_topup","methodDetails":{"networkId":"profile_test","paymentMethodTypes":["card"]}})).unwrap());
                    let header = format!(
                        "WWW-Authenticate: Payment id=\"mpp_challenge_id\", realm=\"tuara.com\", method=\"stripe\", intent=\"charge\", request=\"{encoded}\"\r\n"
                    );
                    (
                        "402 Payment Required",
                        header,
                        json!({"challenge_id":"mppch_topup","charge_cents":2048,"credit_units":2_000_000_000_u64,"fee_units":48_000_000,"fee_basis_points":240}),
                    )
                } else {
                    (
                        "404 Not Found",
                        String::new(),
                        json!({"error":"unexpected endpoint"}),
                    )
                };
                let body = body.to_string();
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
            account,
            stop,
            worker: Some(worker),
        }
    }
    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
    fn paid(&self) -> usize {
        self.requests()
            .iter()
            .filter(|row| row.headers.to_ascii_lowercase().contains("payment "))
            .count()
    }
    fn quotes(&self) -> usize {
        self.requests()
            .iter()
            .filter(|row| {
                row.first.starts_with("POST ")
                    && !row.headers.to_ascii_lowercase().contains("payment ")
            })
            .count()
    }
    fn balance(&self, cents: Option<i64>) {
        self.account.lock().unwrap().available_cents = cents;
    }
}
impl Drop for MockTuara {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}

/// Tuara's mppx verifier declines a Stripe credential whose payload does not
/// echo the challenge request's externalId.
fn bound_to_challenge(headers: &str, external_id: &str) -> bool {
    headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if !name.eq_ignore_ascii_case("authorization") {
                return None;
            }
            let encoded = value.trim().strip_prefix("Payment ")?.split(',').next()?;
            serde_json::from_slice::<Value>(&URL_SAFE_NO_PAD.decode(encoded).ok()?).ok()
        })
        .is_some_and(|credential| credential["payload"]["externalId"] == external_id)
}

fn public(value: &Value) {
    for secret in [KEY, TOKEN, "4242"] {
        assert!(
            !value.to_string().contains(secret),
            "public topup report leaked private data"
        );
    }
}
fn call(db: &Store, args: Value) -> Value {
    let result = topup::dispatch(db, &args).unwrap();
    public(&result);
    result
}
fn action(db: &Store, action: &str) -> Value {
    call(db, json!({"action":action,"provider":PROVIDER}))
}
fn configure() -> Value {
    json!({"action":"configure","provider":PROVIDER,"threshold_cents":500,"amount_cents":2000,
        "max_charge_cents":2048,"monthly_limit_cents":2048,"terms_version":"2026-09","accept_terms":true})
}
fn wallet_log() -> String {
    std::fs::read_to_string(PathBuf::from(std::env::var("HOME").unwrap()).join("wallet.log"))
        .unwrap_or_default()
}
fn check_safely(db: &Store) {
    match topup::dispatch(db, &json!({"action":"check","provider":PROVIDER})) {
        Ok(value) => public(&value),
        Err(error) => public(&json!({"error":error.to_string()})),
    }
}
fn advance_to_wallet(db: &Store) {
    for _ in 0..5 {
        action(db, "check");
        if !wallet_log().is_empty() {
            return;
        }
    }
    panic!("no wallet authorization created");
}
fn advance_to_paid(db: &Store, server: &MockTuara) {
    for _ in 0..6 {
        action(db, "check");
        if server.paid() > 0 {
            return;
        }
    }
    panic!("approved low-balance policy did not submit a payment");
}
fn settle(db: &Store) -> Value {
    for _ in 0..4 {
        let report = action(db, "check");
        if report["status"] == "watching"
            && report["pending"].is_null()
            && report["spent_monthly_cents"] == 2048
        {
            return report;
        }
    }
    panic!("saved successful payment was not accounted for");
}

fn fixture(scenario: &str) {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let bin = directory.path().join("bin");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&bin).unwrap();
    if scenario == "cancel_fails" {
        std::fs::write(home.join("cancel_fail"), "1").unwrap();
    }
    let script = bin.join("link-cli");
    std::fs::write(&script, format!(r##"#!/bin/sh
printf '%s\n' "$*" >> "$HOME/wallet.log"
case " $* " in
  *' spend-request create '*) printf '%s' '[{{"id":"lsrq_topup","amount":2048,"currency":"usd","network_id":"profile_test","credential_type":"shared_payment_token","status":"pending_approval","approval_url":"https://app.link.com/approve/lsrq_topup"}}]' ;;
  *' spend-request retrieve '*)
    if [ -f "$HOME/pending" ]; then
      printf '%s' '[{{"id":"lsrq_topup","amount":2048,"currency":"usd","network_id":"profile_test","credential_type":"shared_payment_token","status":"pending_approval","approval_url":"https://app.link.com/approve/lsrq_topup"}}]'
    else
      printf '%s' '[{{"id":"lsrq_topup","amount":2048,"currency":"usd","network_id":"profile_test","credential_type":"shared_payment_token","status":"approved","shared_payment_token":{{"id":"{TOKEN}"}}}}]'
    fi ;;
  *' spend-request cancel '*)
    [ -f "$HOME/cancel_fail" ] && exit 2
    printf '%s' '[{{"id":"lsrq_topup","amount":2048,"currency":"usd","network_id":"profile_test","credential_type":"shared_payment_token","status":"canceled"}}]' ;;
  *) exit 2 ;;
esac
"##)).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_provider_topup", "--nocapture"])
        .env("HORDE_TOPUP_SCENARIO", scenario)
        .env("HORDE_TOPUP_ROOT", directory.path().join("data"))
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", directory.path().join("config"))
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env_remove("HORDE_WORKER_TOKEN")
        .env_remove("TUARA_API_KEY")
        .env_remove("TOPUP_TEST_KEY")
        .env_remove("TOPUP_ALIAS_KEY")
        .env_remove("LINK_ACCESS_TOKEN")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "scenario {scenario}:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn policy_path() -> PathBuf {
    std::fs::read_dir(horde::branding::config_dir().join("provider-topups"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .unwrap()
}
fn edit_policy(edit: impl FnOnce(&mut Value)) {
    let path = policy_path();
    let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    edit(&mut value);
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}
fn previous_month() -> i64 {
    let now = time::OffsetDateTime::now_utc();
    time::Date::from_calendar_date(now.year(), now.month(), 1)
        .unwrap()
        .midnight()
        .assume_utc()
        .unix_timestamp()
        - 1
}
fn ledger_files() -> Vec<PathBuf> {
    let policy = policy_path();
    let directory = policy.parent().unwrap().join(format!(
        "{}-charges",
        policy.file_stem().unwrap().to_str().unwrap()
    ));
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect()
}

fn restart_check() -> Value {
    let result = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_topup_check", "--nocapture"])
        .env("HORDE_TOPUP_CHECK_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8(result.stdout).unwrap();
    let value: Value = serde_json::from_str(
        stdout
            .lines()
            .find_map(|line| line.strip_prefix("TOPUP_RESULT="))
            .unwrap(),
    )
    .unwrap();
    public(&value);
    value
}
#[test]
fn child_topup_check() {
    if std::env::var("HORDE_TOPUP_CHECK_CHILD").is_err() {
        return;
    }
    let db = Store::open(&PathBuf::from(std::env::var("HORDE_TOPUP_ROOT").unwrap())).unwrap();
    check_safely(&db);
    println!("TOPUP_RESULT={}", action(&db, "status"));
}

#[test]
fn child_provider_topup() {
    let Ok(scenario) = std::env::var("HORDE_TOPUP_SCENARIO") else {
        return;
    };
    let server = MockTuara::new(&scenario);
    let config_dir = horde::branding::config_dir();
    std::fs::create_dir_all(&config_dir).unwrap();
    let endpoint = format!("{}/router/v1", server.origin);
    let config = format!(
        "[providers.tuara-test]\nkind='tuara'\nauth_mode='api'\nbase_url='{endpoint}'\napi_key_env='TOPUP_TEST_KEY'\nmodel='chosen-model'\n[providers.tuara-alias]\nkind='tuara'\nauth_mode='api'\nbase_url='{endpoint}'\napi_key_env='TOPUP_ALIAS_KEY'\n[executors.worker]\nprovider='tuara-test'\n"
    );
    std::fs::write(config_dir.join("config.toml"), &config).unwrap();
    let credentials = format!("TOPUP_TEST_KEY='{KEY}'\nTOPUP_ALIAS_KEY='{KEY}'\n");
    let credentials_path = Settings::credentials_path();
    std::fs::write(&credentials_path, &credentials).unwrap();
    std::fs::set_permissions(&credentials_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let root = PathBuf::from(std::env::var("HORDE_TOPUP_ROOT").unwrap());
    let db = Store::open(&root).unwrap();
    let mut settings = configure();
    if scenario == "cap" {
        settings["max_charge_cents"] = json!(2047);
    }
    let initial = protocol::dispatch(&db, "provider_topup", settings.clone(), None).unwrap();
    public(&initial);
    assert_eq!(initial["enabled"], true);
    assert_eq!(
        server.quotes(),
        0,
        "configuration must not initiate payment"
    );
    assert_eq!(server.paid(), 0);
    assert!(wallet_log().is_empty());
    let request_count = server.requests().len();
    action(&db, "status");
    assert_eq!(
        server.requests().len(),
        request_count,
        "status must be read-only"
    );
    if scenario == "high_balance" {
        for cents in [10000, 500] {
            server.balance(Some(cents));
            action(&db, "check");
        }
        assert_eq!(
            server.quotes(),
            0,
            "balance at or above threshold must not request payment"
        );
        assert!(wallet_log().is_empty());
        return;
    }
    if scenario == "background_tick" {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for _ in 0..6 {
            edit_policy(|policy| policy["next_check_at"] = json!(0));
            runtime.block_on(topup::tick(&root)).unwrap();
            let report = action(&db, "status");
            if report["spent_monthly_cents"] == 2048 && report["pending"].is_null() {
                assert_eq!(report["status"], "watching");
                assert_eq!(server.paid(), 1);
                server.balance(Some(100));
                let reads = server.requests().len();
                runtime.block_on(topup::tick(&root)).unwrap();
                assert_eq!(
                    server.requests().len(),
                    reads,
                    "background checks must honor the success cooldown"
                );
                assert!(report["next_check_at"].as_i64().unwrap() > horde::store::now());
                return;
            }
        }
        panic!("daemon tick did not complete enabled automatic funding");
    }
    if scenario == "cap" {
        for _ in 0..5 {
            check_safely(&db);
        }
        assert_eq!(server.paid(), 0);
        assert!(
            wallet_log().is_empty(),
            "over-cap quote must not reach wallet"
        );
        return;
    }
    if scenario == "alias" {
        advance_to_wallet(&db);
        let mut alias = configure();
        alias["provider"] = json!("tuara-alias");
        assert!(
            topup::dispatch(&db, &alias).is_err(),
            "aliases must not create separate spending policies for one organization"
        );
        assert_eq!(server.paid(), 0);
        return;
    }
    if scenario == "unknown_balance" {
        server.balance(None);
        for _ in 0..3 {
            check_safely(&db);
        }
        assert_eq!(server.quotes(), 0);
        assert!(wallet_log().is_empty());
        return;
    }
    if scenario == "credential_drift" {
        std::fs::write(
            &credentials_path,
            credentials.replace(KEY, "newer-fixture-key"),
        )
        .unwrap();
        for _ in 0..3 {
            check_safely(&db);
        }
        assert_eq!(server.quotes(), 0);
        assert!(wallet_log().is_empty());
        return;
    }
    if scenario == "pending_disable" {
        std::fs::write(
            PathBuf::from(std::env::var("HOME").unwrap()).join("pending"),
            "pending",
        )
        .unwrap();
        advance_to_wallet(&db);
        action(&db, "check");
        assert_eq!(server.paid(), 0);
        assert_eq!(action(&db, "disable")["enabled"], false);
        assert_eq!(
            wallet_log()
                .lines()
                .filter(|line| line.contains("spend-request cancel"))
                .count(),
            1,
            "disabling must revoke the unpaid Link authorization"
        );
        std::fs::remove_file(PathBuf::from(std::env::var("HOME").unwrap()).join("pending"))
            .unwrap();
        for _ in 0..3 {
            check_safely(&db);
        }
        assert_eq!(
            server.paid(),
            0,
            "disabled authorization must not charge after wallet approval"
        );
        return;
    }
    if scenario == "balance_recovers" || scenario == "cancel_fails" {
        advance_to_wallet(&db);
        server.balance(Some(10000));
        for _ in 0..3 {
            check_safely(&db);
        }
        assert_eq!(
            server.paid(),
            0,
            "balance must be rechecked after wallet approval"
        );
        if scenario == "cancel_fails" {
            let report = action(&db, "status");
            assert_eq!(report["status"], "needs_attention");
            assert!(
                !report["pending"].is_null(),
                "an unrevoked Link authorization must remain recorded"
            );
            return;
        }
        assert_eq!(
            wallet_log()
                .lines()
                .filter(|line| line.contains("spend-request cancel"))
                .count(),
            1,
            "a recovered balance must revoke the unused approved authorization"
        );
        return;
    }
    advance_to_paid(&db, &server);
    assert_eq!(server.paid(), 1);
    if scenario.starts_with("uncertain") {
        assert_eq!(action(&db, "status")["status"], "uncertain");
        if scenario == "uncertain_rollover" {
            edit_policy(|policy| policy["pending"]["submitted_at"] = json!(previous_month()));
            assert_eq!(action(&db, "status")["spent_monthly_cents"], 0);
        }
        assert_eq!(restart_check()["status"], "uncertain");
        for (field, value) in [
            ("threshold_cents", json!(600)),
            ("provider", json!("tuara-alias")),
        ] {
            let mut changed = settings.clone();
            changed[field] = value;
            assert!(
                topup::dispatch(&db, &changed).is_err(),
                "unresolved payment must block changed configuration and aliases"
            );
        }
        let result = topup::dispatch(&db, &settings);
        if let Ok(value) = result {
            public(&value);
            assert_eq!(value["status"], "uncertain");
        }
        for _ in 0..3 {
            check_safely(&db);
        }
        assert_eq!(
            server.paid(),
            1,
            "ambiguous payment must not be repeated after restart or reconfiguration"
        );
        assert_eq!(server.quotes(), 1);
        return;
    }
    if scenario == "ledger_before_clear" {
        let policy: Value = serde_json::from_slice(&std::fs::read(policy_path()).unwrap()).unwrap();
        let pending = &policy["pending"];
        let directory = config_dir
            .join("provider-topups")
            .join(format!("{}-charges", policy["id"].as_str().unwrap()));
        let path = directory.join(format!("{}.json", pending["id"].as_str().unwrap()));
        let mut file = tempfile::NamedTempFile::new_in(&directory).unwrap();
        serde_json::to_writer(
            file.as_file_mut(),
            &json!({"id":pending["id"],"submitted_at":pending["submitted_at"],"cents":2048}),
        )
        .unwrap();
        file.persist(path).unwrap();
        assert_eq!(
            action(&db, "status")["spent_monthly_cents"],
            2048,
            "pending and ledger must deduplicate the same charge"
        );
    } else {
        // Simulate a crash after response fsync and before the receipt transition.
        edit_policy(|policy| {
            policy["pending"]["phase"] = json!("submitting");
            policy["status"] = json!("submitting");
        });
    }
    // Recover and account for a saved successful response from a fresh OS process.
    restart_check();
    let completed = settle(&db);
    assert_eq!(completed["spent_monthly_cents"], 2048);
    assert_eq!(completed["remaining_monthly_cents"], 0);
    assert!(!completed["last_payment"].is_null());
    assert_eq!(
        ledger_files().len(),
        1,
        "completed charges must have durable ledger entries"
    );
    if scenario == "month_rollover" {
        let ledger = ledger_files().pop().unwrap();
        let mut charge: Value = serde_json::from_slice(&std::fs::read(&ledger).unwrap()).unwrap();
        charge["submitted_at"] = json!(previous_month());
        std::fs::write(&ledger, serde_json::to_vec(&charge).unwrap()).unwrap();
        assert_eq!(action(&db, "status")["spent_monthly_cents"], 0);
        assert_eq!(action(&db, "status")["remaining_monthly_cents"], 2048);
        assert!(
            ledger.exists(),
            "a new month must not delete past charge evidence"
        );
    }
    server.balance(Some(100));
    if scenario == "target_rebind" {
        std::fs::write(
            config_dir.join("config.toml"),
            config.replace("TOPUP_TEST_KEY", "TOPUP_REBOUND_KEY"),
        )
        .unwrap();
        std::fs::write(
            &credentials_path,
            format!("{credentials}TOPUP_REBOUND_KEY='{KEY}'\n"),
        )
        .unwrap();
        let rebound = call(&db, settings);
        assert_eq!(rebound["spent_monthly_cents"], 2048);
        assert_eq!(
            action(&db, "check")["status"],
            "budget_exhausted",
            "same key under a new credential setting must refresh the pinned target"
        );
        assert_eq!(server.paid(), 1);
        return;
    }
    if scenario == "month_rollover" {
        for _ in 0..5 {
            action(&db, "check");
            if server.paid() == 2 {
                break;
            }
        }
        assert_eq!(
            server.paid(),
            2,
            "a resolved prior-month charge should not consume the new monthly budget"
        );
        settle(&db);
        assert_eq!(ledger_files().len(), 2);
        return;
    }
    let ledger_before = ledger_files()
        .into_iter()
        .map(|path| (path.clone(), std::fs::read(path).unwrap()))
        .collect::<Vec<_>>();
    action(&db, "disable");
    let reconfigured = call(&db, settings);
    assert_eq!(
        reconfigured["spent_monthly_cents"], 2048,
        "reconfiguration cannot reset the monthly ledger"
    );
    for (path, bytes) in ledger_before {
        assert_eq!(
            std::fs::read(path).unwrap(),
            bytes,
            "reconfiguring must preserve durable charge history"
        );
    }
    for _ in 0..5 {
        check_safely(&db);
    }
    assert_eq!(
        server.paid(),
        1,
        "monthly budget includes fees and survives disable/reconfigure"
    );
    let requests = server.requests();
    let paid = requests
        .iter()
        .find(|row| row.headers.to_ascii_lowercase().contains("payment "))
        .unwrap();
    let quote = requests
        .iter()
        .find(|row| {
            row.first.starts_with("POST ") && !row.headers.to_ascii_lowercase().contains("payment ")
        })
        .unwrap();
    assert_eq!(
        paid.body, quote.body,
        "paid retry must preserve exactly the quoted request"
    );
    assert!(
        paid.headers
            .to_ascii_lowercase()
            .contains(&format!("bearer {KEY}"))
    );
    assert_eq!(
        wallet_log()
            .lines()
            .filter(|line| line.contains("spend-request create"))
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_to_string(credentials_path).unwrap(),
        credentials,
        "topup must not replace credentials"
    );
    assert_eq!(
        std::fs::read_to_string(config_dir.join("config.toml")).unwrap(),
        config
    );
}

#[test]
fn topup_above_or_at_threshold_does_not_request_payment() {
    fixture("high_balance");
}
#[test]
fn topup_daemon_tick_advances_an_enabled_policy_without_explicit_check_actions() {
    fixture("background_tick");
}
#[test]
fn topup_approved_low_balance_is_accounted_once_and_preserves_monthly_budget() {
    fixture("success");
}
#[test]
fn topup_new_month_releases_resolved_budget_but_holds_uncertain_payments() {
    fixture("month_rollover");
    fixture("uncertain_rollover");
}
#[test]
fn topup_reconfiguration_with_same_key_refreshes_target_without_resetting_spend() {
    fixture("target_rebind");
}
#[test]
fn topup_recovers_ledger_written_before_pending_clear_without_double_counting() {
    fixture("ledger_before_clear");
}
#[test]
fn topup_charge_cap_is_enforced_before_wallet_authorization() {
    fixture("cap");
}
#[test]
fn topup_pending_approval_cannot_charge_after_policy_disable() {
    fixture("pending_disable");
}
#[test]
fn topup_rechecks_balance_before_using_approved_wallet_token() {
    fixture("balance_recovers");
}
#[test]
fn topup_keeps_an_unrevoked_wallet_authorization_for_reconciliation() {
    fixture("cancel_fails");
}
#[test]
fn topup_uncertain_charge_cannot_repeat_after_restart_or_reconfigure() {
    fixture("uncertain");
}
#[test]
fn topup_blocks_duplicate_organization_through_another_provider_alias() {
    fixture("alias");
}
#[test]
fn topup_holds_when_balance_or_credentials_are_unknown() {
    fixture("unknown_balance");
    fixture("credential_drift");
}
#[test]
fn topup_is_unavailable_to_workers_and_project_bound_credentials() {
    let directory = tempfile::tempdir().unwrap();
    let db = Store::open(directory.path()).unwrap();
    assert!(!protocol::worker_allowed("provider_topup"));
    assert!(!protocol::project_allowed("provider_topup"));
    let args = json!({"action":"status","provider":PROVIDER});
    assert!(
        protocol::dispatch_scoped(&db, "provider_topup", args.clone(), None, Some("default"))
            .is_err()
    );
    assert!(protocol::dispatch(&db, "provider_topup", args, Some("invalid-worker-token")).is_err());
}
