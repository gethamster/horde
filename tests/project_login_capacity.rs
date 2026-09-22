use horde::{accounts, capacity, config::ExecutorConfig, protocol, store::Store};
use serde_json::json;

fn managed(db: &Store, name: &str) -> ExecutorConfig {
    let id=accounts::dispatch(db,"account_create", &json!({"project":"default","name":name,"provider":"codex","auth_mode":"api","base_url":"https://fixture.invalid/v1","concurrency":2})).unwrap().unwrap()["id"].as_str().unwrap().to_owned();
    let config = ExecutorConfig {
        account: Some(id),
        project: Some("default".into()),
        kind: "codex".into(),
        auth_mode: "api".into(),
        api_key_env: "SHARED_METADATA_ONLY".into(),
        base_url: "https://fixture.invalid/v1".into(),
        ..Default::default()
    };
    rotate(db, &config);
    config
}
fn rotate(db: &Store, config: &ExecutorConfig) {
    accounts::set_credential(
        db,
        "default",
        config.account.as_deref().unwrap(),
        &accounts::Credential {
            kind: "api_key".into(),
            secret: uuid::Uuid::new_v4().to_string(),
            expires_at: None,
            metadata: json!({}),
        },
    )
    .unwrap();
}
fn observe(db: &Store, config: &ExecutorConfig) {
    for source in ["provider", "local_budget"] {
        capacity::observe(
            db,
            &capacity::Snapshot {
                account: capacity::account(config),
                provider: "codex".into(),
                window: if source == "provider" {
                    "primary".into()
                } else {
                    source.into()
                },
                used_percent: Some(if source == "provider" { 100.0 } else { 25.0 }),
                reset_at: None,
                observed_at: horde::store::now(),
                source: source.into(),
            },
        )
        .unwrap();
    }
}
fn rows(db: &Store, config: &ExecutorConfig) -> Vec<serde_json::Value> {
    db.rows(
        "SELECT source FROM account_capacity WHERE account=? ORDER BY source",
        &[&capacity::account(config)],
    )
    .unwrap()
}
#[test]
fn managed_rotation_fences_old_observations_without_resetting_account_quota() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let first = managed(&db, "first");
    let other = managed(&db, "other");
    observe(&db, &first);
    observe(&db, &other);
    let old = capacity::credential_generation(&db, &first).unwrap();
    let other_generation = capacity::credential_generation(&db, &other).unwrap();
    rotate(&db, &first);
    let fresh = capacity::credential_generation(&db, &first).unwrap();
    assert_ne!(
        old, fresh,
        "a managed credential version must invalidate prior observations"
    );
    assert_eq!(
        other_generation,
        capacity::credential_generation(&db, &other).unwrap()
    );
    assert_eq!(
        rows(&db, &first).len(),
        2,
        "credential renewal does not reset account quota"
    );
    assert!(!capacity::available(&db, &capacity::account(&first)).unwrap());
    assert_eq!(rows(&db, &other).len(), 2);
    let event = json!({"rate_limits":{"primary":{"used_percent":0.0,"resets_at":9999999999_i64}}});
    capacity::ingest_current(&db, &first, old.as_deref(), &event).unwrap();
    assert_eq!(rows(&db, &first).len(), 2);
    capacity::ingest_current(&db, &first, fresh.as_deref(), &event).unwrap();
    assert_eq!(rows(&db, &first).len(), 2);
    assert!(
        capacity::available(&db, &capacity::account(&first)).unwrap(),
        "a fresh authoritative quota report clears exhaustion"
    );
}
#[test]
fn legacy_login_rotation_does_not_change_managed_account_quota() {
    let dir = tempfile::tempdir().unwrap();
    // An unused configuration directory keeps the developer's own settings out.
    let db = Store::open_with_config_dir(dir.path(), &dir.path().join("config")).unwrap();
    let account = managed(&db, "private");
    observe(&db, &account);
    // A pinned default-project task may explicitly select a managed account.
    let mut settings = horde::config::Settings::default();
    settings.providers.insert(
        "managed".into(),
        horde::config::Provider {
            kind: account.kind.clone(),
            auth_mode: account.auth_mode.clone(),
            base_url: account.base_url.clone(),
            api_key_env: account.api_key_env.clone(),
            account: account.account.clone(),
            ..Default::default()
        },
    );
    db.conn
        .execute(
            "INSERT INTO tasks VALUES('pinned','test','/fixture','queued',?,'{}',0)",
            [serde_json::to_string(&settings).unwrap()],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO task_projects VALUES('pinned','default',NULL)",
            [],
        )
        .unwrap();
    let legacy = ExecutorConfig {
        account: None,
        project: None,
        ..account.clone()
    };
    capacity::credentials_changed(&db, &legacy).unwrap();
    assert_eq!(
        rows(&db, &account).len(),
        2,
        "ambient key replacement cannot clear managed quota sharing the env-var label"
    );
}
#[test]
fn project_bound_login_is_hidden_and_rejected_before_login_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    assert!(!protocol::project_allowed("provider_login"));
    assert!(!protocol::worker_allowed("provider_login"));
    let error = protocol::dispatch_scoped(
        &db,
        "provider_login",
        json!({"action":"status","session_id":"unknown"}),
        None,
        Some("default"),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("project-bound credentials"), "{error}");
}
#[test]
fn administrator_login_reaches_strict_request_parser_without_synthetic_project() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let error = protocol::dispatch(
        &db,
        "provider_login",
        json!({"action":"status","session_id":"unknown"}),
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("login session unavailable"), "{error}");
}
#[test]
fn explicit_nondefault_project_cannot_use_host_wide_login_or_setup() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let project = protocol::dispatch(&db, "project_create", json!({"name":"hamster"}), None)
        .unwrap()["id"]
        .clone();
    for (name, args) in [
        (
            "provider_login",
            json!({"project":project,"action":"status","session_id":"unknown"}),
        ),
        ("agent_setup", json!({"project":project,"action":"inspect"})),
    ] {
        let error = protocol::dispatch(&db, name, args, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("default project"), "{name}: {error}");
    }
}

