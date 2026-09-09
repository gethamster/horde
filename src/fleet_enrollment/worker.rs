//! Outbound worker enrollment and renewal with a locally generated, durable key.
use super::{Certificate, Invitation, MAX_PACKET, replace_private};
use crate::network::{DirectPeer, NetworkConfig, Provider};
use anyhow::{Context, Result, ensure};
use rcgen::{CertificateParams, KeyPair, PublicKeyData};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::Read,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use tonic::transport::{Certificate as TlsCertificate, ClientTlsConfig, Endpoint, Identity};
use x509_parser::prelude::{FromDer, X509Certificate, X509CertificationRequest};

const STATE_FILE: &str = "fleet-worker.json";
const PENDING_FILE: &str = "fleet-worker-pending.json";
const KEY_FILE: &str = "fleet-worker.key";
const CSR_FILE: &str = "fleet-worker.csr";
const DEADLINE: Duration = Duration::from_secs(15);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerState {
    invitation: Invitation,
    certificate: Certificate,
    #[serde(default)]
    credential_file: Option<PathBuf>,
}

enum Credential {
    File(PathBuf),
    Json(String),
}

impl Credential {
    fn read(&self) -> Result<Invitation> {
        let raw = match self {
            Self::File(path) => invitation_read(path)?,
            Self::Json(raw) => raw.clone(),
        };
        ensure!(raw.len() <= MAX_PACKET, "enrollment invitation too large");
        let invitation: Invitation =
            serde_json::from_str(&raw).context("invalid enrollment invitation")?;
        invitation.validate()?;
        Ok(invitation)
    }

    fn file_reference(&self) -> Result<Option<PathBuf>> {
        match self {
            Self::File(path) => Ok(Some(std::path::absolute(path)?)),
            Self::Json(_) => Ok(None),
        }
    }
}

fn environment_credential() -> Result<Option<Credential>> {
    let file = crate::branding::var_os("HORDE_ENROLLMENT_FILE");
    let raw = crate::branding::var_os("HORDE_ENROLLMENT_JSON");
    ensure!(
        file.is_none() || raw.is_none(),
        "set only one of HORDE_ENROLLMENT_FILE and HORDE_ENROLLMENT_JSON"
    );
    if let Some(path) = file {
        return Ok(Some(Credential::File(path.into())));
    }
    raw.map(|raw| {
        raw.into_string()
            .map(Credential::Json)
            .map_err(|_| anyhow::anyhow!("enrollment invitation must be UTF-8"))
    })
    .transpose()
}

fn recovery_credential(state: &WorkerState) -> Result<Credential> {
    environment_credential()?.or_else(|| state.credential_file.clone().map(Credential::File))
        .context("worker certificate expired; supply its fleet credential with HORDE_ENROLLMENT_FILE, HORDE_ENROLLMENT_JSON, or network join --invitation")
}

fn validate_reassertion(state: &WorkerState, invitation: &Invitation) -> Result<()> {
    invitation.validate()?;
    ensure!(
        !invitation.token.is_empty(),
        "enrollment credential is empty"
    );
    ensure!(
        serde_json::to_value(sanitized(invitation))? == serde_json::to_value(&state.invitation)?,
        "recovery credential belongs to a different fleet or controller"
    );
    Ok(())
}

async fn reassert(root: &Path, state: WorkerState, source: &Credential) -> Result<WorkerState> {
    let invitation = source.read().context(
        "worker certificate expired; restore its fleet enrollment credential file or injected credential",
    )?;
    validate_reassertion(&state, &invitation)?;
    let certificate = exchange(root, &invitation, &read_key(root)?, None).await?;
    ensure!(
        certificate.runtime_id == state.certificate.runtime_id,
        "recovery changed worker identity"
    );
    let next = WorkerState {
        certificate,
        credential_file: source.file_reference()?.or(state.credential_file),
        ..state
    };
    persist(root, &next)?;
    install(root, &next)?;
    Ok(next)
}

fn private_read(path: &Path) -> Result<String> {
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    read_private_file(file)
}

