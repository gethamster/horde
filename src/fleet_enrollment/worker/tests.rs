use super::*;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
};

fn fixture() -> (Invitation, Certificate, KeyPair) {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(vec!["controller.test".into()]).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let key = KeyPair::generate().unwrap();
    let issuer = Issuer::from_ca_cert_pem(&ca.pem(), ca_key).unwrap();
    let mut params = CertificateParams::new(Vec::new()).unwrap();
    params
        .distinguished_name
        .push(DnType::CommonName, "worker-test");
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    params.not_before = time::OffsetDateTime::now_utc() - time::Duration::minutes(1);
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::hours(24);
    let expires = params.not_after.unix_timestamp();
    let cert = params.signed_by(&key, &issuer).unwrap();
    (
        Invitation {
            version: 1,
            key_id: "fleet-test".into(),
            token: "top-secret".into(),
            endpoint: "127.0.0.1:7444".parse().unwrap(),
            tls_name: "controller.test".into(),
            ca_pem: ca.pem(),
            controller_id: "controller-test".into(),
            controller_address: "127.0.0.1:7443".parse().unwrap(),
            controller_fingerprint: "a".repeat(64),
        },
        Certificate {
            runtime_id: "worker-test".into(),
            certificate_pem: cert.pem(),
            expires,
            renew_after: expires - 3600,
            concurrency: 2,
        },
        key,
    )
}

#[test]
fn rejects_certificate_for_another_private_key() {
    let (invitation, certificate, _) = fixture();
    assert!(
        validate_certificate(
            &invitation,
            &certificate,
            &KeyPair::generate().unwrap(),
            true
        )
        .is_err()
    );
}

#[test]
fn validates_identity_times_and_issuer() {
    let (invitation, certificate, key) = fixture();
    validate_certificate(&invitation, &certificate, &key, true).unwrap();
    let wrong_id = Certificate {
        runtime_id: "another-worker".into(),
        ..certificate.clone()
    };
    assert!(validate_certificate(&invitation, &wrong_id, &key, true).is_err());
    let wrong_expiry = Certificate {
        expires: certificate.expires + 1,
        ..certificate.clone()
    };
    assert!(validate_certificate(&invitation, &wrong_expiry, &key, true).is_err());
    let (other_issuer, _, _) = fixture();
    assert!(validate_certificate(&other_issuer, &certificate, &key, true).is_err());
}

#[test]
fn installs_from_durable_state_without_saving_enrollment_secret() {
    let root = tempfile::tempdir().unwrap();
    let (invitation, certificate, key) = fixture();
    crate::secrets::write_private(&root.path().join(KEY_FILE), key.serialize_pem().as_bytes())
        .unwrap();
    let state = WorkerState {
        invitation: sanitized(&invitation),
        certificate,
        credential_file: None,
    };
    persist(root.path(), &state).unwrap();
    let saved = std::fs::read_to_string(root.path().join(STATE_FILE)).unwrap();
    assert!(!saved.contains("top-secret"));
    // Simulate process death after the state commit but before config installation.
    install(root.path(), &state).unwrap();
    let network = NetworkConfig::load(Some(&root.path().join("managed-network.toml"))).unwrap();
    assert_eq!(network.runtime_id, "worker-test");
    assert_eq!(network.controller_peer.as_deref(), Some("controller-test"));
    assert_eq!(network.allowed_clients.len(), 1);
    assert_eq!(network.execution_clients, vec!["controller-test"]);
    assert!(network.enrollment_token.is_none());
    assert!(network.delegate_peers.is_empty());
    let db = crate::store::Store::open(root.path()).unwrap();
    crate::management::set(&db, "concurrency", "1").unwrap();
    install(root.path(), &state).unwrap();
    assert_eq!(
        crate::management::value(&db, "concurrency")
            .unwrap()
            .as_deref(),
        Some("1")
    );
    let recovered = load(root.path()).unwrap().unwrap();
    assert_eq!(
        recovered.certificate.runtime_id,
        state.certificate.runtime_id
    );
    assert!(recovered.invitation.token.is_empty());
}