#[test]
fn invocation_generation_requires_exact_persisted_account_and_version() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let config = managed(&db, "selected");
    let other = managed(&db, "other");
    db.conn
        .execute(
            "INSERT INTO tasks VALUES('launch','test','/fixture','running','{}','{}',0)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO task_projects VALUES('launch','default',NULL)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO steps VALUES('step','launch','work','{}','running',NULL)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO attempts(id,step,state,started) VALUES('attempt','step','running',0)",
            [],
        )
        .unwrap();
    assert!(capacity::invocation_generation(&db, "attempt", &config).is_err());
    db.conn.execute("INSERT INTO attempt_bindings SELECT 'attempt','default','local',account,id,credential_version,'native' FROM auth_profiles WHERE account=?",[config.account.as_deref().unwrap()]).unwrap();
    let generation = capacity::invocation_generation(&db, "attempt", &config).unwrap();
    assert_eq!(
        generation,
        capacity::credential_generation(&db, &config).unwrap()
    );
    assert!(capacity::invocation_generation(&db, "attempt", &other).is_err());
    rotate(&db, &config);
    assert!(
        capacity::invocation_generation(&db, "attempt", &config)
            .unwrap_err()
            .to_string()
            .contains("binding")
    );
}

#[test]
fn remote_credential_replacement_preserves_quota_and_fences_previous_version() {
    let controller_dir = tempfile::tempdir().unwrap();
    let controller = Store::open(controller_dir.path()).unwrap();
    let worker_dir = tempfile::tempdir().unwrap();
    let worker = Store::open(worker_dir.path()).unwrap();
    let config = managed(&controller, "shared");
    let account = config.account.as_deref().unwrap();
    let initial = accounts::provision(&controller, "default", account, "local", "first").unwrap();
    accounts::receive(&worker, &initial).unwrap();
    observe(&worker, &config);
    let old = capacity::credential_generation(&worker, &config).unwrap();
    rotate(&controller, &config);
    let next = accounts::provision(&controller, "default", account, "local", "next").unwrap();
    accounts::receive(&worker, &next).unwrap();
    assert_ne!(
        old,
        capacity::credential_generation(&worker, &config).unwrap()
    );
    assert!(!capacity::available(&worker, account).unwrap());
    let event = json!({"rate_limits":{"primary":{"used_percent":0.0,"resets_at":9999999999_i64}}});
    capacity::ingest_current(&worker, &config, old.as_deref(), &event).unwrap();
    assert!(
        !capacity::available(&worker, account).unwrap(),
        "late old-profile report cannot clear quota"
    );
    let current = capacity::credential_generation(&worker, &config).unwrap();
    capacity::ingest_current(&worker, &config, current.as_deref(), &event).unwrap();
    assert!(capacity::available(&worker, account).unwrap());
    accounts::receive(&worker, &next).unwrap();
    assert!(
        capacity::available(&worker, account).unwrap(),
        "delivery retries preserve the fresh observation"
    );
}