// Projected Kubernetes secrets use symlinks; validate the opened target.
fn invitation_read(path: &Path) -> Result<String> {
    read_private_file(File::open(path)?)
}

fn read_private_file(file: File) -> Result<String> {
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o077 == 0,
        "enrollment files must be private regular files"
    );
    ensure!(
        metadata.len() <= MAX_PACKET as u64,
        "enrollment file too large"
    );
    let mut raw = String::new();
    file.take(MAX_PACKET as u64 + 1).read_to_string(&mut raw)?;
    ensure!(raw.len() <= MAX_PACKET, "enrollment file too large");
    Ok(raw)
}

fn lock(root: &Path) -> Result<File> {
    std::fs::create_dir_all(root)?;
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join("fleet-worker.lock"))?;
    fs2::FileExt::try_lock_exclusive(&file)
        .context("another worker enrollment operation is running")?;
    Ok(file)
}

fn sanitized(invitation: &Invitation) -> Invitation {
    Invitation {
        token: String::new(),
        ..invitation.clone()
    }
}

fn load(root: &Path) -> Result<Option<WorkerState>> {
    if !root.join(STATE_FILE).try_exists()? {
        return Ok(None);
    }
    let state: WorkerState = serde_json::from_str(&private_read(&root.join(STATE_FILE))?)?;
    ensure!(
        state.invitation.token.is_empty(),
        "persisted enrollment state must not contain a credential"
    );
    state.invitation.validate()?;
    validate_certificate(
        &state.invitation,
        &state.certificate,
        &read_key(root)?,
        false,
    )?;
    Ok(Some(state))
}

fn read_key(root: &Path) -> Result<KeyPair> {
    KeyPair::from_pem(&private_read(&root.join(KEY_FILE))?).context("invalid worker private key")
}

fn prepare(root: &Path, invitation: &Invitation) -> Result<KeyPair> {
    invitation.validate()?;
    ensure!(
        !invitation.token.is_empty(),
        "enrollment credential is empty"
    );
    ensure!(
        crate::branding::var_os("HORDE_BOOTSTRAP_JSON").is_none(),
        "fleet enrollment cannot be combined with legacy bootstrap"
    );
    for filename in ["network-runtime.toml", "managed-network.toml"] {
        ensure!(
            !root.join(filename).try_exists()?,
            "fleet enrollment cannot replace an existing network identity"
        );
    }
    let db = crate::store::Store::open(root)?;
    ensure!(
        crate::management::value(&db, "bootstrap_identity")?.is_none(),
        "fleet enrollment cannot replace a bootstrapped worker"
    );
    let pending = root.join(PENDING_FILE);
    if pending.try_exists()? {
        let original: Invitation = serde_json::from_str(&private_read(&pending)?)?;
        ensure!(
            serde_json::to_value(original)? == serde_json::to_value(sanitized(invitation))?,
            "pending enrollment belongs to a different invitation"
        );
    } else {
        ensure!(
            !root.join(KEY_FILE).try_exists()? && !root.join(CSR_FILE).try_exists()?,
            "worker identity exists without enrollment state"
        );
        replace_private(&pending, &serde_json::to_vec(&sanitized(invitation))?)?;
    }
    if !root.join(KEY_FILE).try_exists()? {
        crate::secrets::write_private(
            &root.join(KEY_FILE),
            KeyPair::generate()?.serialize_pem().as_bytes(),
        )?;
        File::open(root)?.sync_all()?;
    }
    read_key(root)
}

fn csr(root: &Path, key: &KeyPair) -> Result<String> {
    let path = root.join(CSR_FILE);
    if !path.try_exists()? {
        let params = CertificateParams::new(Vec::<String>::new())?;
        crate::secrets::write_private(&path, params.serialize_request(key)?.pem()?.as_bytes())?;
        File::open(root)?.sync_all()?;
    }
    let raw = private_read(&path)?;
    let pem = pem::parse(&raw)?;
    let (remainder, request) = X509CertificationRequest::from_der(pem.contents())
        .map_err(|_| anyhow::anyhow!("invalid persisted worker CSR"))?;
    ensure!(
        remainder.is_empty()
            && request.certification_request_info.subject_pki.raw == key.subject_public_key_info(),
        "worker CSR does not match local key"
    );
    request
        .verify_signature()
        .context("invalid worker CSR signature")?;
    Ok(raw)
}

