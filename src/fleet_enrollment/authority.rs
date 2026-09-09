//! Durable admission authority. Fleet secrets admit members; each member owns its key.
use super::{CERT_LIFETIME, Certificate, Invitation, ServerConfig};
use crate::{
    network::NetworkConfig,
    store::{Store, hash, id, now},
};
use anyhow::{Context, Result, ensure};
use rcgen::{
    CertificateParams, CertificateSigningRequestParams, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose, PublicKeyData,
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use x509_parser::prelude::{FromDer, X509Certificate};

fn private_key(path: &std::path::Path) -> Result<KeyPair> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && meta.permissions().mode() & 0o077 == 0,
        "issuer and controller keys must be private regular files (chmod 600)"
    );
    Ok(KeyPair::from_pem(&std::fs::read_to_string(path)?)?)
}

/// Check CA authority and signer correspondence before exposing admission.
pub fn validate_signer(network: &NetworkConfig, server: &ServerConfig) -> Result<()> {
    issuer(network, server).map(|_| ())
}

fn issuer(network: &NetworkConfig, server: &ServerConfig) -> Result<Issuer<'static, KeyPair>> {
    network.validate()?;
    ensure!(
        network.controller_peer.is_none(),
        "a worker cannot act as fleet controller"
    );
    let ca_pem = std::fs::read_to_string(&network.ca_cert)?;
    let ca_der = pem::parse(&ca_pem)?;
    let (_, ca) = X509Certificate::from_der(ca_der.contents())
        .map_err(|_| anyhow::anyhow!("invalid CA certificate"))?;
    let key = private_key(&server.issuer_key)?;
    ensure!(
        ca.public_key().raw == key.subject_public_key_info(),
        "issuer key does not match network CA"
    );
    ensure!(
        ca.validity().is_valid() && ca.validity().not_after.timestamp() > now() + CERT_LIFETIME,
        "CA expired or expires within worker certificate lifetime"
    );
    ensure!(
        ca.basic_constraints()?.is_some_and(|v| v.value.ca)
            && ca.key_usage()?.is_some_and(|v| v.value.key_cert_sign()),
        "issuer certificate must authorize CA signing"
    );
    let controller_der = pem::parse(std::fs::read(&network.identity_cert)?)?;
    let (_, controller) = X509Certificate::from_der(controller_der.contents())
        .map_err(|_| anyhow::anyhow!("invalid controller certificate"))?;
    ensure!(
        controller.validity().is_valid(),
        "controller certificate expired"
    );
    controller
        .verify_signature(Some(ca.public_key()))
        .context("controller certificate not signed by network CA")?;
    ensure!(
        controller.public_key().raw
            == private_key(&network.identity_key)?.subject_public_key_info(),
        "controller key does not match certificate"
    );
    ensure!(controller.subject_alternative_name()?.is_some_and(|san|san.value.general_names.iter().any(|name|matches!(name,x509_parser::extensions::GeneralName::DNSName(dns) if *dns==server.tls_name))), "controller certificate does not match enrollment TLS name");
    Ok(Issuer::from_ca_cert_pem(&ca_pem, key)?)
}

pub fn create_key(
    db: &Store,
    network: &NetworkConfig,
    server: &ServerConfig,
    name: &str,
    expires_in: i64,
    max_workers: usize,
    concurrency: usize,
) -> Result<Invitation> {
    ensure!(
        !name.trim().is_empty() && name.len() <= 128 && !name.chars().any(char::is_control),
        "fleet key name must contain 1..128 printable bytes"
    );
    ensure!(
        (60..=366 * 86400).contains(&expires_in),
        "fleet key lifetime must be 60 seconds to 366 days"
    );
    ensure!(
        (1..=1_000_000).contains(&max_workers),
        "fleet member limit must be 1..1000000"
    );
    ensure!(
        (1..=64).contains(&concurrency),
        "worker concurrency must be 1..64"
    );
    validate_signer(network, server)?;
    let mut random = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random)
        .map_err(|_| anyhow::anyhow!("secure random generation failed"))?;
    let invitation = Invitation {
        version: 1,
        key_id: id(),
        token: hex::encode(random),
        endpoint: server.listen,
        tls_name: server.tls_name.clone(),
        ca_pem: std::fs::read_to_string(&network.ca_cert)?,
        controller_id: network.runtime_id.clone(),
        controller_address: server.controller_address,
        controller_fingerprint: hash(
            pem::parse(std::fs::read(&network.identity_cert)?)?.contents(),
        ),
    };
    invitation.validate()?;
    db.atomic(|| {
        db.conn.execute(
            "INSERT INTO fleet_enrollment_keys VALUES(?,?,?,?,?,?,'active',?)",
            params![
                invitation.key_id,
                name,
                hash(invitation.token.as_bytes()),
                now() + expires_in,
                max_workers as i64,
                concurrency as i64,
                now()
            ],
        )?;
        crate::management::event(
            db,
            "fleet.key_created",
            json!({"id":invitation.key_id,"name":name}),
        )
    })?;
    Ok(invitation)
}

