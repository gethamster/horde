use horde::{
    capacity::{self, Snapshot},
    config::Settings,
    management, protocol,
    store::{Store, now},
};
use serde_json::json;

#[test]
fn live_limit_survives_restart_and_rejects_invalid_values() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    protocol::dispatch(&db, "runtime_config_set", json!({"concurrency":2}), None).unwrap();
    assert_eq!(management::limit(&db).unwrap(), 2);
    for n in [0, 65] {
        assert!(
            protocol::dispatch(&db, "runtime_config_set", json!({"concurrency":n}), None).is_err()
        );
    }
    drop(db);
    assert_eq!(
        management::limit(&Store::open(dir.path()).unwrap()).unwrap(),
        2
    );
}
#[test]
fn worker_cannot_change_runtime_or_publish_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    for name in [
        "runtime_config_set",
        "runtime_create",
        "runtime_update",
        "account_observe",
        "runtime_drain",
        "management_ack",
    ] {
        assert!(!protocol::worker_allowed(name));
        assert!(protocol::dispatch(&db, name, json!({}), Some("worker-token")).is_err());
    }
}
fn observation() -> Snapshot {
    Snapshot {
        account: "codex:login".into(),
        provider: "codex".into(),
        window: "primary".into(),
        used_percent: Some(95.0),
        reset_at: Some(now() + 3600),
        observed_at: now(),
        source: "provider".into(),
    }
}
#[test]
fn capacity_fallback_unknown_reset_and_stale_windows() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let mut settings = Settings::default();
    // The observation below is of the codex subscription account, so the role under
    // capacity pressure is the one resolving to the codex provider.
    settings.fallbacks.insert("codex".into(), "claude".into());
    assert_eq!(
        capacity::select(&db, &settings, "codex")
            .unwrap()
            .as_deref(),
        Some("codex")
    );
    let mut s = observation();
    capacity::observe(&db, &s).unwrap();
    assert_eq!(
        capacity::select(&db, &settings, "codex")
            .unwrap()
            .as_deref(),
        Some("claude")
    );
    settings.fallbacks.clear();
    assert!(capacity::select(&db, &settings, "codex").unwrap().is_none());
    s.reset_at = Some(now() - 1);
    capacity::observe(&db, &s).unwrap();
    assert!(capacity::available(&db, &s.account).unwrap());
    s.reset_at = Some(now() + 60);
    s.observed_at = now() - 301;
    // Use a separate window so the deliberately old observation isn't rejected as out of order.
    s.window = "secondary".into();
    capacity::observe(&db, &s).unwrap();
    assert!(capacity::available(&db, &s.account).unwrap());
}
#[test]
fn capacity_observations_are_validated_and_threshold_events_deduplicated() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let mut s = observation();
    capacity::observe(&db, &s).unwrap();
    capacity::observe(&db, &s).unwrap();
    assert_eq!(
        db.rows("SELECT * FROM management_events", &[])
            .unwrap()
            .len(),
        1
    );
    s.used_percent = Some(101.0);
    assert!(capacity::observe(&db, &s).is_err());
    s.used_percent = Some(f64::NAN);
    assert!(capacity::observe(&db, &s).is_err());
    s.used_percent = Some(100.0);
    s.observed_at = now() - 10;
    capacity::observe(&db, &s).unwrap();
    assert_eq!(
        db.rows("SELECT used FROM account_capacity", &[]).unwrap()[0]["used"],
        95.0
    );
}
#[test]
fn remote_management_deduplicates_and_binds_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let args = json!({"request_id":"123","action":"runtime_update","version":"0.2.1"});
    management::remote_command(&db, "parent", &args).unwrap();
    management::remote_command(&db, "parent", &args).unwrap();
    assert_eq!(
        db.rows("SELECT * FROM runtime_operations", &[])
            .unwrap()
            .len(),
        1
    );
    assert!(
        management::remote_command(
            &db,
            "parent",
            &json!({"request_id":"123","action":"runtime_update","version":"0.2.2"})
        )
        .is_err()
    );
}
#[test]
fn service_definition_runs_as_user_and_escapes_paths() {
    use std::path::Path;
    let mac = horde::service::render(
        "macos",
        Path::new("/a&b/task"),
        Path::new("/data"),
        "alice",
        Path::new("/Users/alice"),
        Path::new("/Users/alice/.config"),
    )
    .unwrap();
    assert!(mac.contains("/a&amp;b/task"));
    assert!(mac.contains("<key>UserName</key><string>alice</string>"));
    let linux = horde::service::render(
        "linux",
        Path::new("/a%b/task"),
        Path::new("/data"),
        "alice",
        Path::new("/home/alice"),
        Path::new("/home/alice/.config"),
    )
    .unwrap();
    assert!(linux.contains("User=alice"));
    assert!(linux.contains("/a%%b/task"));
    assert!(
        horde::service::render(
            "linux",
            Path::new("/bin/task"),
            Path::new("/data"),
            "root\nExecStart=bad",
            Path::new("/home/alice"),
            Path::new("/home/alice/.config")
        )
        .is_err()
    );
}
#[test]
fn service_definition_preserves_selected_configuration_directory() {
    use std::path::Path;
    let render = |platform, config| {
        horde::service::render(
            platform,
            Path::new("/usr/local/bin/task"),
            Path::new("/data/runtime"),
            "alice",
            Path::new("/home/alice"),
            Path::new(config),
        )
    };
    let config = "/data/alice & team 100%/config";
    let mac = render("macos", config).unwrap();
    assert!(
        mac.contains(
            "<key>XDG_CONFIG_HOME</key><string>/data/alice &amp; team 100%/config</string>"
        )
    );
    let linux = render("linux", config).unwrap();
    assert!(linux.contains("Environment=\"XDG_CONFIG_HOME=/data/alice & team 100%%/config\""));
    for platform in ["macos", "linux"] {
        assert!(render(platform, "relative/config").is_err());
        assert!(render(platform, "/data/config\nEnvironment=BAD").is_err());
    }
}
#[test]
fn signed_release_rejects_tampering_and_incompatibility() {
    use ring::{
        rand::SystemRandom,
        signature::{Ed25519KeyPair, KeyPair},
    };
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let manifest =
        json!({"version":"0.5.0","protocol":1,"schema_min":2,"schema_max":3,"artifacts":[]})
            .to_string();
    let sig = key.sign(manifest.as_bytes());
    assert!(
        horde::update::verify(manifest.as_bytes(), sig.as_ref(), key.public_key().as_ref()).is_ok()
    );
    assert!(horde::update::verify(b"changed", sig.as_ref(), key.public_key().as_ref()).is_err());
    for (minimum, maximum, compatible) in [(2, 3, true), (3, 3, true), (2, 2, false), (4, 4, false)]
    {
        let mut candidate: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        candidate["schema_min"] = json!(minimum);
        candidate["schema_max"] = json!(maximum);
        let candidate = candidate.to_string();
        let signature = key.sign(candidate.as_bytes());
        assert_eq!(
            horde::update::verify(
                candidate.as_bytes(),
                signature.as_ref(),
                key.public_key().as_ref()
            )
            .is_ok(),
            compatible,
            "schema range {minimum}..={maximum}"
        );
    }
    let incompatible = manifest.replace("\"protocol\":1", "\"protocol\":2");
    let sig = key.sign(incompatible.as_bytes());
    assert!(
        horde::update::verify(
            incompatible.as_bytes(),
            sig.as_ref(),
            key.public_key().as_ref()
        )
        .is_err()
    );
}
#[test]
fn kubernetes_has_dedicated_storage_and_limits() {
    let p = horde::fleet::Profile {
        provider: "kubernetes".into(),
        image: format!("ghcr.io/asomervell/horde@sha256:{}", "0".repeat(64)),
        ..Default::default()
    };
    p.validate().unwrap();
    let v = horde::fleet::kubernetes_manifest(&p, "worker-1");
    assert_eq!(v["kind"], "StatefulSet");
    assert_eq!(v["spec"]["replicas"], 1);
    assert_eq!(
        v["spec"]["volumeClaimTemplates"][0]["metadata"]["name"],
        "data"
    );
    assert_eq!(
        v["spec"]["template"]["spec"]["containers"][0]["resources"]["limits"]["memory"],
        "2048Mi"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outbound_mtls_control_enrolls_once_and_manages_without_inbound_remote_port() {
    use horde::network::{DirectPeer, NetworkConfig, Provider};
    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
        KeyUsagePurpose,
    };
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let remote = temp.path().join("remote");
    let db = Store::open(&root).unwrap();
    Store::open(&remote).unwrap();
    let mut parameters = CertificateParams::new(Vec::<String>::new()).unwrap();
    parameters.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    parameters.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let key = KeyPair::generate().unwrap();
    let ca = parameters.self_signed(&key).unwrap();
    let issuer = Issuer::new(parameters, key);
    let make = |name: &str| {
        let key = KeyPair::generate().unwrap();
        let mut parameters = CertificateParams::new(vec![format!("{name}.test")]).unwrap();
        parameters.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        let cert = parameters.signed_by(&key, &issuer).unwrap();
        let path = temp.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        horde::secrets::write_private(&path.join("key.pem"), key.serialize_pem().as_bytes())
            .unwrap();
        std::fs::write(path.join("cert.pem"), cert.pem()).unwrap();
        std::fs::write(path.join("ca.pem"), ca.pem()).unwrap();
        (
            NetworkConfig {
                provider: Provider::Direct,
                runtime_id: name.into(),
                identity_key: path.join("key.pem"),
                identity_cert: path.join("cert.pem"),
                ca_cert: path.join("ca.pem"),
                timeout_seconds: 5,
                ..Default::default()
            },
            horde::store::hash(cert.der()),
        )
    };
    let (mut parent, parent_fingerprint) = make("parent");
    let (mut child, child_fingerprint) = make("child");
    parent
        .allowed_clients
        .insert(parent_fingerprint.clone(), "parent".into());
    db.conn
        .execute(
            "INSERT INTO runtime_enrollments VALUES('child',?,?,?,'pending')",
            rusqlite::params![
                child_fingerprint,
                horde::store::hash(b"single-use-token"),
                now() + 60
            ],
        )
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    parent.port = listener.local_addr().unwrap().port();
    child.controller_peer = Some("parent".into());
    child.enrollment_token = Some("single-use-token".into());
    child.execution_clients.push("parent".into());
    child.management_clients.push("parent".into());
    child
        .allowed_clients
        .insert(parent_fingerprint, "parent".into());
    child.peers.insert(
        "parent".into(),
        DirectPeer {
            address: listener.local_addr().unwrap(),
            tls_name: "parent.test".into(),
        },
    );
    let server_config = parent.clone();
    let server_root = root.clone();
    let server = tokio::spawn(async move {
        horde::network::serve_runtime(
            &server_config,
            listener,
            std::future::pending::<()>(),
            server_root,
        )
        .await
    });
    let control = tokio::spawn(horde::control::connect(remote.clone(), child.clone()));
    let mut reply = None;
    for _ in 0..100 {
        if let Ok(Some(value)) =
            horde::control::call(&parent, "child", "capabilities", &json!({})).await
        {
            reply = Some(value);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(reply.unwrap()["runtime"], "child");
    assert_eq!(
        db.rows("SELECT state,token_hash FROM runtime_enrollments", &[])
            .unwrap()[0],
        json!({"state":"active","token_hash":""})
    );
    let args = json!({"request_id":"update-1","action":"runtime_update","version":"0.2.1"});
    let result = horde::federation::call(&parent, "child", "manage", args.clone())
        .await
        .unwrap();
    assert_eq!(result["state"], "accepted");
    let result = horde::federation::call(&parent, "child", "manage", args)
        .await
        .unwrap();
    assert_eq!(result["state"], "local_pending");
    control.abort();
    let _ = control.await;
    let remote_db = Store::open(&remote).unwrap();
    remote_db
        .conn
        .execute(
            "UPDATE runtime_operations SET state='succeeded' WHERE id='parent:update-1'",
            [],
        )
        .unwrap();
    child.enrollment_token = None;
    let reconnected = tokio::spawn(horde::control::connect(remote.clone(), child));
    let mut completed = false;
    for _ in 0..100 {
        let args = json!({"request_id":"update-1","action":"runtime_update","version":"0.2.1"});
        if horde::federation::call(&parent, "child", "manage", args)
            .await
            .is_ok_and(|v| v["state"] == "succeeded")
        {
            completed = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(completed);
    assert_eq!(
        remote_db
            .rows("SELECT * FROM runtime_operations", &[])
            .unwrap()
            .len(),
        1
    );
    reconnected.abort();
    server.abort();
    let _ = reconnected.await;
    let _ = server.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cloud_provisioning_uses_provider_contracts_and_deduplicates_requests() {
    use axum::{Json, Router, http::HeaderMap, routing::post};
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex};
    struct Process(std::process::Child);
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for provider in ["e2b", "daytona"] {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let seen = captured.clone();
        let route = if provider == "e2b" {
            "/sandboxes"
        } else {
            "/sandbox"
        };
        let app=Router::new().route(route,post(move|headers:HeaderMap,Json(body):Json<serde_json::Value>|{let seen=seen.clone();async move{
            assert!(headers.get("x-api-key").is_some_and(|v|v=="fixture-secret-do-not-store")||headers.get("authorization").is_some_and(|v|v=="Bearer fixture-secret-do-not-store"));
            seen.lock().unwrap().push(body);
            Json(json!({"sandboxID":"sandbox-fixture","id":"sandbox-fixture","envdAccessToken":"provider-response-secret"}))
        }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let temp = tempfile::Builder::new()
            .prefix("task-provider-")
            .tempdir_in("/tmp")
            .unwrap();
        let root = temp.path().join("data");
        let config = temp.path().join("config");
        std::fs::create_dir_all(config.join("horde")).unwrap();
        std::fs::write(config.join("horde/runtimes.toml"),format!("[profiles.fixture]\nprovider='{provider}'\nendpoint='http://{address}'\napi_key_env='FIXTURE_PROVIDER_KEY'\nimage='task-template'\n")).unwrap();
        let process = Process(
            Command::new(env!("CARGO_BIN_EXE_horde"))
                .args(["--data-dir"])
                .arg(&root)
                .arg("daemon")
                .env("XDG_CONFIG_HOME", &config)
                .env("HOME", temp.path())
                .env("FIXTURE_PROVIDER_KEY", "fixture-secret-do-not-store")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        for _ in 0..200 {
            if root.join("daemon.sock").exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let create = || {
            Command::new(env!("CARGO_BIN_EXE_horde"))
                .arg("--data-dir")
                .arg(&root)
                .args([
                    "runtime",
                    "create",
                    "worker-1",
                    "--profile",
                    "fixture",
                    "--request-id",
                    "create-once",
                ])
                .env("XDG_CONFIG_HOME", &config)
                .output()
                .unwrap()
        };
        assert!(create().status.success());
        assert!(create().status.success());
        let db = Store::open(&root).unwrap();
        let mut completed = false;
        for _ in 0..200 {
            let rows = db
                .rows("SELECT state FROM runtime_operations", &[])
                .unwrap();
            if rows.first().is_some_and(|r| r["state"] == "succeeded") {
                completed = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            completed,
            "{}",
            serde_json::to_string(&db.rows("SELECT * FROM runtime_operations", &[]).unwrap())
                .unwrap()
        );
        let bodies = captured.lock().unwrap().clone();
        assert_eq!(bodies.len(), 1);
        if provider == "e2b" {
            assert_eq!(bodies[0]["templateID"], "task-template");
            assert_eq!(bodies[0]["secure"], true);
            assert_eq!(bodies[0]["autoPause"], true);
        } else {
            assert_eq!(bodies[0]["snapshot"], "task-template");
            assert_eq!(bodies[0]["labels"]["task-runtime"], "worker-1");
        }
        let rows = db.rows("SELECT * FROM managed_runtimes", &[]).unwrap();
        assert_eq!(rows[0]["state"], "provisioned");
        assert_eq!(rows[0]["resource"], "sandbox-fixture");
        let stored = serde_json::to_string(&rows).unwrap();
        assert!(!stored.contains("fixture-secret-do-not-store"));
        assert!(!stored.contains("provider-response-secret"));
        drop(process);
        server.abort();
        let _ = server.await;
    }
}

#[test]
fn quota_hold_respects_the_role_selected_by_prior_failures() {
    let temp = tempfile::tempdir().unwrap();
    let db = Store::open(temp.path()).unwrap();
    let mut settings = Settings::default();
    settings.fallbacks.insert("worker".into(), "claude".into());
    let plan = horde::template::compile(
        "simulated",
        &horde::template::load_templates(temp.path()).unwrap(),
        std::collections::BTreeMap::from([("task".into(), "test".into())]),
    )
    .unwrap();
    let task = db.submit("test", temp.path(), &settings, &plan).unwrap();
    let step = db.steps(&task).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    db.conn
        .execute(
            "INSERT INTO attempts(id,step,state,started) VALUES('failed-attempt',?,'failed',?)",
            rusqlite::params![step, now()],
        )
        .unwrap();
    let mut s = observation();
    s.account = "claude:login".into();
    s.provider = "claude".into();
    capacity::observe(&db, &s).unwrap();
    assert!(
        capacity::select(&db, &settings, "worker")
            .unwrap()
            .is_some()
    );
    assert!(
        capacity::select_for_step(&db, &settings, "worker", &step)
            .unwrap()
            .is_none()
    );
}