fn validate_certificate(
    invitation: &Invitation,
    reply: &Certificate,
    key: &KeyPair,
    require_current: bool,
) -> Result<()> {
    ensure!(
        reply.certificate_pem.len() <= MAX_PACKET && (1..=64).contains(&reply.concurrency),
        "invalid worker certificate reply"
    );
    ensure!(
        !reply.runtime_id.is_empty()
            && reply.runtime_id.len() <= 128
            && reply
                .runtime_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
        "invalid worker runtime ID"
    );
    let pem = pem::parse(&reply.certificate_pem)?;
    let ca = pem::parse(&invitation.ca_pem)?;
    let (remainder, cert) = X509Certificate::from_der(pem.contents())
        .map_err(|_| anyhow::anyhow!("invalid worker certificate"))?;
    let (_, issuer) = X509Certificate::from_der(ca.contents())
        .map_err(|_| anyhow::anyhow!("invalid controller CA"))?;
    ensure!(
        remainder.is_empty() && pem.tag() == "CERTIFICATE" && !cert.is_ca(),
        "invalid worker leaf certificate"
    );
    ensure!(
        cert.public_key().raw == key.subject_public_key_info(),
        "worker certificate does not match local private key"
    );
    cert.verify_signature(Some(issuer.public_key()))
        .context("worker certificate issuer mismatch")?;
    ensure!(
        cert.issuer() == issuer.subject(),
        "worker certificate issuer name mismatch"
    );
    ensure!(
        cert.subject().iter_common_name().count() == 1
            && cert
                .subject()
                .iter_common_name()
                .next()
                .and_then(|cn| cn.as_str().ok())
                == Some(reply.runtime_id.as_str()),
        "worker certificate identity mismatch"
    );
    let usage = cert
        .extended_key_usage()?
        .context("worker certificate must specify client authentication")?;
    ensure!(
        usage.value.client_auth && !usage.value.server_auth && !usage.value.any,
        "worker certificate must permit only client authentication"
    );
    ensure!(
        reply.expires == cert.validity().not_after.timestamp()
            && reply.renew_after < reply.expires
            && reply.renew_after > cert.validity().not_before.timestamp(),
        "invalid worker certificate renewal times"
    );
    ensure!(
        !require_current || cert.validity().is_valid(),
        "worker certificate is not currently valid"
    );
    Ok(())
}

fn persist(root: &Path, state: &WorkerState) -> Result<()> {
    ensure!(
        state.invitation.token.is_empty(),
        "cannot persist a fleet enrollment credential"
    );
    replace_private(&root.join(STATE_FILE), &serde_json::to_vec(state)?)
}

fn write_same(path: &Path, contents: &[u8]) -> Result<()> {
    if path.try_exists()? {
        ensure!(
            private_read(path)?.as_bytes() == contents,
            "existing worker identity file differs"
        );
        return Ok(());
    }
    crate::secrets::write_private(path, contents)
}

