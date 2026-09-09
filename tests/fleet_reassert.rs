use horde::{
    fleet_enrollment::{Certificate, Invitation, ServerConfig, authority, service},
    network::{NetworkConfig, Provider},
    store::{Store, hash, now},
};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::Duration,
};
use tokio::{net::TcpListener, task::JoinHandle};

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
            1,
            2,
        )
        .unwrap()
    }

    fn write_invitation(&self, invitation: &Invitation) -> std::path::PathBuf {
        let path = self.dir.path().join(format!("{}.json", invitation.key_id));
        horde::secrets::write_private(&path, &serde_json::to_vec(invitation).unwrap()).unwrap();
        path
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

fn state(root: &Path) -> Certificate {
    let value: Value =
        serde_json::from_slice(&std::fs::read(root.join("fleet-worker.json")).unwrap()).unwrap();
    serde_json::from_value(value["certificate"].clone()).unwrap()
}

async fn join(root: &Path, invitation: &Path) -> Output {
    command(root)
        .args(["network", "join", "--invitation"])
        .arg(invitation)
        .output()
        .await
        .unwrap()
}

async fn enrolled(f: &Fixture) -> (PathBuf, Invitation, PathBuf, Certificate) {
    let invitation = f.invitation();
    let path = f.write_invitation(&invitation);
    let root = f.dir.path().join("worker");
    let output = join(&root, &path).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let certificate = state(&root);
    (root, invitation, path, certificate)
}

fn expire(f: &Fixture, root: &Path) -> Certificate {
    let previous = state(root);
    let timestamp = now();
    let issuer = Issuer::from_ca_cert_pem(
        &std::fs::read_to_string(&f.network.ca_cert).unwrap(),
        KeyPair::from_pem(&std::fs::read_to_string(&f.server.issuer_key).unwrap()).unwrap(),
    )
    .unwrap();
    let key = KeyPair::from_pem(&std::fs::read_to_string(root.join("fleet-worker.key")).unwrap())
        .unwrap();
    let mut parameters = CertificateParams::default();
    parameters.is_ca = IsCa::ExplicitNoCa;
    parameters
        .distinguished_name
        .push(DnType::CommonName, &previous.runtime_id);
    parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    parameters.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    parameters.not_before = time::OffsetDateTime::from_unix_timestamp(timestamp - 7200).unwrap();
    parameters.not_after = time::OffsetDateTime::from_unix_timestamp(timestamp - 3600).unwrap();
    let signed = parameters.signed_by(&key, &issuer).unwrap();
    let expired = Certificate {
        certificate_pem: signed.pem(),
        expires: timestamp - 3600,
        renew_after: timestamp - 5400,
        ..previous
    };
    let fingerprint = hash(signed.der());
    let db = f.db();
    db.conn
        .execute(
            "UPDATE fleet_enrollment_certificates SET expires=? WHERE runtime=?",
            rusqlite::params![expired.expires, expired.runtime_id],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO fleet_enrollment_certificates VALUES(?,?,?,?,?,?)",
            rusqlite::params![
                fingerprint,
                expired.runtime_id,
                expired.certificate_pem,
                expired.expires,
                expired.renew_after,
                expired.concurrency
            ],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE fleet_enrollment_members SET current_fingerprint=? WHERE runtime=?",
            rusqlite::params![fingerprint, expired.runtime_id],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE runtime_enrollments SET fingerprint=?, expires=? WHERE runtime=?",
            rusqlite::params![fingerprint, expired.expires, expired.runtime_id],
        )
        .unwrap();
    let state_file = root.join("fleet-worker.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&state_file).unwrap()).unwrap();
    value["certificate"] = serde_json::to_value(&expired).unwrap();
    std::fs::write(state_file, serde_json::to_vec(&value).unwrap()).unwrap();
    let cert_path = root.join(format!(
        "fleet-worker-{}.pem",
        hash(expired.certificate_pem.as_bytes())
    ));
    horde::secrets::write_private(&cert_path, expired.certificate_pem.as_bytes()).unwrap();
    for file in ["managed-network.toml", "network-runtime.toml"] {
        let current = NetworkConfig::load(Some(&root.join(file))).unwrap();
        let updated = NetworkConfig {
            identity_cert: cert_path.clone(),
            ..current
        };
        std::fs::write(root.join(file), toml::to_string(&updated).unwrap()).unwrap();
    }
    expired
}

fn assert_recovered(f: &Fixture, root: &Path, original: &Certificate, private_key: &[u8]) {
    let recovered = state(root);
    assert_eq!(recovered.runtime_id, original.runtime_id);
    assert!(
        recovered.expires > now(),
        "expired certificate was not reasserted"
    );
    assert_eq!(
        std::fs::read(root.join("fleet-worker.key")).unwrap(),
        private_key
    );
    assert!(authority::is_active(&f.db(), &original.runtime_id).unwrap());
    let members: i64 = f
        .db()
        .conn
        .query_row("SELECT COUNT(*) FROM fleet_enrollment_members", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(members, 1, "reassertion consumed an admission slot");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_join_reasserts_expired_certificate_without_changing_identity_or_quota() {
    let f = Fixture::new().await;
    let (root, _, invitation, original) = enrolled(&f).await;
    let private_key = std::fs::read(root.join("fleet-worker.key")).unwrap();
    expire(&f, &root);
    let expired_state = std::fs::read(root.join("fleet-worker.json")).unwrap();
    let output = join(&root, &invitation).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_recovered(&f, &root, &original, &private_key);
    let recovered = state(&root).certificate_pem;
    // Retry after losing the local response commit; the controller must reuse its certificate.
    std::fs::write(root.join("fleet-worker.json"), expired_state).unwrap();
    assert!(join(&root, &invitation).await.status.success());
    assert_eq!(
        state(&root).certificate_pem,
        recovered,
        "retry needlessly issued another certificate"
    );
}

async fn expired_certificate_cannot_renew(f: &Fixture, root: &Path) {
    use horde::federation::wire::{RenewalRequest, enrollment_client::EnrollmentClient};
    use tonic::transport::{Certificate as TlsCertificate, ClientTlsConfig, Endpoint, Identity};
    let tls = ClientTlsConfig::new()
        .ca_certificate(TlsCertificate::from_pem(
            std::fs::read(&f.network.ca_cert).unwrap(),
        ))
        .domain_name(&f.server.tls_name)
        .identity(Identity::from_pem(
            state(root).certificate_pem,
            std::fs::read(root.join("fleet-worker.key")).unwrap(),
        ));
    let connection = Endpoint::from_shared(format!("https://{}", f.server.listen))
        .unwrap()
        .tls_config(tls)
        .unwrap()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(3))
        .connect()
        .await;
    if let Ok(channel) = connection {
        assert!(
            EnrollmentClient::new(channel)
                .renew(RenewalRequest {
                    csr_pem: std::fs::read_to_string(root.join("fleet-worker.csr")).unwrap(),
                })
                .await
                .is_err(),
            "expired client certificate unexpectedly authenticated"
        );
    }
}

async fn recovered_daemon(mut command: tokio::process::Command, root: &Path) {
    use tokio::io::AsyncReadExt;
    let mut child = command
        .arg("daemon")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let mut error = String::new();
            child
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut error)
                .await
                .unwrap();
            panic!("worker daemon failed during reassertion ({status}): {error}");
        }
        if root.join("daemon.sock").exists() && state(root).expires > now() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "daemon did not recover its expired certificate"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    child.kill().await.unwrap();
    child.wait().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_reasserts_with_injected_file_json_or_retained_credential_path() {
    for source in ["file", "json", "retained"] {
        let f = Fixture::new().await;
        let (root, invitation, path, original) = enrolled(&f).await;
        let private_key = std::fs::read(root.join("fleet-worker.key")).unwrap();
        expire(&f, &root);
        expired_certificate_cannot_renew(&f, &root).await;
        let mut startup = command(&root);
        match source {
            "file" => {
                startup.env("HORDE_ENROLLMENT_FILE", &path);
            }
            "json" => {
                std::fs::remove_file(&path).unwrap();
                startup.env(
                    "HORDE_ENROLLMENT_JSON",
                    serde_json::to_string(&invitation).unwrap(),
                );
            }
            _ => {}
        }
        recovered_daemon(startup, &root).await;
        assert_recovered(&f, &root, &original, &private_key);
        assert!(
            !std::fs::read_to_string(root.join("fleet-worker.json"))
                .unwrap()
                .contains(&invitation.token)
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoked_or_expired_admission_and_revoked_worker_cannot_reassert() {
    for denial in ["expired-key", "revoked-key", "revoked-worker"] {
        let f = Fixture::new().await;
        let (root, invitation, path, original) = enrolled(&f).await;
        expire(&f, &root);
        match denial {
            "expired-key" => {
                f.db()
                    .conn
                    .execute(
                        "UPDATE fleet_enrollment_keys SET expires=? WHERE id=?",
                        rusqlite::params![now() - 1, invitation.key_id],
                    )
                    .unwrap();
            }
            "revoked-key" => authority::revoke_key(&f.db(), &invitation.key_id).unwrap(),
            _ => authority::revoke_worker(&f.db(), &original.runtime_id).unwrap(),
        }
        let previous_state = std::fs::read(root.join("fleet-worker.json")).unwrap();
        let private_key = std::fs::read(root.join("fleet-worker.key")).unwrap();
        let result = join(&root, &path).await;
        assert!(!result.status.success(), "reassertion accepted {denial}");
        assert_eq!(
            std::fs::read(root.join("fleet-worker.json")).unwrap(),
            previous_state
        );
        assert_eq!(
            std::fs::read(root.join("fleet-worker.key")).unwrap(),
            private_key
        );
        assert!(!String::from_utf8_lossy(&result.stderr).contains(&invitation.token));
        assert!(!authority::is_active(&f.db(), &original.runtime_id).unwrap());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_reports_missing_reassertion_credential_without_replacing_worker_identity() {
    let f = Fixture::new().await;
    let (root, _, path, _) = enrolled(&f).await;
    expire(&f, &root);
    std::fs::remove_file(path).unwrap();
    let previous = std::fs::read(root.join("fleet-worker.json")).unwrap();
    let private_key = std::fs::read(root.join("fleet-worker.key")).unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        command(&root).arg("daemon").output(),
    )
    .await
    .expect("daemon did not reject expired identity with missing credential")
    .unwrap();
    assert!(!result.status.success());
    let error = String::from_utf8_lossy(&result.stderr).to_lowercase();
    assert!(
        error.contains("expired") && (error.contains("credential") || error.contains("enrollment")),
        "missing credential error is not actionable: {error}"
    );
    assert_eq!(
        std::fs::read(root.join("fleet-worker.json")).unwrap(),
        previous
    );
    assert_eq!(
        std::fs::read(root.join("fleet-worker.key")).unwrap(),
        private_key
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn renewal_timer_reasserts_expired_worker_using_retained_private_credential_file() {
    let f = Fixture::new().await;
    let (root, invitation, _, original) = enrolled(&f).await;
    let private_key = std::fs::read(root.join("fleet-worker.key")).unwrap();
    expire(&f, &root);
    horde::fleet_enrollment::worker::renew_if_due(&root)
        .await
        .unwrap();
    assert_recovered(&f, &root, &original, &private_key);
    assert!(
        !std::fs::read_to_string(root.join("fleet-worker.json"))
            .unwrap()
            .contains(&invitation.token)
    );
}
