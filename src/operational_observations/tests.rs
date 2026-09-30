use super::*;
use serde_json::json;

fn fixture() -> (tempfile::TempDir, Store, Value) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let db = Store::open(root.path()).unwrap();
    let token = root.path().join("observations-token");
    std::fs::write(&token, "private-telemetry-token-at-least-32-chars").unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = json!({"schema_version":1,"project_id":"default","enabled":true,"tenant_id":"operator","telemetry_project_id":"ops","diagnostic_thread_id":"diagnostic-thread","endpoint":"http://signals:8080/v1/observations","token_file":token,"start_cursor":0});
    db.conn.execute("INSERT INTO tasks VALUES('run-1','DO NOT EXPORT OBJECTIVE','/private/path','failed','{}','{}',0)", []).unwrap();
    db.conn
        .execute("INSERT INTO revisions VALUES('run-1',0,'{}',0)", [])
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO task_projects VALUES('run-1','default',NULL)",
            [],
        )
        .unwrap();
    crate::run::bind_run_context(&db, "run-1", Some("thread-1"), Some("brief-1")).unwrap();
    (root, db, config)
}

#[test]
fn redaction_and_association_do_not_depend_on_runs_service() {
    let (_root, db, config) = fixture();
    setup(&db, &config).unwrap();
    db.event(
        "run-1",
        "step.finished",
        json!({"state":"failed","result":{"error":"PRIVATE TOKEN AND TRANSCRIPT"}}),
    )
    .unwrap();
    capture(&db).unwrap();
    let rows = db
        .rows(
            "SELECT payload,key FROM operational_observation_outbox",
            &[],
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    let payload: Value = serde_json::from_str(rows[0]["payload"].as_str().unwrap()).unwrap();
    assert_eq!(payload["project_id"], "ops");
    assert_eq!(payload["thread_id"], "diagnostic-thread");
    let message: Value = serde_json::from_str(payload["message"].as_str().unwrap()).unwrap();
    assert_eq!(message["thread_id"], "thread-1");
    assert_eq!(message["brief_id"], "brief-1");
    assert_eq!(message["reason"], "attempt_failed");
    assert!(!payload.to_string().contains("PRIVATE"));
    assert!(
        !status(&db, Some("default"))
            .unwrap()
            .to_string()
            .contains("token")
    );
}

#[test]
fn rejected_endpoint_cannot_redirect_private_credentials() {
    let (_root, db, config) = fixture();
    for endpoint in [
        "http://evil/v1/observations",
        "https://user:pass@host/v1/observations",
        "http://signals:8080/other",
        "http://signals:8080/v1/observations?secret=yes",
    ] {
        let mut changed = config.clone();
        changed["endpoint"] = json!(endpoint);
        assert!(setup(&db, &changed).is_err());
    }
}

#[test]
fn disabled_config_does_not_scan_or_dispatch() {
    let (_root, db, mut config) = fixture();
    config["enabled"] = json!(false);
    setup(&db, &config).unwrap();
    db.event("run-1", "attempt.interrupted", json!({"token":"private"}))
        .unwrap();
    capture(&db).unwrap();
    assert_eq!(status(&db, Some("default")).unwrap()["pending"], 0);
}

fn fail(db: &Store) {
    db.event("run-1","step.finished",json!({"state":"failed","result":{"error":"access token renewal requires the controller refresh owner TOKEN"}})).unwrap();
}
fn messages(db: &Store) -> Vec<Value> {
    db.rows("SELECT payload FROM operational_observation_outbox WHERE payload IS NOT NULL ORDER BY event_seq",&[]).unwrap().iter().map(|r|serde_json::from_str::<Value>(r["payload"].as_str().unwrap()).unwrap()).collect()
}

#[test]
fn captures_only_fixed_categories_and_authoritative_codes() {
    let (_root, db, config) = fixture();
    setup(&db, &config).unwrap();
    for (kind, data) in [
        ("step.finished", json!({"state":"succeeded"})),
        (
            "step.finished",
            json!({"state":"failed","failure_code":"credential_refresh_required"}),
        ),
        ("attempt.interrupted", json!({"raw":"SECRET"})),
        ("run.preview_held", json!({"error":"SECRET"})),
        (
            "run.preview_phase",
            json!({"phase":"held","error":"SECRET"}),
        ),
        ("run.preview_interrupted", json!({"id":"job"})),
        ("run.preview_phase", json!({"phase":"succeeded"})),
    ] {
        db.event("run-1", kind, data).unwrap();
    }
    capture(&db).unwrap();
    assert_eq!(messages(&db).len(), 5);
    assert_eq!(
        serde_json::from_str::<Value>(messages(&db)[0]["message"].as_str().unwrap()).unwrap()["reason"],
        "credential_refresh_required"
    );
    assert!(!json!(messages(&db)).to_string().contains("SECRET"));
    capture(&db).unwrap();
    assert_eq!(messages(&db).len(), 5);
}

#[test]
fn durable_cursor_identity_and_explicit_retry_survive_restart() {
    let (root, db, config) = fixture();
    setup(&db, &config).unwrap();
    fail(&db);
    capture(&db).unwrap();
    let original = messages(&db);
    let first = status(&db, Some("default")).unwrap();
    let key: String = db
        .conn
        .query_row("SELECT key FROM operational_observation_outbox", [], |r| {
            r.get(0)
        })
        .unwrap();
    finish(&db, &key, transport::Outcome::Held("idempotency_conflict")).unwrap();
    drop(db);
    let db = Store::open(root.path()).unwrap();
    capture(&db).unwrap();
    assert_eq!(messages(&db), original);
    let mut changed = config.clone();
    changed["start_cursor"] = json!(9999);
    changed["retry_held"] = json!(true);
    setup(&db, &changed).unwrap();
    assert_eq!(
        status(&db, Some("default")).unwrap()["projects"],
        first["projects"]
    );
    assert_eq!(status(&db, Some("default")).unwrap()["pending"], 1);
    changed["telemetry_project_id"] = json!("other");
    assert!(setup(&db, &changed).is_err());
    finish(&db, &key, transport::Outcome::Retry("transport_uncertain")).unwrap();
    finish(&db, &key, transport::Outcome::Delivered).unwrap();
    assert_eq!(status(&db, None).unwrap()["delivered"], 1);
}

#[test]
fn malformed_context_is_quarantined_without_losing_cursor_or_leaking() {
    let (_root, db, config) = fixture();
    setup(&db, &config).unwrap();
    // Legacy binding permits characters unsuitable for structural incident correlation.
    db.conn
        .execute(
            "INSERT INTO tasks VALUES('run-2','PRIVATE OBJECTIVE','/path','failed','{}','{}',0)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO task_projects VALUES('run-2','default',NULL)",
            [],
        )
        .unwrap();
    crate::run::bind_run_context(&db, "run-2", Some("PRIVATE CONTACT SPACE"), Some("brief-2"))
        .unwrap();
    db.event("run-2", "attempt.interrupted", json!({})).unwrap();
    capture(&db).unwrap();
    let state = status(&db, None).unwrap();
    assert_eq!(state["held"], 1);
    assert_eq!(state["holds"][0]["error_code"], "invalid_correlation");
    assert!(!state.to_string().contains("PRIVATE"));
    let mut retry = config.clone();
    retry["retry_held"] = json!(true);
    setup(&db, &retry).unwrap();
    assert_eq!(status(&db, None).unwrap()["held"], 1);
}

#[test]
fn scan_is_bounded_and_backpressure_preserves_uncaptured_events() {
    let (_root, db, config) = fixture();
    setup(&db, &config).unwrap();
    for _ in 0..1100 {
        fail(&db);
    }
    capture(&db).unwrap();
    assert_eq!(messages(&db).len(), 99); // Run binding occupies first journal record.
    for _ in 0..10 {
        capture(&db).unwrap();
    }
    let before = status(&db, None).unwrap();
    assert_eq!(before["pending"], 1000);
    capture(&db).unwrap();
    assert_eq!(status(&db, None).unwrap(), before);
}

#[test]
fn no_configuration_is_a_read_only_noop_and_setup_validation_is_strict() {
    let (_root, db, config) = fixture();
    fail(&db);
    capture(&db).unwrap();
    assert_eq!(status(&db, None).unwrap()["configured"], false);
    for (field, value) in [
        ("schema_version", json!(2)),
        ("start_cursor", json!(-1)),
        ("project_id", json!("missing")),
        ("tenant_id", json!("contact email")),
        ("token_file", json!("../secret")),
        ("unknown", json!(true)),
    ] {
        let mut changed = config.clone();
        changed[field] = value;
        assert!(setup(&db, &changed).is_err());
    }
    let mut https = config.clone();
    https["endpoint"] = json!("https://trusted.example/v1/observations");
    setup(&db, &https).unwrap();
    for value in [".ops", ":thread", "-tenant", "_scope"] {
        assert!(!valid_id(value));
        let mut changed = config.clone();
        changed["diagnostic_thread_id"] = json!(value);
        assert!(setup(&db, &changed).is_err());
    }
}

#[test]
fn credential_files_reject_unsafe_modes_symlinks_and_invalid_values() {
    use std::os::unix::fs::PermissionsExt;
    let (root, _db, config) = fixture();
    let path = PathBuf::from(config["token_file"].as_str().unwrap());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(transport::token(&path).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    for value in [
        "short".to_owned(),
        "x".repeat(4097),
        "x".repeat(32) + "\nother",
    ] {
        std::fs::write(&path, value).unwrap();
        assert!(transport::token(&path).is_err());
    }
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(transport::token(&link).is_err());
    assert!(transport::token(root.path()).is_err());
    assert!(transport::token(&root.path().join("missing")).is_err());
}

#[test]
fn diagnostics_are_scoped_read_only_and_classify_historical_refresh_owner() {
    let (_root, db, _config) = fixture();
    db.conn
        .execute(
            "INSERT INTO steps VALUES('step-1','run-1','implement','{}','failed',NULL)",
            [],
        )
        .unwrap();
    db.conn.execute("INSERT INTO attempts VALUES('attempt-1','step-1',NULL,'failed',1,2,NULL,?,NULL)",[json!({"error":"access token renewal requires the controller refresh owner SECRET"}).to_string()]).unwrap();
    let before = db.conn.total_changes();
    let value = diagnostics(&db, "default", "run-1", 0, 1).unwrap();
    assert_eq!(value["attempts"][0]["failure_code"], "attempt_failed");
    assert_eq!(
        value["attempts"][0]["historical_hint"],
        "suspected_credential_refresh_required"
    );
    assert_eq!(value["events"].as_array().unwrap().len(), 1);
    assert!(!value.to_string().contains("SECRET"));
    assert!(!value.to_string().contains("OBJECTIVE"));
    assert_eq!(db.conn.total_changes(), before);
    assert!(diagnostics(&db, "other", "run-1", 0, 50).is_err());
    assert!(diagnostics(&db, "default", "run-1", -1, 50).is_err());
    assert!(diagnostics(&db, "default", "run-1", 0, 51).is_err());
    let result = crate::protocol::dispatch(
        &db,
        "run_diagnostics",
        json!({"task":"run-1","limit":1}),
        None,
    )
    .unwrap();
    assert_eq!(result["project_id"], "default");
    let worker = db.register("run-1", None).unwrap();
    assert!(
        crate::protocol::dispatch(
            &db,
            "run_diagnostics",
            json!({"task":"run-1"}),
            worker["token"].as_str()
        )
        .is_ok()
    );
    assert!(
        crate::protocol::dispatch(
            &db,
            "run_diagnostics",
            json!({"task":"another"}),
            worker["token"].as_str()
        )
        .is_err()
    );
    assert!(!crate::protocol::worker_allowed("operational-observations"));
}

async fn mock_server(status: u16, body: String) -> (String, tokio::task::JoinHandle<String>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = vec![0; 65536];
        let size = stream.read(&mut bytes).await.unwrap();
        let request = String::from_utf8_lossy(&bytes[..size]).to_string();
        let response = format!(
            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        request
    });
    (format!("http://{address}/v1/observations"), task)
}

#[tokio::test]
async fn transport_validates_receipts_rejections_and_bounded_body() {
    let (_root, _db, config) = fixture();
    let mut c: Config = serde_json::from_value(config).unwrap();
    for (status, body, expected) in [
        (
            202,
            json!({"id":uuid::Uuid::new_v4().to_string(),"idempotency_key":"deliver:test"})
                .to_string(),
            "delivered",
        ),
        (
            200,
            json!({"id":uuid::Uuid::new_v4().to_string(),"idempotency_key":"deliver:test"})
                .to_string(),
            "delivered",
        ),
        (
            200,
            json!({"id":uuid::Uuid::new_v4().to_string(),"idempotency_key":"different"})
                .to_string(),
            "held",
        ),
        (200, "not-json".into(), "held"),
        (200, "x".repeat(4097), "held"),
        (
            200,
            json!({"id":"bad","idempotency_key":"deliver:test"}).to_string(),
            "held",
        ),
        (409, "SECRET SERVER ERROR".into(), "held"),
        (401, "secret".into(), "held"),
        (403, "secret".into(), "held"),
        (302, "redirect".into(), "held"),
        (400, "rejected".into(), "held"),
        (429, "pressure".into(), "retry"),
        (503, "unavailable".into(), "retry"),
    ] {
        let (url, task) = mock_server(status, body).await;
        c.endpoint = url;
        let result = transport::send(&c, "deliver:test", "{\"safe\":true}").await;
        let request = task.await.unwrap();
        assert!(
            request
                .to_lowercase()
                .contains("idempotency-key: deliver:test")
        );
        assert!(request.to_lowercase().contains("x-tenant-id: operator"));
        let actual = match result {
            transport::Outcome::Delivered => "delivered",
            transport::Outcome::Retry(_) => "retry",
            transport::Outcome::Held(_) => "held",
        };
        assert_eq!(actual, expected, "status {status}");
    }
    c.endpoint = "http://127.0.0.1:1/v1/observations".into();
    assert!(matches!(
        transport::send(&c, "key", "{}").await,
        transport::Outcome::Retry("transport_uncertain")
    ));
    c.token_file = PathBuf::from("/missing/private/token");
    assert!(matches!(
        transport::send(&c, "key", "{}").await,
        transport::Outcome::Held("credential_unavailable")
    ));
}

#[tokio::test]
async fn lost_response_replays_original_frozen_intent_via_controller_tick() {
    let (root, db, config) = fixture();
    setup(&db, &config).unwrap();
    fail(&db);
    capture(&db).unwrap();
    let key: String = db
        .conn
        .query_row("SELECT key FROM operational_observation_outbox", [], |r| {
            r.get(0)
        })
        .unwrap();
    let original = messages(&db);
    // Inject a loopback mock into persisted controller policy, never product setup input.
    let (url, server) = mock_server(503, "response unavailable".into()).await;
    let mut mocked = config.clone();
    mocked["endpoint"] = json!(url);
    db.conn
        .execute(
            "UPDATE operational_observation_policies SET config=?",
            [mocked.to_string()],
        )
        .unwrap();
    export_tick(&db, &trusted(&mocked)).await.unwrap();
    server.await.unwrap();
    assert_eq!(status(&db, None).unwrap()["pending"], 1);
    drop(db);
    let db = Store::open(root.path()).unwrap();
    let (url, server) = mock_server(
        200,
        json!({"id":uuid::Uuid::new_v4().to_string(),"idempotency_key":key}).to_string(),
    )
    .await;
    mocked["endpoint"] = json!(url);
    db.conn
        .execute(
            "UPDATE operational_observation_policies SET config=?",
            [mocked.to_string()],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE operational_observation_outbox SET next_attempt=0",
            [],
        )
        .unwrap();
    export_tick(&db, &trusted(&mocked)).await.unwrap();
    let request = server.await.unwrap();
    assert!(request.contains(&key));
    assert_eq!(messages(&db), original);
    assert_eq!(status(&db, None).unwrap()["delivered"], 1);
    export_tick(&db, &trusted(&mocked)).await.unwrap();
}

#[tokio::test]
async fn discarded_success_response_produces_one_durable_remote_effect() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (_root, db, config) = fixture();
    setup(&db, &config).unwrap();
    fail(&db);
    capture(&db).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut mocked = config;
    mocked["endpoint"] = json!(format!("http://{address}/v1/observations"));
    db.conn
        .execute(
            "UPDATE operational_observation_policies SET config=?",
            [mocked.to_string()],
        )
        .unwrap();
    let server = tokio::spawn(async move {
        let mut effects = std::collections::BTreeSet::new();
        for ordinal in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0; 65536];
            let mut size = 0;
            loop {
                size += stream.read(&mut bytes[size..]).await.unwrap();
                let text = String::from_utf8_lossy(&bytes[..size]);
                if let Some((headers, body)) = text.split_once("\r\n\r\n") {
                    let length: usize = headers
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .unwrap()
                        .parse()
                        .unwrap();
                    if body.len() >= length {
                        break;
                    }
                }
            }
            let request = String::from_utf8(bytes[..size].to_vec()).unwrap();
            let (headers, body) = request.split_once("\r\n\r\n").unwrap();
            let key = headers
                .lines()
                .find_map(|l| l.strip_prefix("idempotency-key: "))
                .unwrap();
            effects.insert((key.to_owned(), body.to_owned()));
            if ordinal == 0 {
                continue;
            } // Effect committed; successful response is lost entirely.
            let body =
                json!({"id":uuid::Uuid::new_v4().to_string(),"idempotency_key":key}).to_string();
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
        effects.len()
    });
    export_tick(&db, &trusted(&mocked)).await.unwrap();
    assert_eq!(status(&db, None).unwrap()["pending"], 1);
    db.conn
        .execute(
            "UPDATE operational_observation_outbox SET next_attempt=0",
            [],
        )
        .unwrap();
    export_tick(&db, &trusted(&mocked)).await.unwrap();
    assert_eq!(status(&db, None).unwrap()["delivered"], 1);
    assert_eq!(server.await.unwrap(), 1);
}