fn install(root: &Path, state: &WorkerState) -> Result<()> {
    for name in ["managed-network.toml", "network-runtime.toml"] {
        if root.join(name).try_exists()? {
            let current = NetworkConfig::load(Some(&root.join(name)))?;
            ensure!(
                current.runtime_id == state.certificate.runtime_id
                    && current.identity_key == root.join(KEY_FILE)
                    && current.controller_peer.as_deref() == Some(&state.invitation.controller_id),
                "fleet enrollment cannot replace an unrelated network identity"
            );
        }
    }
    let cert_path = root.join(format!(
        "fleet-worker-{}.pem",
        crate::store::hash(state.certificate.certificate_pem.as_bytes())
    ));
    let ca_path = root.join("fleet-worker-ca.pem");
    write_same(&cert_path, state.certificate.certificate_pem.as_bytes())?;
    write_same(&ca_path, state.invitation.ca_pem.as_bytes())?;
    let controller = &state.invitation.controller_id;
    let network = NetworkConfig {
        provider: Provider::Direct,
        runtime_id: state.certificate.runtime_id.clone(),
        controller_peer: Some(controller.clone()),
        execution_clients: vec![controller.clone()],
        management_clients: vec![controller.clone()],
        ca_cert: ca_path,
        identity_cert: cert_path,
        identity_key: root.join(KEY_FILE),
        allowed_clients: [(
            state.invitation.controller_fingerprint.to_ascii_lowercase(),
            controller.clone(),
        )]
        .into(),
        peers: [(
            controller.clone(),
            DirectPeer {
                address: state.invitation.controller_address,
                tls_name: state.invitation.tls_name.clone(),
            },
        )]
        .into(),
        ..Default::default()
    };
    network.validate()?;
    replace_private(
        &root.join("managed-network.toml"),
        toml::to_string(&network)?.as_bytes(),
    )?;
    crate::federation::configure(root, &network)?;
    let db = crate::store::Store::open(root)?;
    if crate::management::value(&db, "concurrency")?.is_none() {
        crate::management::set(
            &db,
            "concurrency",
            &state.certificate.concurrency.to_string(),
        )?;
    }
    Ok(())
}

async fn exchange(
    root: &Path,
    invitation: &Invitation,
    key: &KeyPair,
    existing: Option<&Certificate>,
) -> Result<Certificate> {
    use crate::federation::wire::{
        EnrollmentRequest, RenewalRequest, enrollment_client::EnrollmentClient,
    };
    let tls = ClientTlsConfig::new()
        .ca_certificate(TlsCertificate::from_pem(&invitation.ca_pem))
        .domain_name(&invitation.tls_name);
    let tls = match existing {
        Some(cert) => tls.identity(Identity::from_pem(
            &cert.certificate_pem,
            key.serialize_pem(),
        )),
        None => tls,
    };
    let endpoint = Endpoint::from_shared(format!("https://{}", invitation.endpoint))?
        .tls_config(tls)?
        .connect_timeout(DEADLINE)
        .timeout(DEADLINE);
    let response = tokio::time::timeout(DEADLINE, async {
        let mut client = EnrollmentClient::new(endpoint.connect().await?)
            .max_decoding_message_size(MAX_PACKET)
            .max_encoding_message_size(MAX_PACKET);
        let csr_pem = csr(root, key)?;
        let response = match existing {
            Some(_) => client.renew(RenewalRequest { csr_pem }).await,
            None => {
                client
                    .register(EnrollmentRequest {
                        key_id: invitation.key_id.clone(),
                        token: invitation.token.clone(),
                        csr_pem,
                    })
                    .await
            }
        }
        .map_err(|status| anyhow::anyhow!("worker enrollment rejected ({})", status.code()))?;
        Ok::<_, anyhow::Error>(response.into_inner())
    })
    .await
    .context("worker enrollment timed out")??;
    ensure!(
        response.json.len() <= MAX_PACKET,
        "worker enrollment reply too large"
    );
    let certificate: Certificate =
        serde_json::from_str(&response.json).context("invalid enrollment reply")?;
    validate_certificate(invitation, &certificate, key, true)?;
    if let Some(previous) = existing {
        ensure!(
            certificate.runtime_id == previous.runtime_id,
            "renewal changed worker identity"
        );
    }
    Ok(certificate)
}

async fn enroll(root: &Path, source: &Credential) -> Result<Certificate> {
    let invitation = source.read()?;
    let key = prepare(root, &invitation)?;
    let certificate = exchange(root, &invitation, &key, None).await?;
    let state = WorkerState {
        invitation: sanitized(&invitation),
        certificate: certificate.clone(),
        credential_file: source.file_reference()?,
    };
    persist(root, &state)?;
    install(root, &state)?;
    Ok(certificate)
}

fn daemon_lock(root: &Path) -> Result<File> {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join("daemon.lock"))?;
    fs2::FileExt::try_lock_exclusive(&file)
        .context("stop the daemon before enrolling this machine")?;
    Ok(file)
}

fn ensure_unused_user_network(config: &NetworkConfig) -> Result<()> {
    ensure!(
        config.provider == Provider::Disabled,
        "fleet enrollment cannot replace configured user network authority; use a dedicated worker account"
    );
    Ok(())
}

