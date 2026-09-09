use horde::{
    fleet_enrollment::{Certificate, Invitation, ServerConfig, authority::*},
    network::{NetworkConfig, Provider},
    store::{Store, hash, now},
};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};

struct Fixture {
    _dir: tempfile::TempDir,
    db: Store,
    network: NetworkConfig,
    server: ServerConfig,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = Store::open(dir.path()).unwrap();
        let key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::default();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca = ca_params.self_signed(&key).unwrap();
        let issuer = Issuer::new(ca_params, key);
        let controller = KeyPair::generate().unwrap();
        let params = CertificateParams::new(vec!["controller.test".to_owned()]).unwrap();
        let cert = params.signed_by(&controller, &issuer).unwrap();
        let network = NetworkConfig {
            provider: Provider::Direct,
            runtime_id: "controller".into(),
            ca_cert: dir.path().join("ca.pem"),
            identity_cert: dir.path().join("controller.pem"),
            identity_key: dir.path().join("controller.key"),
            ..Default::default()
        };
        let server = ServerConfig {
            listen: "127.0.0.1:7444".parse().unwrap(),
            issuer_key: dir.path().join("ca.key"),
            controller_address: "127.0.0.1:7443".parse().unwrap(),
            tls_name: "controller.test".into(),
        };
        horde::secrets::write_private(&network.ca_cert, ca.pem().as_bytes()).unwrap();
        horde::secrets::write_private(&network.identity_cert, cert.pem().as_bytes()).unwrap();
        horde::secrets::write_private(&network.identity_key, controller.serialize_pem().as_bytes())
            .unwrap();
        horde::secrets::write_private(&server.issuer_key, issuer.key().serialize_pem().as_bytes())
            .unwrap();
        Self {
            _dir: dir,
            db,
            network,
            server,
        }
    }
    fn invite(&self, cap: usize) -> Invitation {
        create_key(
            &self.db,
            &self.network,
            &self.server,
            "sandbox",
            3600,
            cap,
            2,
        )
        .unwrap()
    }
    fn register(&self, invitation: &Invitation, csr: &str) -> anyhow::Result<Certificate> {
        register(
            &self.db,
            &self.network,
            &self.server,
            &invitation.key_id,
            &invitation.token,
            csr,
        )
    }
}
fn csr() -> String {
    CertificateParams::default()
        .serialize_request(&KeyPair::generate().unwrap())
        .unwrap()
        .pem()
        .unwrap()
}
fn fingerprint(cert: &Certificate) -> String {
    hash(pem::parse(&cert.certificate_pem).unwrap().contents())
}