#[test]
fn refuses_to_overwrite_an_existing_runtime() {
    let root = tempfile::tempdir().unwrap();
    let existing = NetworkConfig {
        runtime_id: "valuable-existing-runtime".into(),
        ..Default::default()
    };
    crate::federation::configure(root.path(), &existing).unwrap();
    let (invitation, certificate, _) = fixture();
    assert!(prepare(root.path(), &invitation).is_err());
    assert!(
        install(
            root.path(),
            &WorkerState {
                invitation,
                certificate,
                credential_file: None,
            }
        )
        .is_err()
    );
    assert_eq!(
        NetworkConfig::load(Some(&root.path().join("network-runtime.toml")))
            .unwrap()
            .runtime_id,
        "valuable-existing-runtime"
    );
}

#[test]
fn retries_keep_the_same_local_key_and_csr() {
    let root = tempfile::tempdir().unwrap();
    let (invitation, _, _) = fixture();
    let first = prepare(root.path(), &invitation).unwrap();
    let second = prepare(root.path(), &invitation).unwrap();
    assert_eq!(
        first.subject_public_key_info(),
        second.subject_public_key_info()
    );
    assert_eq!(
        csr(root.path(), &first).unwrap(),
        csr(root.path(), &second).unwrap()
    );
    let changed = Invitation {
        controller_id: "another-controller".into(),
        ..invitation
    };
    assert!(prepare(root.path(), &changed).is_err());
}
#[test]
fn projected_invitation_symlinks_are_supported_but_state_symlinks_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("projected");
    crate::secrets::write_private(&target, b"secret").unwrap();
    let link = root.path().join("invitation");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert_eq!(invitation_read(&link).unwrap(), "secret");
    assert!(private_read(&link).is_err());
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(invitation_read(&link).is_err());
}

#[test]
fn enrollment_refuses_a_running_daemon_or_user_network_authority() {
    let root = tempfile::tempdir().unwrap();
    let _held = daemon_lock(root.path()).unwrap();
    assert!(daemon_lock(root.path()).is_err());
    assert!(
        ensure_unused_user_network(&NetworkConfig {
            provider: Provider::Direct,
            ..Default::default()
        })
        .is_err()
    );
    ensure_unused_user_network(&NetworkConfig::default()).unwrap();
}
#[test]
fn interrupted_enrollment_cannot_start_without_its_invitation() {
    let root = tempfile::tempdir().unwrap();
    ensure_no_pending_enrollment(root.path()).unwrap();
    let (invitation, _, _) = fixture();
    prepare(root.path(), &invitation).unwrap();
    assert!(ensure_no_pending_enrollment(root.path()).is_err());
}

#[test]
fn reassertion_keeps_the_original_controller_and_fleet_binding() {
    let (invitation, certificate, _) = fixture();
    let state = WorkerState {
        invitation: sanitized(&invitation),
        certificate,
        credential_file: None,
    };
    validate_reassertion(&state, &invitation).unwrap();
    let other_fleet = Invitation {
        key_id: "different-fleet".into(),
        ..invitation.clone()
    };
    assert!(validate_reassertion(&state, &other_fleet).is_err());
    let other_controller = Invitation {
        endpoint: "127.0.0.1:9999".parse().unwrap(),
        ..invitation.clone()
    };
    assert!(validate_reassertion(&state, &other_controller).is_err());
    assert!(validate_reassertion(&state, &sanitized(&invitation)).is_err());
}

#[test]
fn credential_reference_preserves_mount_path_across_secret_rotation() {
    let root = tempfile::tempdir().unwrap();
    let (invitation, _, _) = fixture();
    let first = root.path().join("first.json");
    let second = root.path().join("second.json");
    for path in [&first, &second] {
        crate::secrets::write_private(path, &serde_json::to_vec(&invitation).unwrap()).unwrap();
    }
    let mount = root.path().join("mount.json");
    std::os::unix::fs::symlink(&first, &mount).unwrap();
    let source = Credential::File(mount.clone());
    assert_eq!(source.file_reference().unwrap(), Some(mount.clone()));
    std::fs::remove_file(&mount).unwrap();
    std::os::unix::fs::symlink(&second, &mount).unwrap();
    assert_eq!(source.read().unwrap().key_id, invitation.key_id);
}
