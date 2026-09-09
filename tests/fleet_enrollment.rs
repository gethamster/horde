use horde::{
    federation::wire::{
        CallRequest, EnrollmentRequest, RenewalRequest, enrollment_client::EnrollmentClient,
        federation_client::FederationClient,
    },
    fleet_enrollment::{Certificate, Invitation, ServerConfig, authority, service, worker},
    network::{NetworkConfig, Provider},
    store::{Store, now},
};
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle};
use tonic::transport::{
    Certificate as TlsCertificate, Channel, ClientTlsConfig, Endpoint, Identity,
};

struct Fixture {
    dir: tempfile::TempDir,
    network: NetworkConfig,
    server: ServerConfig,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        Store::open(dir.path()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let runtime = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca = params.self_signed(&key).unwrap();
        let issuer = Issuer::new(params, key);
        let controller = KeyPair::generate().unwrap();
        let cert = CertificateParams::new(vec!["controller.test".into()])
            .unwrap()
            .signed_by(&controller, &issuer)
            .unwrap();
        let network = NetworkConfig {
            provider: Provider::Direct,
            runtime_id: format!("controller-{}", horde::store::id()),
            port: runtime.local_addr().unwrap().port(),
            timeout_seconds: 3,
            ca_cert: dir.path().join("ca.pem"),
            identity_cert: dir.path().join("controller.pem"),
            identity_key: dir.path().join("controller.key"),
            ..Default::default()
        };
        let server = ServerConfig {
            listen: listener.local_addr().unwrap(),
            controller_address: runtime.local_addr().unwrap(),
            tls_name: "controller.test".into(),
            issuer_key: dir.path().join("ca.key"),
        };
        for (path, contents) in [
            (&network.ca_cert, ca.pem()),
            (&network.identity_cert, cert.pem()),
            (&network.identity_key, controller.serialize_pem()),
            (&server.issuer_key, issuer.key().serialize_pem()),
        ] {
            horde::secrets::write_private(path, contents.as_bytes()).unwrap();
        }
        horde::federation::configure(dir.path(), &network).unwrap();
        let config = network.clone();
        let enrollment = server.clone();
        let root = dir.path().to_owned();
        let enrollment_task = tokio::spawn(async move {
            service::serve(&config, &enrollment, listener, root, std::future::pending())
                .await
                .unwrap();
        });
        let config = network.clone();
        let root = dir.path().to_owned();
        let runtime_task = tokio::spawn(async move {
            horde::network::serve_runtime(&config, runtime, std::future::pending(), root)
                .await
                .unwrap();
        });
        Self {
            dir,
            network,
            server,
            tasks: vec![enrollment_task, runtime_task],
        }
    }

    fn db(&self) -> Store {
        Store::open(self.dir.path()).unwrap()
    }

    fn invitation(&self) -> Invitation {
        authority::create_key(
            &self.db(),
            &self.network,
            &self.server,
            "sandboxes",
            3600,
            10,
            2,
        )
        .unwrap()
    }

    fn write_invitation(&self, invitation: &Invitation) -> std::path::PathBuf {
        let path = self.dir.path().join(format!("{}.json", invitation.key_id));
        horde::secrets::write_private(&path, &serde_json::to_vec(invitation).unwrap()).unwrap();
        path
    }

    async fn channel(&self, enrollment: bool, identity: Option<Identity>) -> Channel {
        let tls = ClientTlsConfig::new()
            .ca_certificate(TlsCertificate::from_pem(
                std::fs::read(&self.network.ca_cert).unwrap(),
            ))
            .domain_name(&self.server.tls_name);
        let tls = match identity {
            Some(identity) => tls.identity(identity),
            None => tls,
        };
        let address = if enrollment {
            self.server.listen
        } else {
            self.server.controller_address
        };
        Endpoint::from_shared(format!("https://{address}"))
            .unwrap()
            .tls_config(tls)
            .unwrap()
            .timeout(Duration::from_secs(3))
            .connect_timeout(Duration::from_secs(3))
            .connect()
            .await
            .unwrap()
    }
}