#[tokio::test]
async fn api_broker_uses_captured_generation_after_credential_rotation() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let config = managed(&db, "api");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = ExecutorConfig {
        base_url: format!("http://{}/v1", listener.local_addr().unwrap()),
        ..config
    };
    let old = capacity::credential_generation(&db, &config).unwrap();
    let broker = horde::credentials::Broker::with_key_observed(
        &config,
        "scoped-fixture",
        "old-fixture-key".into(),
        5,
        &db.root,
        old,
    )
    .await
    .unwrap();
    rotate(&db, &config);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 8192];
        let mut request = Vec::new();
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut buffer).await.unwrap();
            assert!(n > 0);
            request.extend_from_slice(&buffer[..n]);
        }
        assert!(String::from_utf8_lossy(&request).contains("Bearer old-fixture-key"));
        stream.write_all(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 2\r\nRetry-After: 3600\r\nConnection: close\r\n\r\n{}").await.unwrap();
    });
    let response = reqwest::Client::new()
        .post(format!("{}/responses", broker.base_url))
        .bearer_auth("scoped-fixture")
        .json(&json!({"model":"fixture","input":"test"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    server.await.unwrap();
    assert!(
        rows(&db, &config).is_empty(),
        "old-key API responses cannot poison the renewed profile's quota observations"
    );
}

fn managed_pin_fixture(action: &str) {
    let dir = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_managed_pin_preflight", "--nocapture"])
        .env("HORDE_MANAGED_PIN_ACTION", action)
        .env("HORDE_MANAGED_PIN_ROOT", dir.path().join("data"))
        .env("HOME", dir.path().join("home"))
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env_remove("HORDE_WORKER_TOKEN")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "managed account authentication preflight failed"
    );
}
#[test]
fn shared_login_rejects_managed_provider_before_starting_a_session() {
    managed_pin_fixture("login");
}
#[test]
fn global_key_setup_rejects_managed_provider_before_writing_config() {
    managed_pin_fixture("configure");
}
#[tokio::test]
async fn child_managed_pin_preflight() {
    let Ok(action) = std::env::var("HORDE_MANAGED_PIN_ACTION") else {
        return;
    };
    let root = std::path::PathBuf::from(std::env::var("HORDE_MANAGED_PIN_ROOT").unwrap());
    let db = Store::open(&root).unwrap();
    let (provider, mode, url) = if action == "login" {
        ("codex", "login", "")
    } else {
        ("tuara", "api", "https://tuara.com/router/v1")
    };
    let account=accounts::dispatch(&db,"account_create",&json!({"project":"default","name":"managed","provider":provider,"auth_mode":mode,"base_url":url})).unwrap().unwrap()["id"].as_str().unwrap().to_owned();
    let config_dir = horde::branding::config_dir();
    std::fs::create_dir_all(&config_dir).unwrap();
    let configured = if action.ends_with("-alias") {
        "default"
    } else {
        "pinned"
    };
    let mut text = format!(
        "[providers.{configured}]\nkind='{provider}'\nauth_mode='{mode}'\naccount='{account}'\nbase_url='{url}'\napi_key_env='PINNED_FIXTURE_KEY'\nprogram='/usr/bin/false'\n"
    );
    if action == "configure-legacy" {
        text.push_str("[providers.zzz-legacy]\nkind='tuara'\nauth_mode='api'\nbase_url='https://tuara.com/router/v1'\napi_key_env='PINNED_FIXTURE_KEY'\n");
    }
    let path = config_dir.join("config.toml");
    std::fs::write(&path, &text).unwrap();
    if action == "configure-legacy" {
        let result = horde::agent_setup::run(&root, &json!({"action":"configure_provider","provider":"zzz-legacy","credential":"new-legacy-fixture-key"})).await.unwrap();
        assert_eq!(result["status"], "configured");
        assert_eq!(
            horde::config::Settings::load_user()
                .unwrap()
                .provider("pinned")
                .unwrap()
                .account
                .as_deref(),
            Some(account.as_str())
        );
        return;
    }
    let selected = if action.ends_with("-alias") {
        "tuara"
    } else {
        "pinned"
    };
    let error = if action.starts_with("login") {
        horde::provider_login::dispatch(
            &db,
            &json!({"action":"start","provider":selected,"request_id":"managed-pin"}),
        )
        .unwrap_err()
        .to_string()
    } else {
        horde::agent_setup::run(&root,&json!({"action":"configure_provider","provider":selected,"credential":"fixture-key-no-write"})).await.unwrap_err().to_string()
    };
    assert!(
        error.contains("managed account"),
        "expected a managed account rejection"
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), text);
    assert!(!horde::config::Settings::credentials_path().exists());
    assert!(!config_dir.join("credential-overrides.json").exists());
    assert!(!root.join("provider-logins").exists());
}

#[test]
fn preset_alias_cannot_bypass_managed_provider_preflight() {
    managed_pin_fixture("configure-alias");
    managed_pin_fixture("login-alias");
}
#[test]
fn legacy_key_setup_selects_its_provider_when_managed_metadata_shares_env_name() {
    managed_pin_fixture("configure-legacy");
}