#[test]
fn fleet_authority_admission_retry_and_total_quota() {
    let f = Fixture::new();
    let invite = f.invite(1);
    let request = csr();
    let cert = f.register(&invite, &request).unwrap();
    assert_eq!(
        f.register(&invite, &request).unwrap().certificate_pem,
        cert.certificate_pem
    );
    assert!(f.register(&invite, &csr()).is_err());
    assert_eq!(
        identity(&f.db, &fingerprint(&cert)).unwrap(),
        Some(cert.runtime_id.clone())
    );
    assert!(is_active(&f.db, &cert.runtime_id).unwrap());
    let stored: String =
        f.db.conn
            .query_row("SELECT token_hash FROM fleet_enrollment_keys", [], |r| {
                r.get(0)
            })
            .unwrap();
    assert_eq!(stored, hash(invite.token.as_bytes()));
    assert_ne!(stored, invite.token);
    assert!(
        !list_keys(&f.db)
            .unwrap()
            .to_string()
            .contains(&invite.token)
    );
    let owned: i64 =
        f.db.conn
            .query_row("SELECT COUNT(*) FROM managed_runtimes", [], |r| r.get(0))
            .unwrap();
    assert_eq!(owned, 0);
    revoke_worker(&f.db, &cert.runtime_id).unwrap();
    assert!(!is_active(&f.db, &cert.runtime_id).unwrap());
    assert!(identity(&f.db, &fingerprint(&cert)).unwrap().is_none());
    assert!(f.register(&invite, &request).is_err());
    assert!(f.register(&invite, &csr()).is_err());
    assert!(renew(&f.db, &f.network, &f.server, &fingerprint(&cert), &request).is_err());
}
#[test]
fn fleet_authority_key_revocation_does_not_revoke_members() {
    let f = Fixture::new();
    let invite = f.invite(3);
    let request = csr();
    let cert = f.register(&invite, &request).unwrap();
    revoke_key(&f.db, &invite.key_id).unwrap();
    assert!(f.register(&invite, &csr()).is_err());
    assert_eq!(
        renew(&f.db, &f.network, &f.server, &fingerprint(&cert), &request)
            .unwrap()
            .certificate_pem,
        cert.certificate_pem
    );
    let second = f.invite(3);
    assert!(f.register(&second, &request).is_err());
}
#[test]
fn fleet_authority_renewal_overlap_and_expiry() {
    let f = Fixture::new();
    let invite = f.invite(1);
    let request = csr();
    let first = f.register(&invite, &request).unwrap();
    let old = fingerprint(&first);
    f.db.conn
        .execute(
            "UPDATE fleet_enrollment_certificates SET renew_after=?",
            [now() - 1],
        )
        .unwrap();
    let next = renew(&f.db, &f.network, &f.server, &old, &request).unwrap();
    assert_ne!(next.certificate_pem, first.certificate_pem);
    assert_eq!(
        renew(&f.db, &f.network, &f.server, &old, &request)
            .unwrap()
            .certificate_pem,
        next.certificate_pem
    );
    assert!(identity(&f.db, &old).unwrap().is_some());
    f.db.conn
        .execute(
            "UPDATE fleet_enrollment_certificates SET expires=? WHERE fingerprint=?",
            rusqlite::params![now() - 1, old],
        )
        .unwrap();
    assert!(identity(&f.db, &old).unwrap().is_none());
    assert!(renew(&f.db, &f.network, &f.server, &old, &request).is_err());
    assert!(is_active(&f.db, &first.runtime_id).unwrap());
    f.db.conn
        .execute(
            "UPDATE fleet_enrollment_certificates SET expires=?",
            [now() - 1],
        )
        .unwrap();
    assert!(!is_active(&f.db, &first.runtime_id).unwrap());
}
#[test]
fn fleet_authority_discards_requested_authority() {
    let f = Fixture::new();
    let invite = f.invite(1);
    let mut parameters = CertificateParams::new(vec!["controller.test".into()]).unwrap();
    parameters.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    parameters.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let request = parameters
        .serialize_request(&KeyPair::generate().unwrap())
        .unwrap()
        .pem()
        .unwrap();
    let cert = f.register(&invite, &request).unwrap();
    use x509_parser::prelude::{FromDer, X509Certificate};
    let der = pem::parse(&cert.certificate_pem).unwrap();
    let (_, parsed) = X509Certificate::from_der(der.contents()).unwrap();
    assert!(!parsed.basic_constraints().unwrap().unwrap().value.ca);
    assert!(parsed.subject_alternative_name().unwrap().is_none());
    let eku = parsed.extended_key_usage().unwrap().unwrap();
    assert!(eku.value.client_auth);
    assert!(!eku.value.server_auth && !eku.value.any);
    let ku = parsed.key_usage().unwrap().unwrap();
    assert!(ku.value.digital_signature());
    assert!(!ku.value.key_cert_sign());
    assert_eq!(
        parsed
            .subject()
            .iter_common_name()
            .next()
            .unwrap()
            .as_str()
            .unwrap(),
        cert.runtime_id
    );
    assert_eq!(parsed.validity().not_after.timestamp(), cert.expires);
}
#[test]
fn fleet_authority_rejects_bad_credentials_and_signers() {
    let f = Fixture::new();
    let invite = f.invite(1);
    assert!(
        register(
            &f.db,
            &f.network,
            &f.server,
            &invite.key_id,
            "wrong",
            &csr()
        )
        .is_err()
    );
    assert!(f.register(&invite, "not a signed CSR").is_err());
    let cert = f.register(&invite, &csr()).unwrap();
    assert!(renew(&f.db, &f.network, &f.server, &fingerprint(&cert), &csr()).is_err());
    std::fs::write(
        &f.server.issuer_key,
        KeyPair::generate().unwrap().serialize_pem(),
    )
    .unwrap();
    assert!(create_key(&f.db, &f.network, &f.server, "bad", 3600, 1, 1).is_err());
}

#[test]
fn fleet_authority_expired_key_preserves_renewal_and_signing_failure_rolls_back_quota() {
    let f = Fixture::new();
    let invite = f.invite(2);
    let request = csr();
    let member = f.register(&invite, &request).unwrap();
    f.db.conn
        .execute("UPDATE fleet_enrollment_keys SET expires=?", [now() - 1])
        .unwrap();
    f.db.conn
        .execute(
            "UPDATE fleet_enrollment_certificates SET renew_after=?",
            [now() - 1],
        )
        .unwrap();
    assert!(f.register(&invite, &csr()).is_err());
    assert!(
        renew(
            &f.db,
            &f.network,
            &f.server,
            &fingerprint(&member),
            &request
        )
        .is_ok()
    );
    f.db.conn
        .execute("UPDATE fleet_enrollment_keys SET expires=?", [now() + 3600])
        .unwrap();
    let saved = std::fs::read(&f.server.issuer_key).unwrap();
    std::fs::write(
        &f.server.issuer_key,
        KeyPair::generate().unwrap().serialize_pem(),
    )
    .unwrap();
    assert!(f.register(&invite, &csr()).is_err());
    let count: i64 =
        f.db.conn
            .query_row("SELECT COUNT(*) FROM fleet_enrollment_members", [], |r| {
                r.get(0)
            })
            .unwrap();
    assert_eq!(count, 1);
    std::fs::write(&f.server.issuer_key, saved).unwrap();
    assert!(f.register(&invite, &csr()).is_ok());
}