pub fn list_keys(db: &Store) -> Result<Value> {
    Ok(db.rows("SELECT k.id,k.name,k.expires,k.max_workers,k.concurrency,k.state,k.created,(SELECT COUNT(*) FROM fleet_enrollment_members m WHERE m.key_id=k.id) AS admitted_workers FROM fleet_enrollment_keys k ORDER BY k.created,k.id",&[])?.into())
}

pub fn revoke_key(db: &Store, key_id: &str) -> Result<()> {
    db.atomic(|| {
        ensure!(
            db.conn.execute(
                "UPDATE fleet_enrollment_keys SET state='revoked' WHERE id=?",
                [key_id]
            )? == 1,
            "unknown fleet key"
        );
        crate::management::event(db, "fleet.key_revoked", json!({"id":key_id}))
    })
}
pub fn revoke_worker(db: &Store, runtime: &str) -> Result<()> {
    db.atomic(|| {
        ensure!(
            db.conn.execute(
                "UPDATE fleet_enrollment_members SET state='revoked' WHERE runtime=?",
                [runtime]
            )? == 1,
            "unknown fleet worker"
        );
        db.conn.execute(
            "UPDATE runtime_enrollments SET state='revoked' WHERE runtime=?",
            [runtime],
        )?;
        crate::management::event(db, "fleet.worker_revoked", json!({"runtime":runtime}))
    })
}

fn request(csr_pem: &str) -> Result<(CertificateSigningRequestParams, String)> {
    ensure!(csr_pem.len() <= 16 * 1024, "CSR too large");
    let request =
        CertificateSigningRequestParams::from_pem(csr_pem).context("invalid signed CSR")?;
    let public_hash = hash(&request.public_key.subject_public_key_info());
    Ok((request, public_hash))
}

pub fn register(
    db: &Store,
    network: &NetworkConfig,
    server: &ServerConfig,
    key_id: &str,
    token: &str,
    csr_pem: &str,
) -> Result<Certificate> {
    ensure!(
        key_id.len() <= 128 && token.len() == 64,
        "invalid fleet credential"
    );
    let (request, public_hash) = request(csr_pem)?;
    db.atomic(|| {
        let key=db.rows("SELECT * FROM fleet_enrollment_keys WHERE id=? AND token_hash=?",&[&key_id,&hash(token.as_bytes())])?.into_iter().next().context("invalid fleet credential")?;
        let existing=db.rows("SELECT * FROM fleet_enrollment_members WHERE public_key_hash=?",&[&public_hash])?;
        if let Some(member)=existing.first() {
            ensure!(member["key_id"]==key_id && member["state"]=="active","worker key already admitted to another fleet or revoked");
            let runtime=member["runtime"].as_str().context("member runtime")?;
            ensure!(enrollment_active(db,runtime)?,"worker revoked");
            let cert=current(db,runtime)?;
            if cert.expires > now() {
                return Ok(cert);
            }
            ensure!(key["state"]=="active" && key["expires"].as_i64().unwrap_or(0)>now(),"fleet key revoked or expired");
            let certificate = issue(db, network, server, runtime, request, cert.concurrency)?;
            crate::management::event(db,"fleet.worker_readmitted",json!({"runtime":runtime,"key_id":key_id}))?;
            return Ok(certificate);
        }
        ensure!(key["state"]=="active" && key["expires"].as_i64().unwrap_or(0)>now(),"fleet key revoked or expired");
        let count:i64=db.conn.query_row("SELECT COUNT(*) FROM fleet_enrollment_members WHERE key_id=?",[key_id],|r|r.get(0))?;
        ensure!(count<key["max_workers"].as_i64().unwrap_or(0),"fleet member limit reached");
        let runtime=format!("fleet-{}", &public_hash[..40]);
        ensure!(!db.conn.query_row("SELECT EXISTS(SELECT 1 FROM runtime_enrollments WHERE runtime=?)",[&runtime],|r|r.get::<_,bool>(0))?,"runtime identity already exists");
        let concurrency=key["concurrency"].as_u64().context("fleet concurrency")? as usize;
        db.conn.execute("INSERT INTO fleet_enrollment_members(runtime,key_id,public_key_hash,state,created) VALUES(?,?,?,'active',?)",params![runtime,key_id,public_hash,now()])?;
        let certificate=issue(db,network,server,&runtime,request,concurrency)?;
        crate::management::event(db,"fleet.worker_admitted",json!({"runtime":runtime,"key_id":key_id}))?;
        Ok(certificate)
    })
}