fn command(root: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_horde"));
    command
        .arg("--data-dir")
        .arg(root)
        .env("XDG_CONFIG_HOME", root.join("user-config"))
        .env_remove("HORDE_WORKER_TOKEN")
        .env_remove("HORDE_BOOTSTRAP_JSON")
        .env_remove("HORDE_ENROLLMENT_FILE")
        .env_remove("HORDE_ENROLLMENT_JSON")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    command
}

async fn join(root: &Path, invitation: &Path) -> Certificate {
    let output = command(root)
        .args(["network", "join", "--invitation"])
        .arg(invitation)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    state(root)
}

fn state(root: &Path) -> Certificate {
    let value: Value =
        serde_json::from_slice(&std::fs::read(root.join("fleet-worker.json")).unwrap()).unwrap();
    serde_json::from_value(value["certificate"].clone()).unwrap()
}

fn identity(root: &Path) -> Identity {
    Identity::from_pem(
        state(root).certificate_pem,
        std::fs::read(root.join("fleet-worker.key")).unwrap(),
    )
}

fn force_renewal(f: &Fixture, root: &Path) {
    f.db()
        .conn
        .execute(
            "UPDATE fleet_enrollment_certificates SET renew_after=?",
            [now() - 1],
        )
        .unwrap();
    let path = root.join("fleet-worker.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["certificate"]["renew_after"] = json!(now() - 1);
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("timed out waiting for worker state");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_invitation_creates_distinct_durable_worker_identities_and_renews_without_key() {
    let f = Fixture::new().await;
    let invitation = f.invitation();
    let path = f.write_invitation(&invitation);
    let first = f.dir.path().join("worker-a");
    let second = f.dir.path().join("worker-b");
    let a = join(&first, &path).await;
    let b = join(&second, &path).await;
    assert_ne!(a.runtime_id, b.runtime_id);
    let key = std::fs::read(first.join("fleet-worker.key")).unwrap();
    assert_ne!(key, std::fs::read(second.join("fleet-worker.key")).unwrap());
    let persisted = std::fs::read_to_string(first.join("fleet-worker.json")).unwrap();
    assert!(!persisted.contains(&invitation.token));
    let pending = std::fs::read_to_string(first.join("fleet-worker-pending.json")).unwrap();
    assert!(!pending.contains(&invitation.token));
    std::fs::remove_file(path).unwrap();
    let again = join(&first, &f.dir.path().join("missing-invitation")).await;
    assert_eq!(again.runtime_id, a.runtime_id);
    assert_eq!(again.certificate_pem, a.certificate_pem);
    assert_eq!(std::fs::read(first.join("fleet-worker.key")).unwrap(), key);
    authority::revoke_key(&f.db(), &invitation.key_id).unwrap();
    force_renewal(&f, &first);
    worker::renew_if_due(&first).await.unwrap();
    let renewed = state(&first);
    assert_eq!(renewed.runtime_id, a.runtime_id);
    assert_ne!(renewed.certificate_pem, a.certificate_pem);
    assert_eq!(std::fs::read(first.join("fleet-worker.key")).unwrap(), key);
    let config = NetworkConfig::load(Some(&first.join("managed-network.toml"))).unwrap();
    assert_eq!(
        std::fs::read_to_string(config.identity_cert).unwrap(),
        renewed.certificate_pem
    );
    assert!(config.enrollment_token.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enrollment_credentials_cannot_renew_or_call_runtime_apis() {
    let f = Fixture::new().await;
    let invitation = f.invitation();
    let channel = f.channel(true, None).await;
    let mut enrollment = EnrollmentClient::new(channel.clone());
    let error = enrollment
        .renew(RenewalRequest {
            csr_pem: "invalid".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Unauthenticated);
    let error = enrollment
        .register(EnrollmentRequest {
            key_id: invitation.key_id.clone(),
            token: "invalid".into(),
            csr_pem: "invalid".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::PermissionDenied);
    let mut request = tonic::Request::new(CallRequest {
        method: "runtime_list".into(),
        json: "{}".into(),
    });
    request.metadata_mut().insert(
        "authorization",
        format!("Bearer {}", invitation.token).parse().unwrap(),
    );
    let error = FederationClient::new(channel)
        .call(request)
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Unimplemented);
    // TLS 1.3 may report a missing client certificate only on the first RPC.
    let tls = ClientTlsConfig::new()
        .ca_certificate(TlsCertificate::from_pem(invitation.ca_pem))
        .domain_name(invitation.tls_name);
    let connection = Endpoint::from_shared(format!("https://{}", f.server.controller_address))
        .unwrap()
        .tls_config(tls)
        .unwrap()
        .timeout(Duration::from_secs(3))
        .connect()
        .await;
    if let Ok(channel) = connection {
        let mut request = tonic::Request::new(CallRequest {
            method: "runtime_list".into(),
            json: "{}".into(),
        });
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", invitation.token).parse().unwrap(),
        );
        assert!(FederationClient::new(channel).call(request).await.is_err());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enrolled_worker_has_reverse_control_presence_and_revocation_closes_connection() {
    let f = Fixture::new().await;
    let invitation = f.invitation();
    let path = f.write_invitation(&invitation);
    let root = f.dir.path().join("worker");
    let certificate = join(&root, &path).await;
    let mut client = FederationClient::new(f.channel(false, Some(identity(&root))).await);
    let (send, receive) = mpsc::channel(4);
    send.send(CallRequest {
        method: "heartbeat".into(),
        json: "{}".into(),
    })
    .await
    .unwrap();
    let mut stream = client
        .control(tokio_stream::wrappers::ReceiverStream::new(receive))
        .await
        .unwrap()
        .into_inner();
    send.send(CallRequest {
        method: "heartbeat".into(),
        json: json!({"status":{"version":"fixture","drained":false}}).to_string(),
    })
    .await
    .unwrap();
    wait_until(|| {
        horde::fleet::dispatch(&f.db(), "runtime_list", &json!({}))
            .unwrap()
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == certificate.runtime_id && v["state"] == "ready")
    })
    .await;
    let config = f.network.clone();
    let runtime = certificate.runtime_id.clone();
    let call =
        tokio::spawn(
            async move { horde::control::call(&config, &runtime, "status", &json!({})).await },
        );
    let request = tokio::time::timeout(Duration::from_secs(3), stream.message())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let packet: Value = serde_json::from_str(&request.json).unwrap();
    assert_eq!(packet["method"], "status");
    send.send(CallRequest {
        method: "reply".into(),
        json: json!({"id":packet["id"],"result":{"healthy":true}}).to_string(),
    })
    .await
    .unwrap();
    assert_eq!(call.await.unwrap().unwrap().unwrap()["healthy"], true);
    authority::revoke_worker(&f.db(), &certificate.runtime_id).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(8), stream.message())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    force_renewal(&f, &root);
    assert!(worker::renew_if_due(&root).await.is_err());
    let denied = client
        .call(CallRequest {
            method: "status".into(),
            json: "{}".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(denied.code(), tonic::Code::PermissionDenied);
    let runtimes = horde::fleet::dispatch(&f.db(), "runtime_list", &json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(runtimes[0]["state"], "revoked");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_enrolls_from_injected_file_and_restarts_without_secret() {
    let f = Fixture::new().await;
    let invitation = f.invitation();
    let path = f.write_invitation(&invitation);
    let root = f.dir.path().join("automatic-worker");
    let mut child = command(&root)
        .arg("daemon")
        .env("HORDE_ENROLLMENT_FILE", &path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until(|| root.join("daemon.sock").exists()).await;
    let original = state(&root);
    child.kill().await.unwrap();
    child.wait().await.unwrap();
    std::fs::remove_file(path).unwrap();
    std::fs::remove_file(root.join("daemon.sock")).unwrap();
    let mut restarted = command(&root)
        .arg("daemon")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until(|| root.join("daemon.sock").exists()).await;
    assert_eq!(state(&root).runtime_id, original.runtime_id);
    assert_eq!(state(&root).certificate_pem, original.certificate_pem);
    let count: i64 = f
        .db()
        .conn
        .query_row("SELECT COUNT(*) FROM fleet_enrollment_members", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    restarted.kill().await.unwrap();
    restarted.wait().await.unwrap();
}

fn create_arguments(f: &Fixture, output: &Path) -> Vec<String> {
    [
        "network".into(),
        "key".into(),
        "create".into(),
        "portable-fleet".into(),
        "--listen".into(),
        f.server.listen.to_string(),
        "--enrollment-address".into(),
        "192.0.2.10:8444".into(),
        "--controller-address".into(),
        f.server.controller_address.to_string(),
        "--tls-name".into(),
        f.server.tls_name.clone(),
        "--output".into(),
        output.to_str().unwrap().into(),
        "--max-workers".into(),
        "3".into(),
    ]
    .into()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_creates_private_key_lists_and_revokes_without_exposing_secret() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new().await;
    horde::secrets::write_private(
        &f.dir.path().join("managed-network.toml"),
        toml::to_string(&f.network).unwrap().as_bytes(),
    )
    .unwrap();
    let path = f.dir.path().join("fleet-secret.json");
    let args = create_arguments(&f, &path);
    let created = command(f.dir.path()).args(&args).output().await.unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let raw = std::fs::read(&path).unwrap();
    let invitation: Invitation = serde_json::from_slice(&raw).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
        0
    );
    assert_eq!(invitation.endpoint, "192.0.2.10:8444".parse().unwrap());
    let persisted: ServerConfig = toml::from_str(
        &std::fs::read_to_string(f.dir.path().join("enrollment-server.toml")).unwrap(),
    )
    .unwrap();
    assert_eq!(persisted.listen, f.server.listen);
    assert!(!String::from_utf8_lossy(&created.stdout).contains(&invitation.token));
    let duplicate = command(f.dir.path()).args(&args).output().await.unwrap();
    assert!(!duplicate.status.success());
    assert_eq!(std::fs::read(&path).unwrap(), raw);
    let listed = command(f.dir.path())
        .args(["network", "key", "list"])
        .output()
        .await
        .unwrap();
    assert!(listed.status.success());
    assert!(!String::from_utf8_lossy(&listed.stdout).contains(&invitation.token));
    let list: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], invitation.key_id);
    let revoked = command(f.dir.path())
        .args(["network", "key", "revoke", &invitation.key_id])
        .output()
        .await
        .unwrap();
    assert!(revoked.status.success());
    assert!(!String::from_utf8_lossy(&revoked.stdout).contains(&invitation.token));
    assert_eq!(
        authority::list_keys(&f.db()).unwrap()[0]["state"],
        "revoked"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_task_credentials_cannot_use_enrollment_administration_commands() {
    let f = Fixture::new().await;
    let invitation = f.invitation();
    let path = f.write_invitation(&invitation);
    let output = f.dir.path().join("forbidden-secret.json");
    let cases = vec![
        create_arguments(&f, &output),
        vec!["network".into(), "key".into(), "list".into()],
        vec![
            "network".into(),
            "key".into(),
            "revoke".into(),
            invitation.key_id.clone(),
        ],
        vec![
            "network".into(),
            "join".into(),
            "--invitation".into(),
            path.to_str().unwrap().into(),
        ],
        vec!["network".into(), "revoke".into(), "worker-fixture".into()],
    ];
    for args in cases {
        let result = command(f.dir.path())
            .args(&args)
            .env("HORDE_WORKER_TOKEN", "task-token")
            .output()
            .await
            .unwrap();
        assert!(!result.status.success(), "worker authorized for {args:?}");
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(
            error.contains("administrative access"),
            "unexpected rejection for {args:?}: {error}"
        );
    }
    assert!(!output.exists());
    assert!(!f.dir.path().join("fleet-worker.json").exists());
    assert_eq!(authority::list_keys(&f.db()).unwrap()[0]["state"], "active");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worker_rejects_controller_signed_by_another_ca_before_enrollment() {
    let f = Fixture::new().await;
    let original = f.invitation();
    let mut parameters = CertificateParams::default();
    parameters.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    parameters.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let wrong_ca = parameters
        .self_signed(&KeyPair::generate().unwrap())
        .unwrap();
    let invitation = Invitation {
        ca_pem: wrong_ca.pem(),
        ..original
    };
    let path = f.write_invitation(&invitation);
    let root = f.dir.path().join("untrusted-controller-worker");
    let output = command(&root)
        .args(["network", "join", "--invitation"])
        .arg(path)
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(!root.join("fleet-worker.json").exists());
    assert!(!String::from_utf8_lossy(&output.stderr).contains(&invitation.token));
    let count: i64 = f
        .db()
        .conn
        .query_row("SELECT COUNT(*) FROM fleet_enrollment_members", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_enrolls_from_injected_json_without_persisting_fleet_secret() {
    let f = Fixture::new().await;
    let invitation = f.invitation();
    let root = f.dir.path().join("json-worker");
    let mut child = command(&root)
        .arg("daemon")
        .env(
            "HORDE_ENROLLMENT_JSON",
            serde_json::to_string(&invitation).unwrap(),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until(|| root.join("daemon.sock").exists()).await;
    assert_eq!(state(&root).concurrency, 2);
    assert!(
        !std::fs::read_to_string(root.join("fleet-worker.json"))
            .unwrap()
            .contains(&invitation.token)
    );
    child.kill().await.unwrap();
    child.wait().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_session_certificate_closes_control_even_when_renewed_worker_remains_active() {
    let f = Fixture::new().await;
    let path = f.write_invitation(&f.invitation());
    let root = f.dir.path().join("renewing-worker");
    let original = join(&root, &path).await;
    let fingerprint = horde::store::hash(pem::parse(&original.certificate_pem).unwrap().contents());
    let mut client = FederationClient::new(f.channel(false, Some(identity(&root))).await);
    let (send, receive) = mpsc::channel(4);
    send.send(CallRequest {
        method: "heartbeat".into(),
        json: "{}".into(),
    })
    .await
    .unwrap();
    let mut old_stream = client
        .control(tokio_stream::wrappers::ReceiverStream::new(receive))
        .await
        .unwrap()
        .into_inner();
    force_renewal(&f, &root);
    worker::renew_if_due(&root).await.unwrap();
    assert_ne!(state(&root).certificate_pem, original.certificate_pem);
    f.db()
        .conn
        .execute(
            "UPDATE fleet_enrollment_certificates SET expires=? WHERE fingerprint=?",
            rusqlite::params![now() - 1, fingerprint],
        )
        .unwrap();
    assert!(authority::is_active(&f.db(), &original.runtime_id).unwrap());
    assert!(
        authority::identity(&f.db(), &fingerprint)
            .unwrap()
            .is_none()
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(8), old_stream.message())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    let mut renewed = FederationClient::new(f.channel(false, Some(identity(&root))).await);
    let (send_new, receive_new) = mpsc::channel(4);
    send_new
        .send(CallRequest {
            method: "heartbeat".into(),
            json: "{}".into(),
        })
        .await
        .unwrap();
    assert!(
        renewed
            .control(tokio_stream::wrappers::ReceiverStream::new(receive_new))
            .await
            .is_ok()
    );
}