#[test]
fn schema_ten_upgrade_is_additive_and_requires_daemon_to_stop() {
    use fs2::FileExt;
    let (root, db, _config) = fixture();
    db.conn.execute_batch("DROP TABLE operational_observation_outbox; DROP TABLE operational_observation_policies; PRAGMA user_version=10;").unwrap();
    drop(db);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.path().join("daemon.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    assert!(Store::open(root.path()).is_err());
    lock.unlock().unwrap();
    let db = Store::open(root.path()).unwrap();
    assert_eq!(
        db.task("run-1").unwrap()["objective"],
        "DO NOT EXPORT OBJECTIVE"
    );
    assert_eq!(status(&db, None).unwrap()["configured"], false);
    assert_eq!(
        db.conn
            .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        11
    );
}

#[test]
fn typed_controller_auth_error_has_stable_safe_category() {
    let error: anyhow::Error = crate::account_auth::RefreshOwnerRequired.into();
    let error = error.context("controller execution failed");
    assert!(error.is::<crate::account_auth::RefreshOwnerRequired>());
    assert!(error.to_string().contains("controller"));
}

#[test]
fn controller_finish_records_typed_auth_failure_without_untrusted_classification() {
    let (_root, db, config) = fixture();
    setup(&db, &config).unwrap();
    db.conn
        .execute(
            "INSERT INTO steps VALUES('step-auth','run-1','auth','{}','running',NULL)",
            [],
        )
        .unwrap();
    let worker = db.register("run-1", Some("step-auth")).unwrap();
    let worker_id = worker["id"].as_str().unwrap();
    db.conn.execute("INSERT INTO attempts VALUES('attempt-auth','step-auth',?,'running',1,NULL,NULL,NULL,NULL)",[worker_id]).unwrap();
    db.finish(
        "step-auth",
        "attempt-auth",
        worker_id,
        Err(crate::account_auth::RefreshOwnerRequired.into()),
    )
    .unwrap();
    capture(&db).unwrap();
    let message: Value =
        serde_json::from_str(messages(&db)[0]["message"].as_str().unwrap()).unwrap();
    assert_eq!(message["reason"], "credential_refresh_required");
    assert_eq!(
        diagnostics(&db, "default", "run-1", 0, 50).unwrap()["attempts"][0]["failure_code"],
        "credential_refresh_required"
    );
}

#[test]
fn fifo_credentials_fail_without_blocking_controller() {
    let (root, _db, _config) = fixture();
    let path = root.path().join("fifo");
    let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let started = std::time::Instant::now();
    assert!(transport::token(&path).is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}

#[tokio::test]
async fn controller_capture_never_reads_transport_secret_or_sends_http() {
    let (_root, db, mut config) = fixture();
    config["token_file"] = json!("/run/system/telemetry/nonexistent-token");
    setup(&db, &config).unwrap();
    fail(&db);
    tick(&db).await.unwrap();
    let attempts: i64 = db
        .conn
        .query_row(
            "SELECT sum(attempts) FROM operational_observation_outbox",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(attempts, 0);
    assert_eq!(status(&db, None).unwrap()["pending"], 1);
}

fn trusted(config: &Value) -> exporter::Trusted {
    exporter::Trusted {
        schema_version: 1,
        endpoint: config["endpoint"].as_str().unwrap().into(),
        token_file: config["token_file"].as_str().unwrap().into(),
        tenant_id: config["tenant_id"].as_str().unwrap().into(),
        telemetry_project_id: config["telemetry_project_id"].as_str().unwrap().into(),
        diagnostic_thread_id: config["diagnostic_thread_id"].as_str().unwrap().into(),
        project_ids: vec!["default".into()],
    }
}

#[tokio::test]
async fn exporter_rejects_database_redirect_file_scope_and_payload_tampering() {
    let (_root, db, config) = fixture();
    setup(&db, &config).unwrap();
    fail(&db);
    capture(&db).unwrap();
    let gate = trusted(&config);
    let key: String = db
        .conn
        .query_row("SELECT key FROM operational_observation_outbox", [], |r| {
            r.get(0)
        })
        .unwrap();
    let original = messages(&db)[0].to_string();
    for (field, value) in [
        ("endpoint", json!("https://evil.example/v1/observations")),
        ("token_file", json!("/private/other-secret")),
        ("tenant_id", json!("other")),
        ("telemetry_project_id", json!("other")),
        ("diagnostic_thread_id", json!("other")),
        ("project_id", json!("other")),
    ] {
        let mut modified = config.clone();
        modified[field] = value;
        db.conn
            .execute(
                "UPDATE operational_observation_policies SET config=?",
                [modified.to_string()],
            )
            .unwrap();
        db.conn
            .execute(
                "UPDATE operational_observation_outbox SET state='pending'",
                [],
            )
            .unwrap();
        export_tick(&db, &gate).await.unwrap();
        assert_eq!(
            status(&db, None).unwrap()["holds"][0]["error_code"],
            "transport_policy_mismatch"
        );
    }
    let c: Config = serde_json::from_value(config).unwrap();
    let mut payload: Value = serde_json::from_str(&original).unwrap();
    assert!(gate.authorize(&c, &key, &original).is_ok());
    payload["message"] = json!("raw secret");
    assert!(gate.authorize(&c, &key, &payload.to_string()).is_err());
    assert!(gate.authorize(&c, "changed-key", &original).is_err());
}