/// Enroll once from a private invitation file. Existing workers retain their identity.
pub async fn join(root: &Path, invitation_path: &Path) -> Result<Certificate> {
    join_inner(root, invitation_path, false).await
}

/// Join a dedicated worker root without changing user-level network authority.
pub async fn join_isolated(root: &Path, invitation_path: &Path) -> Result<Certificate> {
    join_inner(root, invitation_path, true).await
}

/// Read a private invitation before selecting its local worker directory.
pub fn read_invitation(path: &Path) -> Result<Invitation> {
    super::admin()?;
    Credential::File(path.into()).read()
}

/// Validate an existing worker without replacing state owned by its running daemon.
pub fn validate_join(root: &Path, invitation_path: &Path) -> Result<Option<Certificate>> {
    super::admin()?;
    let _lock = lock(root)?;
    let Some(state) = load(root)? else {
        return Ok(None);
    };
    validate_reassertion(&state, &read_invitation(invitation_path)?)?;
    Ok(Some(state.certificate))
}

async fn join_inner(root: &Path, invitation_path: &Path, isolated: bool) -> Result<Certificate> {
    super::admin()?;
    let _lock = lock(root)?;
    let canonical_root = root.canonicalize()?;
    let root = canonical_root.as_path();
    let _daemon = daemon_lock(root)?;
    if let Some(state) = load(root)? {
        if state.certificate.expires <= crate::store::now() {
            return Ok(
                reassert(root, state, &Credential::File(invitation_path.into()))
                    .await?
                    .certificate,
            );
        }
        install(root, &state)?;
        return Ok(state.certificate);
    }
    if !isolated {
        ensure_unused_user_network(&NetworkConfig::load(None)?)?;
    }
    enroll(root, &Credential::File(invitation_path.into())).await
}

fn ensure_no_pending_enrollment(root: &Path) -> Result<()> {
    for filename in [PENDING_FILE, KEY_FILE, CSR_FILE] {
        ensure!(
            !root.join(filename).try_exists()?,
            "unfinished worker enrollment requires the original enrollment invitation"
        );
    }
    Ok(())
}

/// Called after the daemon lock: persisted identities restart without enrollment secrets.
pub async fn bootstrap(root: &Path) -> Result<()> {
    let source = environment_credential()?;
    if !root.join(STATE_FILE).try_exists()? && source.is_none() {
        ensure_no_pending_enrollment(root)?;
        return Ok(());
    }
    super::admin()?;
    let _lock = lock(root)?;
    let canonical_root = root.canonicalize()?;
    let root = canonical_root.as_path();
    if let Some(state) = load(root)? {
        if state.certificate.expires <= crate::store::now() {
            let source = recovery_credential(&state)?;
            reassert(root, state, &source).await?;
            return Ok(());
        }
        install(root, &state)?;
        return Ok(());
    }
    ensure_unused_user_network(&NetworkConfig::load(None)?)?;
    enroll(root, &source.context("enrollment invitation required")?).await?;
    Ok(())
}

/// Renew over mutual TLS. Enrollment-key expiration does not affect existing workers.
pub async fn renew_if_due(root: &Path) -> Result<()> {
    if !root.join(STATE_FILE).try_exists()? {
        return Ok(());
    }
    super::admin()?;
    let _lock = lock(root)?;
    let canonical_root = root.canonicalize()?;
    let root = canonical_root.as_path();
    let state = load(root)?.context("worker enrollment state missing")?;
    install(root, &state)?;
    if crate::store::now() < state.certificate.renew_after {
        return Ok(());
    }
    if state.certificate.expires <= crate::store::now() {
        let source = recovery_credential(&state)?;
        reassert(root, state, &source).await?;
        return Ok(());
    }
    let key = read_key(root)?;
    let certificate = exchange(root, &state.invitation, &key, Some(&state.certificate)).await?;
    let next = WorkerState {
        certificate,
        ..state
    };
    persist(root, &next)?;
    install(root, &next)
}

#[cfg(test)]
mod tests;