#[test]
fn fleet_authority_concurrent_admission_obeys_quota() {
    let f = Fixture::new();
    let invitation = f.invite(1);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let root = f.db.root.clone();
            let network = f.network.clone();
            let server = f.server.clone();
            let invitation = invitation.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let db = Store::open(&root).unwrap();
                let request = csr();
                barrier.wait();
                register(
                    &db,
                    &network,
                    &server,
                    &invitation.key_id,
                    &invitation.token,
                    &request,
                )
                .is_ok()
            })
        })
        .collect();
    let admitted = handles
        .into_iter()
        .map(|handle| usize::from(handle.join().unwrap()))
        .sum::<usize>();
    assert_eq!(admitted, 1);
}

#[test]
fn fleet_authority_private_signer_and_controller_identity_checks() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let wrong_name = ServerConfig {
        tls_name: "other.test".into(),
        ..f.server.clone()
    };
    assert!(create_key(&f.db, &f.network, &wrong_name, "wrong", 3600, 1, 1).is_err());
    let worker = NetworkConfig {
        controller_peer: Some("upstream".into()),
        ..f.network.clone()
    };
    assert!(create_key(&f.db, &worker, &f.server, "wrong", 3600, 1, 1).is_err());
    std::fs::set_permissions(&f.server.issuer_key, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(create_key(&f.db, &f.network, &f.server, "wrong", 3600, 1, 1).is_err());
}

#[test]
fn fleet_authority_reasserts_expired_member_without_new_identity_or_quota() {
    let f = Fixture::new();
    let invitation = f.invite(1);
    let request = csr();
    let first = f.register(&invitation, &request).unwrap();
    f.db.conn
        .execute(
            "UPDATE fleet_enrollment_certificates SET expires=?",
            [now() - 1],
        )
        .unwrap();
    assert!(renew(&f.db, &f.network, &f.server, &fingerprint(&first), &request).is_err());
    let recovered = f.register(&invitation, &request).unwrap();
    assert_eq!(recovered.runtime_id, first.runtime_id);
    assert_ne!(recovered.certificate_pem, first.certificate_pem);
    assert!(recovered.expires > now());
    assert!(is_active(&f.db, &first.runtime_id).unwrap());
    assert!(identity(&f.db, &fingerprint(&first)).unwrap().is_none());
    assert_eq!(
        identity(&f.db, &fingerprint(&recovered)).unwrap(),
        Some(first.runtime_id)
    );
    assert_eq!(
        f.register(&invitation, &request).unwrap().certificate_pem,
        recovered.certificate_pem
    );
    assert!(f.register(&invitation, &csr()).is_err());
    let members: i64 =
        f.db.conn
            .query_row("SELECT COUNT(*) FROM fleet_enrollment_members", [], |r| {
                r.get(0)
            })
            .unwrap();
    let events: i64 =
        f.db.conn
            .query_row(
                "SELECT COUNT(*) FROM management_events WHERE kind='fleet.worker_readmitted'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(members, 1);
    assert_eq!(events, 1);
    revoke_key(&f.db, &invitation.key_id).unwrap();
    assert_eq!(
        f.register(&invitation, &request).unwrap().certificate_pem,
        recovered.certificate_pem
    );
}

#[test]
fn fleet_authority_readmission_requires_valid_original_key_and_active_membership() {
    for restriction in [
        "expired_key",
        "revoked_key",
        "revoked_member",
        "revoked_enrollment",
        "different_key",
    ] {
        let f = Fixture::new();
        let invitation = f.invite(1);
        let request = csr();
        let member = f.register(&invitation, &request).unwrap();
        f.db.conn
            .execute(
                "UPDATE fleet_enrollment_certificates SET expires=?",
                [now() - 1],
            )
            .unwrap();
        let credential = match restriction {
            "expired_key" => {
                f.db.conn
                    .execute("UPDATE fleet_enrollment_keys SET expires=?", [now() - 1])
                    .unwrap();
                invitation
            }
            "revoked_key" => {
                revoke_key(&f.db, &invitation.key_id).unwrap();
                invitation
            }
            "revoked_member" => {
                revoke_worker(&f.db, &member.runtime_id).unwrap();
                invitation
            }
            "revoked_enrollment" => {
                f.db.conn
                    .execute(
                        "UPDATE runtime_enrollments SET state='revoked' WHERE runtime=?",
                        [&member.runtime_id],
                    )
                    .unwrap();
                invitation
            }
            "different_key" => f.invite(1),
            _ => unreachable!(),
        };
        assert!(
            f.register(&credential, &request).is_err(),
            "{restriction} must prevent readmission"
        );
        assert!(!is_active(&f.db, &member.runtime_id).unwrap());
        let count: i64 =
            f.db.conn
                .query_row(
                    "SELECT COUNT(*) FROM fleet_enrollment_certificates",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(count, 1);
    }
}