pub fn renew(
    db: &Store,
    network: &NetworkConfig,
    server: &ServerConfig,
    fingerprint: &str,
    csr_pem: &str,
) -> Result<Certificate> {
    let (request, public_hash) = request(csr_pem)?;
    db.atomic(|| {
        let runtime =
            identity(db, fingerprint)?.context("worker certificate expired or revoked")?;
        let expected: String = db.conn.query_row(
            "SELECT public_key_hash FROM fleet_enrollment_members WHERE runtime=?",
            [&runtime],
            |r| r.get(0),
        )?;
        ensure!(
            expected == public_hash,
            "renewal cannot change worker public key"
        );
        let cert = current(db, &runtime)?;
        if cert.renew_after > now() {
            return Ok(cert);
        }
        let certificate = issue(db, network, server, &runtime, request, cert.concurrency)?;
        crate::management::event(db, "fleet.worker_renewed", json!({"runtime":runtime}))?;
        Ok(certificate)
    })
}

fn current(db: &Store, runtime: &str) -> Result<Certificate> {
    Ok(db.conn.query_row("SELECT c.runtime,c.certificate_pem,c.expires,c.renew_after,c.concurrency FROM fleet_enrollment_certificates c JOIN fleet_enrollment_members m ON m.current_fingerprint=c.fingerprint WHERE m.runtime=?",[runtime],|r|Ok(Certificate {runtime_id:r.get(0)?,certificate_pem:r.get(1)?,expires:r.get(2)?,renew_after:r.get(3)?,concurrency:r.get(4)?}))?)
}

fn issue(
    db: &Store,
    network: &NetworkConfig,
    server: &ServerConfig,
    runtime: &str,
    request: CertificateSigningRequestParams,
    concurrency: usize,
) -> Result<Certificate> {
    let issued = now();
    let mut subject = DistinguishedName::new();
    subject.push(DnType::CommonName, runtime);
    let mut parameters = CertificateParams::default();
    parameters.distinguished_name = subject;
    parameters.is_ca = IsCa::ExplicitNoCa;
    parameters.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    parameters.serial_number = Some(uuid::Uuid::new_v4().as_bytes().to_vec().into());
    parameters.not_before = time::OffsetDateTime::from_unix_timestamp(issued - 300)?;
    parameters.not_after = time::OffsetDateTime::from_unix_timestamp(issued + CERT_LIFETIME)?;
    // Only proof of key possession survives CSR parsing; all requested authority is discarded.
    let safe = CertificateSigningRequestParams {
        params: parameters,
        public_key: request.public_key,
    };
    let signed = safe.signed_by(&issuer(network, server)?)?;
    let fingerprint = hash(signed.der());
    let cert = Certificate {
        runtime_id: runtime.into(),
        certificate_pem: signed.pem(),
        expires: issued + CERT_LIFETIME,
        renew_after: issued + CERT_LIFETIME / 2,
        concurrency,
    };
    db.conn.execute(
        "INSERT INTO fleet_enrollment_certificates VALUES(?,?,?,?,?,?)",
        params![
            fingerprint,
            runtime,
            cert.certificate_pem,
            cert.expires,
            cert.renew_after,
            concurrency as i64
        ],
    )?;
    db.conn.execute(
        "UPDATE fleet_enrollment_members SET current_fingerprint=? WHERE runtime=?",
        params![fingerprint, runtime],
    )?;
    db.conn.execute("INSERT INTO runtime_enrollments VALUES(?,?,'',?,'active') ON CONFLICT(runtime) DO UPDATE SET fingerprint=excluded.fingerprint,expires=excluded.expires",params![runtime,fingerprint,cert.expires])?;
    Ok(cert)
}

fn enrollment_active(db: &Store, runtime: &str) -> Result<bool> {
    Ok(db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM runtime_enrollments WHERE runtime=? AND state='active')",
        [runtime],
        |r| r.get(0),
    )?)
}

pub fn identity(db: &Store, fingerprint: &str) -> Result<Option<String>> {
    Ok(db.conn.query_row("SELECT c.runtime FROM fleet_enrollment_certificates c JOIN fleet_enrollment_members m ON m.runtime=c.runtime JOIN runtime_enrollments e ON e.runtime=c.runtime WHERE c.fingerprint=? AND c.expires>? AND m.state='active' AND e.state='active'",params![fingerprint,now()],|r|r.get(0)).optional()?)
}

pub fn is_active(db: &Store, runtime: &str) -> Result<bool> {
    Ok(db.conn.query_row("SELECT EXISTS(SELECT 1 FROM runtime_enrollments e LEFT JOIN fleet_enrollment_members m ON m.runtime=e.runtime LEFT JOIN fleet_enrollment_certificates c ON c.fingerprint=m.current_fingerprint WHERE e.runtime=? AND e.state='active' AND (m.runtime IS NULL OR (m.state='active' AND c.expires>?)))",params![runtime,now()],|r|r.get(0))?)
}
