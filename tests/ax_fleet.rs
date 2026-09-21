use horde::fleet::Profile;
use serde_json::json;

fn profile() -> Profile {
    serde_json::from_value(json!({
        "provider":"ax", "project":"default",
        "endpoint":"http://127.0.0.1:18080",
        "ax_router_endpoint":"http://127.0.0.1:18081",
        "image":format!("example/horde-ax@sha256:{}", "a".repeat(64))
    }))
    .unwrap()
}

#[test]
fn ax_profile_requires_explicit_endpoints_and_pinned_runner() {
    let p = profile();
    p.validate().unwrap();
    for change in [
        json!({"endpoint":""}),
        json!({"ax_router_endpoint":""}),
        json!({"image":"example/horde-ax:latest"}),
        json!({"ax_revision":"main"}),
        json!({"host":"some-host"}),
    ] {
        let mut v = serde_json::to_value(&p).unwrap();
        for (key, value) in change.as_object().unwrap() {
            v[key] = value.clone();
        }
        assert!(
            serde_json::from_value::<Profile>(v)
                .unwrap()
                .validate()
                .is_err()
        );
    }
}

#[path = "ax_fleet/mock.rs"]
mod mock;
use horde::{fleet::ax, management, store::Store};
struct Fixture {
    _dir: tempfile::TempDir,
    db: Store,
    p: Profile,
    packet: serde_json::Value,
    mock: mock::Mock,
    requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    handles: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for h in &self.handles {
            h.abort();
        }
    }
}
impl Fixture {
    async fn new() -> Self {
        let (mock, endpoint, api) = mock::server().await;
        let (router, requests, http) = mock::router().await;
        let dir = tempfile::tempdir().unwrap();
        let db = Store::open(dir.path()).unwrap();
        let p = Profile {
            endpoint,
            ax_router_endpoint: router,
            ..profile()
        };
        db.conn.execute("INSERT INTO managed_runtimes(id,profile,spec,state,created) VALUES('worker','ax',?,'requested',1)",[serde_json::to_string(&p).unwrap()]).unwrap();
        let packet = json!({"id":"worker","project":{"id":"default"},"key":"private-enrollment-key","network":{"peers":{"controller":{"address":"100.64.1.1:65443"}}}});
        let path = db
            .root
            .join("projects/default/runtime-bootstrap/worker.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, packet.to_string()).unwrap();
        Self {
            _dir: dir,
            db,
            p,
            packet,
            mock,
            requests,
            handles: vec![api, http],
        }
    }
    async fn provision(&self) -> String {
        ax::provision(&self.db, &self.p, "worker", Some(&self.packet))
            .await
            .unwrap()
    }
}
#[tokio::test]
async fn create_is_idempotent_and_bootstrap_never_enters_ax_specs() {
    let f = Fixture::new().await;
    let name = f.provision().await;
    assert_eq!(f.provision().await, name);
    let state = f.mock.0.lock().unwrap();
    assert_eq!(state.writes, vec!["workspace", "gateway", "task"]);
    let task = state.task.as_ref().unwrap();
    let spec = task.spec.as_ref().unwrap();
    assert_eq!(task.api_version, "ax.io/v1alpha1");
    assert!(!spec.debug);
    assert_eq!(spec.workspaces[0].path, "/workspace");
    assert!(!format!("{task:?}").contains("private-enrollment-key"));
    let hosts = &state
        .gateway
        .as_ref()
        .unwrap()
        .spec
        .as_ref()
        .unwrap()
        .egress
        .as_ref()
        .unwrap()
        .allowlist
        .as_ref()
        .unwrap()
        .hosts;
    assert!(
        hosts
            .iter()
            .any(|r| r.host == "100.64.1.1/32" && r.port == 65443)
    );
    let requests = f.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("private-enrollment-key"));
    assert!(requests[0].contains(&format!("ate-target-actor: horde-default/{name}")));
}
#[tokio::test]
async fn changed_owner_project_image_and_debug_are_rejected_without_writes() {
    for change in ["owner", "image", "project", "debug"] {
        let f = Fixture::new().await;
        let name = f.provision().await;
        {
            let mut state = f.mock.0.lock().unwrap();
            let task = state.task.as_mut().unwrap();
            match change {
                "owner" => task.spec.as_mut().unwrap().env[2].value = "someone-else".into(),
                "image" => task.spec.as_mut().unwrap().image = "other-image".into(),
                "project" => task.metadata.as_mut().unwrap().atespace = "horde-other".into(),
                _ => task.spec.as_mut().unwrap().debug = true,
            }
        }
        assert!(
            ax::lifecycle(&f.db, &f.p, "worker", &name, "runtime_destroy")
                .await
                .unwrap_err()
                .to_string()
                .contains("ownership")
        );
        assert_eq!(f.mock.0.lock().unwrap().writes.len(), 3);
    }
}
#[tokio::test]
async fn lost_create_reply_keeps_recovery_identity_and_does_not_duplicate() {
    let f = Fixture::new().await;
    f.mock.0.lock().unwrap().fail_update = true;
    let error = ax::provision(&f.db, &f.p, "worker", Some(&f.packet))
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("secret remote error body"));
    let name =
        f.db.rows(
            "SELECT resource FROM managed_runtimes WHERE id='worker'",
            &[],
        )
        .unwrap()[0]["resource"]
            .as_str()
            .unwrap()
            .to_owned();
    assert!(!name.is_empty());
    assert_eq!(f.provision().await, name);
    assert_eq!(
        f.mock
            .0
            .lock()
            .unwrap()
            .writes
            .iter()
            .filter(|w| **w == "task")
            .count(),
        1
    );
}
#[tokio::test]
async fn stop_waits_for_authenticated_drain_and_keeps_dispatch_hold() {
    let f = Fixture::new().await;
    let name = f.provision().await;
    let result = ax::lifecycle(&f.db, &f.p, "worker", &name, "runtime_stop")
        .await
        .unwrap();
    assert_eq!(result["lifecycle_pending"], true);
    assert_eq!(
        management::value(&f.db, "ax_hold:worker")
            .unwrap()
            .as_deref(),
        Some("true")
    );
    assert!(!f.mock.0.lock().unwrap().writes.contains(&"suspend"));
}
#[tokio::test]
async fn suspended_worker_retains_identity_and_destroy_waits_before_aux_cleanup() {
    let f = Fixture::new().await;
    let name = f.provision().await;
    {
        let mut state = f.mock.0.lock().unwrap();
        let task = state.task.as_mut().unwrap();
        task.spec.as_mut().unwrap().suspend = true;
        task.status.as_mut().unwrap().phase = "Suspended".into();
    }
    let reconciled = ax::lifecycle(&f.db, &f.p, "worker", &name, "runtime_reconcile")
        .await
        .unwrap();
    assert_eq!(reconciled["state"], "stopped");
    let stopped = ax::lifecycle(&f.db, &f.p, "worker", &name, "runtime_stop")
        .await
        .unwrap();
    assert_eq!(stopped["state"], "stopped");
    let result = ax::lifecycle(&f.db, &f.p, "worker", &name, "runtime_destroy")
        .await
        .unwrap();
    assert_eq!(result["lifecycle_pending"], true);
    assert!(f.mock.0.lock().unwrap().workspace.is_some());
    let result = ax::lifecycle(&f.db, &f.p, "worker", &name, "runtime_destroy")
        .await
        .unwrap();
    assert_eq!(result["state"], "removed");
    assert!(f.mock.0.lock().unwrap().workspace.is_none());
}
#[tokio::test]
async fn resume_does_not_recreate_task_or_release_hold_without_horde_readiness() {
    let f = Fixture::new().await;
    let name = f.provision().await;
    {
        let mut state = f.mock.0.lock().unwrap();
        let task = state.task.as_mut().unwrap();
        task.spec.as_mut().unwrap().suspend = true;
        task.status.as_mut().unwrap().phase = "Suspended".into();
    }
    for _ in 0..2 {
        assert_eq!(
            ax::lifecycle(&f.db, &f.p, "worker", &name, "runtime_start")
                .await
                .unwrap()["lifecycle_pending"],
            true
        );
    }
    let state = f.mock.0.lock().unwrap();
    assert_eq!(state.writes.iter().filter(|w| **w == "task").count(), 1);
    assert_eq!(state.writes.iter().filter(|w| **w == "resume").count(), 1);
    assert_eq!(
        management::value(&f.db, "ax_hold:worker")
            .unwrap()
            .as_deref(),
        Some("true")
    );
}
#[tokio::test]
async fn altered_auxiliary_resources_are_retained_after_task_disappears() {
    let f = Fixture::new().await;
    let name = f.provision().await;
    {
        let mut state = f.mock.0.lock().unwrap();
        state.task = None;
        state
            .gateway
            .as_mut()
            .unwrap()
            .spec
            .as_mut()
            .unwrap()
            .listeners[0]
            .port = 8080;
    }
    let result = ax::lifecycle(&f.db, &f.p, "worker", &name, "runtime_destroy").await;
    assert!(result.is_err(), "must not delete an altered gateway");
    assert!(f.mock.0.lock().unwrap().gateway.is_some());
}
#[tokio::test]
async fn reconcile_absent_task_retains_uncertainty_and_bootstrap_identity() {
    let f = Fixture::new().await;
    let name = f.provision().await;
    f.mock.0.lock().unwrap().task = None;
    let result = ax::lifecycle(&f.db, &f.p, "worker", &name, "runtime_reconcile")
        .await
        .unwrap();
    assert_eq!(result["state"], "uncertain");
    assert_eq!(result["absent"], true);
    assert!(f.mock.0.lock().unwrap().workspace.is_some());
    assert!(
        management::value(&f.db, "ax_owner:worker")
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn unactivated_runner_can_be_destroyed_without_a_horde_control_stream() {
    let f = Fixture::new().await;
    let name = f.provision().await;
    f.db.conn.execute("INSERT INTO runtime_enrollments(runtime,fingerprint,token_hash,expires,state) VALUES('worker','fingerprint','token',9999999999,'pending')", []).unwrap();
    let result = ax::lifecycle(&f.db, &f.p, "worker", &name, "runtime_destroy")
        .await
        .unwrap();
    assert_eq!(result["lifecycle_pending"], true);
    assert!(f.mock.0.lock().unwrap().writes.contains(&"delete-task"));
}

#[tokio::test]
async fn explicit_destroy_retry_republishes_terminating_task_once_per_operation() {
    let f = Fixture::new().await;
    let name = f.provision().await;
    {
        let mut state = f.mock.0.lock().unwrap();
        state.retain_delete = true;
        state.task.as_mut().unwrap().status.as_mut().unwrap().phase = "Terminating".into();
    }
    for request in ["delete-one", "delete-two"] {
        f.db.conn
            .execute("UPDATE runtime_operations SET state='uncertain'", [])
            .unwrap();
        f.db.conn.execute("INSERT INTO runtime_operations(id,runtime,action,args,state,created) VALUES(?,'worker','runtime_destroy','{}','running',1)", [request]).unwrap();
        for _ in 0..2 {
            let result = ax::lifecycle(&f.db, &f.p, "worker", &name, "runtime_destroy")
                .await
                .unwrap();
            assert_eq!(result["lifecycle_pending"], true);
        }
    }
    assert_eq!(
        f.mock
            .0
            .lock()
            .unwrap()
            .writes
            .iter()
            .filter(|w| **w == "delete-task")
            .count(),
        2
    );
}

#[tokio::test]
async fn waiting_ax_operation_does_not_starve_later_fleet_operations() {
    let f = Fixture::new().await;
    let name = f.provision().await;
    for (id, action, created, args) in [
        ("stop-one", "runtime_stop", 1, json!({})),
        (
            "inspect-two",
            "runtime_reconcile",
            2,
            json!({"resource":name}),
        ),
    ] {
        f.db.conn.execute("INSERT INTO runtime_operations(id,runtime,action,args,state,created) VALUES(?,'worker',?,?,'pending',?)", rusqlite::params![id,action,args.to_string(),created]).unwrap();
    }
    horde::fleet::tick(&f.db).await.unwrap();
    horde::fleet::tick(&f.db).await.unwrap();
    let rows =
        f.db.rows(
            "SELECT id,state FROM runtime_operations ORDER BY created",
            &[],
        )
        .unwrap();
    assert_eq!(rows[0]["state"], "waiting");
    assert_eq!(rows[1]["state"], "succeeded");
}

#[tokio::test]
async fn fair_cursor_preserves_lifecycle_order_within_one_runtime() {
    let f = Fixture::new().await;
    f.provision().await;
    for (id, action, created) in [
        ("stop-first", "runtime_stop", 1),
        ("start-second", "runtime_start", 2),
    ] {
        f.db.conn.execute("INSERT INTO runtime_operations(id,runtime,action,args,state,created) VALUES(?,'worker',?,'{}','pending',?)", rusqlite::params![id,action,created]).unwrap();
    }
    horde::fleet::tick(&f.db).await.unwrap();
    horde::fleet::tick(&f.db).await.unwrap();
    let rows =
        f.db.rows(
            "SELECT id,state FROM runtime_operations ORDER BY created",
            &[],
        )
        .unwrap();
    assert_eq!(rows[0]["state"], "waiting");
    assert_eq!(rows[1]["state"], "pending");
}

#[tokio::test]
async fn rejected_project_operation_does_not_leave_a_dispatch_hold() {
    let f = Fixture::new().await;
    f.provision().await;
    let result = horde::fleet::dispatch(
        &f.db,
        "runtime_stop",
        &json!({"id":"worker","project":"missing-project","request_id":"rejected"}),
    );
    assert!(result.is_err());
    assert!(
        management::value(&f.db, "ax_hold:worker")
            .unwrap()
            .is_none()
    );
    assert!(
        f.db.rows("SELECT id FROM runtime_operations WHERE id='rejected'", &[])
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn https_ax_endpoint_uses_tls_and_rejects_an_untrusted_certificate() {
    let mut f = Fixture::new().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    f.p.endpoint = format!("https://{}", listener.local_addr().unwrap());
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(vec!["127.0.0.1".into()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let identity = tonic::transport::Identity::from_pem(cert.pem(), key.serialize_pem());
    let service = f.mock.clone();
    f.handles.push(tokio::spawn(async move {
        tonic::transport::Server::builder()
            .tls_config(tonic::transport::ServerTlsConfig::new().identity(identity))
            .unwrap()
            .add_service(ax::wire::ax_server::AxServer::new(service))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    }));
    let error = ax::provision(&f.db, &f.p, "worker", Some(&f.packet))
        .await
        .unwrap_err();
    let details = format!("{error:#}");
    assert!(
        details.contains("UnknownIssuer"),
        "TLS must validate the peer certificate: {details}"
    );
    assert!(f.mock.0.lock().unwrap().writes.is_empty());
}

#[tokio::test]
async fn pinned_ax_image_rejects_binary_update_without_pausing_the_fleet() {
    let f = Fixture::new().await;
    f.provision().await;
    let result = horde::fleet::dispatch(
        &f.db,
        "runtime_update",
        &json!({"id":"worker","version":"0.6.6","request_id":"image-update"}),
    );
    assert!(
        result.is_err(),
        "AX image binaries must not use the native updater"
    );
    assert!(result.unwrap_err().to_string().contains("pinned image"));
    assert!(
        f.db.rows(
            "SELECT id FROM runtime_operations WHERE id='image-update'",
            &[]
        )
        .unwrap()
        .is_empty()
    );
    assert!(
        management::value(&f.db, "fleet_updates_paused")
            .unwrap()
            .is_none()
    );
    for action in ["runtime_restart", "runtime_skills_update"] {
        let accepted =
            horde::fleet::dispatch(&f.db, action, &json!({"id":"worker","request_id":action}))
                .unwrap()
                .unwrap();
        assert_eq!(accepted["state"], "pending");
    }
}

#[tokio::test]
async fn gateway_deduplicates_hosts_when_stock_ax_ignores_ports() {
    let mut f = Fixture::new().await;
    f.p.ax_egress = vec![
        "100.64.1.1/32:65345".into(),
        "100.64.1.1/32:65443".into(),
        "*:443".into(),
        "*:80".into(),
    ];
    f.provision().await;
    let state = f.mock.0.lock().unwrap();
    let hosts = &state
        .gateway
        .as_ref()
        .unwrap()
        .spec
        .as_ref()
        .unwrap()
        .egress
        .as_ref()
        .unwrap()
        .allowlist
        .as_ref()
        .unwrap()
        .hosts;
    assert_eq!(
        hosts.len(),
        2,
        "stock AX turns host rules into CIDRs and ignores ports"
    );
    assert_eq!(hosts[0].host, "100.64.1.1/32");
    assert_eq!(hosts[0].port, 65345);
    assert_eq!(hosts[1].host, "*");
}
