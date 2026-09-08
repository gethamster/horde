//! User-directed enrollment over Tailscale SSH, followed by Horde mTLS verification.
use crate::{
    network::{NetworkConfig, Provider},
    store::{Store, now},
};
use anyhow::{Context, Result, bail, ensure};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncWriteExt;

fn admin() -> Result<()> {
    ensure!(
        crate::branding::var_os("HORDE_WORKER_TOKEN").is_none(),
        "network setup requires administrative access"
    );
    Ok(())
}
fn lock(root: &Path) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("pairing.lock"))?;
    fs2::FileExt::try_lock_exclusive(&file)
        .context("another network setup or pairing is running")?;
    Ok(file)
}
async fn command(program: &Path, args: &[&str]) -> Result<()> {
    ensure!(
        tokio::process::Command::new(program)
            .args(args)
            .kill_on_drop(true)
            .status()
            .await?
            .success(),
        "{} operation failed",
        program.display()
    );
    Ok(())
}
fn tailscale_program() -> PathBuf {
    let app = PathBuf::from("/Applications/Tailscale.app/Contents/MacOS/Tailscale");
    if cfg!(target_os = "macos") && app.exists() {
        app
    } else {
        "tailscale".into()
    }
}
async fn ensure_tailscale(root: &Path, enable_ssh: bool) -> Result<NetworkConfig> {
    let mut program = tailscale_program();
    if tokio::process::Command::new(&program)
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_err()
    {
        eprintln!("Installing Tailscale; the installer may ask for administrator access.");
        if cfg!(target_os = "linux") {
            let installer = root.join(format!("tailscale-install-{}.sh", crate::store::id()));
            command(
                Path::new("curl"),
                &[
                    "--fail",
                    "--silent",
                    "--show-error",
                    "--location",
                    "--proto",
                    "=https",
                    "--max-time",
                    "120",
                    "https://tailscale.com/install.sh",
                    "-o",
                    installer.to_str().context("installer path")?,
                ],
            )
            .await?;
            let result = command(
                Path::new("sh"),
                &[installer.to_str().context("installer path")?],
            )
            .await;
            std::fs::remove_file(installer)?;
            result?;
        } else if cfg!(target_os = "macos") {
            command(Path::new("brew"), &["install", "--cask", "tailscale"])
                .await
                .context("install Homebrew or Tailscale, then rerun network setup")?;
            command(Path::new("open"), &["-a", "Tailscale"]).await?;
        } else {
            bail!("automatic Tailscale installation supports Linux and macOS");
        }
        program = tailscale_program();
    }
    let config = NetworkConfig {
        provider: Provider::Tailscale,
        tailscale_program: program.clone(),
        discover_all: true,
        ..Default::default()
    };
    if crate::network::discover(&config).await.is_err() {
        eprintln!("Connecting Tailscale. Complete the sign-in shown by Tailscale.");
        if cfg!(target_os = "linux") {
            command(
                Path::new("sudo"),
                &[program.to_str().context("Tailscale path")?, "up"],
            )
            .await?;
        } else {
            command(&program, &["up"]).await?;
        }
    }
    if enable_ssh {
        ensure!(
            cfg!(target_os = "linux"),
            "automatic SSH enablement supports Linux workers; other hosts need an existing Tailscale SSH server"
        );
        command(
            Path::new("sudo"),
            &[program.to_str().context("Tailscale path")?, "set", "--ssh"],
        )
        .await?;
    }
    crate::network::discover(&config).await?;
    Ok(config)
}

/// Generate private controller trust exactly once. Existing hand-configured trust is preserved.
pub fn initialize(
    root: &Path,
    mut config: NetworkConfig,
    discovery: &crate::network::Discovery,
) -> Result<NetworkConfig> {
    let file = root.join("managed-network.toml");
    if file.exists() {
        let existing = NetworkConfig::load(Some(&file))?;
        ensure!(
            existing.runtime_id == discovery.local_id,
            "this data directory belongs to another Tailscale node"
        );
        return Ok(existing);
    }
    ensure!(
        !root.join("network-runtime.toml").exists(),
        "existing network configuration requires migration; refusing to replace its trust"
    );
    ensure!(
        !discovery.local_id.is_empty() && !discovery.local_tls_name.is_empty(),
        "Tailscale must provide a stable node ID and MagicDNS name"
    );
    config.runtime_id = discovery.local_id.clone();
    let tls = root.join(format!("network-tls-{}", crate::store::id()));
    std::fs::create_dir(&tls)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tls, std::fs::Permissions::from_mode(0o700))?;
    let ca_key = KeyPair::generate()?;
    let mut ca_params = CertificateParams::new(vec![])?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = ca_params.self_signed(&ca_key)?;
    let issuer = Issuer::from_ca_cert_pem(&ca.pem(), KeyPair::from_pem(&ca_key.serialize_pem())?)?;
    let key = KeyPair::generate()?;
    let mut params = CertificateParams::new(vec![discovery.local_tls_name.clone()])?;
    params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ClientAuth,
        ExtendedKeyUsagePurpose::ServerAuth,
    ];
    params.not_before = time::OffsetDateTime::now_utc() - time::Duration::minutes(5);
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(365);
    let certificate = params.signed_by(&key, &issuer)?;
    for (name, body) in [
        ("ca.pem", ca.pem()),
        ("ca.key", ca_key.serialize_pem()),
        ("runtime.pem", certificate.pem()),
        ("runtime.key", key.serialize_pem()),
    ] {
        crate::secrets::write_private(&tls.join(name), body.as_bytes())?;
    }
    config.ca_cert = tls.join("ca.pem");
    config.identity_cert = tls.join("runtime.pem");
    config.identity_key = tls.join("runtime.key");
    config.validate()?;
    crate::secrets::write_private(&file, toml::to_string(&config)?.as_bytes())?;
    Ok(config)
}

pub async fn setup(root: &Path, enable_ssh: bool, service: bool) -> Result<Value> {
    admin()?;
    let _lock = lock(root)?;
    let config = ensure_tailscale(root, enable_ssh).await?;
    let discovery = crate::network::discover(&config).await?;
    if enable_ssh {
        ensure!(
            !service,
            "select the worker boot service with network add --service during enrollment"
        );
        return Ok(
            json!({"worker":discovery.local_id,"ssh":true,"next":"On the controller: horde network add user@this-worker"}),
        );
    }
    let existing = root.join("managed-network.toml").exists();
    ensure!(
        existing || std::os::unix::net::UnixStream::connect(root.join("daemon.sock")).is_err(),
        "stop the existing daemon with horde stop before enabling networking"
    );
    if !existing {
        ensure!(
            NetworkConfig::load(None)?.provider == Provider::Disabled,
            "existing user network configuration requires migration; refusing to replace its trust"
        );
    }
    let config = initialize(root, config, &discovery)?;
    start(root, service).await?;
    Ok(
        json!({"runtime":config.runtime_id,"ready":true,"peers":discovery.peers,"next":"horde network add user@worker"}),
    )
}
async fn start(root: &Path, service: bool) -> Result<()> {
    if service && std::os::unix::net::UnixStream::connect(root.join("daemon.sock")).is_ok() {
        let db = Store::open(root)?;
        ensure!(
            crate::management::value(&db, "service_installed")?.as_deref() == Some("true"),
            "Horde is already running without a boot service; drain and stop it before installing the service"
        );
    }
    let exe = crate::branding::var_os("HORDE_LAUNCHER")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_exe()?);
    let mut args = vec!["--data-dir", root.to_str().context("data directory")?];
    if service {
        args.extend(["service", "install"]);
    } else {
        args.push("start");
    }
    command(&exe, &args).await
}

pub fn select_peer<'a>(
    discovery: &'a crate::network::Discovery,
    target: &str,
) -> Result<(&'a crate::network::Peer, String)> {
    let (user, host) = target
        .split_once('@')
        .context("use an explicit worker account: user@worker")?;
    ensure!(
        !user.is_empty()
            && user != "root"
            && user
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
            && !user.starts_with('-'),
        "use a non-root worker account"
    );
    let matches: Vec<_> = discovery
        .peers
        .iter()
        .filter(|p| {
            p.id == host
                || p.tls_name == host
                || p.tls_name.split('.').next() == Some(host)
                || p.addresses.iter().any(|a| a.ip().to_string() == host)
        })
        .collect();
    ensure!(
        matches.len() == 1,
        "worker must match exactly one discovered Tailscale node; run horde network peers"
    );
    let peer = matches[0];
    ensure!(peer.online == Some(true), "selected worker is offline");
    Ok((
        peer,
        format!(
            "{user}@{}",
            peer.addresses.first().context("worker address")?.ip()
        ),
    ))
}

fn packet(
    db: &Store,
    config: &NetworkConfig,
    discovery: &crate::network::Discovery,
    id: &str,
) -> Result<Value> {
    // Persist the exact packet before transmission so retries cannot replace remote identity.
    let directory = db.root.join("pairing");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{}.json", crate::store::hash(id.as_bytes())));
    if path.exists() {
        return Ok(serde_json::from_slice(&std::fs::read(path)?)?);
    }
    ensure!(
        db.rows(
            "SELECT runtime FROM runtime_enrollments WHERE runtime=?",
            &[&id]
        )?
        .is_empty(),
        "worker already has an enrollment without a recoverable pairing packet"
    );
    let ca = std::fs::read_to_string(&config.ca_cert)?;
    let key_path = config.ca_cert.with_file_name("ca.key");
    use std::os::unix::fs::PermissionsExt;
    ensure!(
        std::fs::metadata(&key_path)?.permissions().mode() & 0o077 == 0,
        "CA signing key must be private"
    );
    let issuer =
        Issuer::from_ca_cert_pem(&ca, KeyPair::from_pem(&std::fs::read_to_string(key_path)?)?)?;
    let key = KeyPair::generate()?;
    let mut params = CertificateParams::new(vec![format!("{}.task.internal", crate::store::id())])?;
    params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ClientAuth,
        ExtendedKeyUsagePurpose::ServerAuth,
    ];
    params.not_before = time::OffsetDateTime::now_utc() - time::Duration::minutes(5);
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(30);
    let cert = params.signed_by(&key, &issuer)?;
    let token = crate::store::id();
    let mut remote = NetworkConfig {
        provider: Provider::Direct,
        runtime_id: id.into(),
        controller_peer: Some(config.runtime_id.clone()),
        enrollment_token: Some(token.clone()),
        ..Default::default()
    };
    remote.allowed_clients.insert(
        crate::store::hash(pem::parse(std::fs::read(&config.identity_cert)?)?.contents()),
        config.runtime_id.clone(),
    );
    remote.execution_clients.push(config.runtime_id.clone());
    remote.management_clients.push(config.runtime_id.clone());
    remote.peers.insert(
        config.runtime_id.clone(),
        crate::network::DirectPeer {
            address: std::net::SocketAddr::new(
                *discovery
                    .local_addresses
                    .first()
                    .context("local Tailscale address")?,
                config.port,
            ),
            tls_name: discovery.local_tls_name.clone(),
        },
    );
    let value = json!({"id":id,"network":remote,"ca":ca,"certificate":cert.pem(),"key":key.serialize_pem(),"concurrency":4});
    crate::secrets::write_private(&path, serde_json::to_vec(&value)?.as_slice())?;
    Ok(value)
}

pub async fn add(root: &Path, target: &str, service: bool) -> Result<Value> {
    admin()?;
    let _lock = lock(root)?;
    let config = NetworkConfig::load(Some(&root.join("managed-network.toml")))
        .context("run horde network setup first")?;
    ensure!(
        config.controller_peer.is_none(),
        "a worker cannot enroll another controller's workers"
    );
    let discovery = crate::network::discover(&config).await?;
    let (peer, target) = select_peer(&discovery, target)?;
    let runtime_id = format!("ts-{}", &crate::store::hash(peer.id.as_bytes())[..24]);
    start(root, false).await?;
    let db = Store::open(root)?;
    let value = packet(&db, &config, &discovery, &runtime_id)?;
    let fingerprint = crate::store::hash(
        pem::parse(value["certificate"].as_str().context("certificate")?)?.contents(),
    );
    let token = value["network"]["enrollment_token"]
        .as_str()
        .context("token")?;
    db.conn.execute(
        "INSERT OR IGNORE INTO runtime_enrollments VALUES(?,?,?,?, 'pending')",
        rusqlite::params![
            runtime_id,
            fingerprint,
            crate::store::hash(token.as_bytes()),
            now() + 900
        ],
    )?;
    let row = db
        .rows(
            "SELECT * FROM runtime_enrollments WHERE runtime=?",
            &[&runtime_id],
        )?
        .remove(0);
    ensure!(
        row["fingerprint"] == fingerprint && row["state"] != "revoked",
        "pairing identity was revoked or changed"
    );
    // Renew only the time window of this user-requested, identical pending packet.
    db.conn.execute(
        "UPDATE runtime_enrollments SET expires=? WHERE runtime=? AND state='pending'",
        rusqlite::params![now() + 900, runtime_id],
    )?;
    let spec = serde_json::to_string(&crate::fleet::Profile {
        provider: "tailscale".into(),
        peer: Some(runtime_id.clone()),
        ..Default::default()
    })?;
    db.conn.execute("INSERT OR IGNORE INTO managed_runtimes(id,profile,spec,resource,state,created) VALUES(?,'tailscale',?,?,'provisioned',?)", rusqlite::params![runtime_id, spec, target, now()])?;
    eprintln!(
        "Installing and pairing {} through Tailscale SSH…",
        peer.tls_name
    );
    let script = remote_script(service);
    let mut child = tokio::process::Command::new(&config.tailscale_program)
        .args(["ssh", &target, &script])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let bytes = serde_json::to_vec(&value)?;
    let operation = async {
        let mut input = child.stdin.take().context("SSH input")?;
        input.write_all(&bytes).await?;
        input.shutdown().await?;
        drop(input);
        ensure!(
            child.wait().await?.success(),
            "remote install/pair failed; verify Tailscale SSH access for this user, then rerun the same command"
        );
        Ok::<_, anyhow::Error>(())
    };
    tokio::time::timeout(Duration::from_secs(600), operation)
        .await
        .context("remote install timed out; rerun the same command to reconcile")??;
    for _ in 0..60 {
        let row = db.rows("SELECT e.state,p.observed,p.status FROM runtime_enrollments e LEFT JOIN runtime_presence p ON p.runtime=e.runtime WHERE e.runtime=?", &[&runtime_id])?;
        if row.first().is_some_and(|r| {
            r["state"] == "active" && r["observed"].as_i64().is_some_and(|t| t >= now() - 15)
        }) {
            crate::management::event(&db, "runtime_paired", json!({"runtime":runtime_id}))?;
            return Ok(
                json!({"runtime":runtime_id,"host":peer.tls_name,"ready":true,"status":row[0]["status"]}),
            );
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    bail!(
        "worker installed but mTLS handshake not confirmed; allow worker access to controller TCP {}, inspect worker daemon.log, then rerun network add",
        config.port
    )
}

pub fn remote_script(service: bool) -> String {
    // All shell text is constant; credentials travel only on encrypted stdin.
    format!(
        r#"set -eu
export PATH="$HOME/.local/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin"
if ! command -v horde >/dev/null 2>&1 && command -v task >/dev/null 2>&1; then
  exec task network accept{}
fi
if ! command -v horde >/dev/null 2>&1; then
  scratch=$(mktemp -d)
  trap 'rm -f "$scratch/install.sh"; rmdir "$scratch"' EXIT
  curl --fail --silent --show-error --location --proto '=https' --max-time 120 https://horde.sh/install -o "$scratch/install.sh"
  sh "$scratch/install.sh" --no-service </dev/null
fi
exec horde network accept{}
"#,
        if service { " --service" } else { "" },
        if service { " --service" } else { "" }
    )
}

pub async fn accept(root: &Path, service: bool) -> Result<Value> {
    admin()?;
    let _lock = lock(root)?;
    use std::io::Read;
    let mut raw = String::new();
    std::io::stdin()
        .take(128 * 1024 + 1)
        .read_to_string(&mut raw)?;
    ensure!(raw.len() <= 128 * 1024, "bootstrap packet too large");
    let incoming: Value = serde_json::from_str(&raw)?;
    let intent = root.join("pairing-accept.json");
    let retry = if intent.exists() {
        ensure!(
            serde_json::from_slice::<Value>(&std::fs::read(&intent)?)? == incoming,
            "pairing retry cannot replace the pending identity"
        );
        true
    } else {
        false
    };
    let db = Store::open(root)?;
    if !retry && crate::management::value(&db, "bootstrap_identity")?.is_none() {
        ensure!(
            std::os::unix::net::UnixStream::connect(root.join("daemon.sock")).is_err()
                && !root.join("managed-network.toml").exists()
                && NetworkConfig::load(None)?.provider == Provider::Disabled,
            "existing runtime must be stopped and its network identity preserved; use a fresh worker account for enrollment"
        );
    } else if crate::management::value(&db, "bootstrap_identity")?.is_some() {
        let value: Value = serde_json::from_str(&raw)?;
        ensure!(
            std::fs::read_to_string(root.join("tls/runtime.pem"))?
                == value["certificate"].as_str().context("certificate")?,
            "refusing to replace an existing worker certificate"
        );
    }
    if !retry {
        crate::secrets::write_private(&intent, raw.as_bytes())?;
    }
    crate::enrollment::apply_bootstrap(root, &raw)?;
    start(root, service).await?;
    Ok(json!({"installed":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn discovery() -> crate::network::Discovery {
        crate::network::Discovery {
            provider: Provider::Direct,
            local_id: "parent".into(),
            local_tls_name: "parent.test".into(),
            local_addresses: vec!["127.0.0.1".parse().unwrap()],
            peers: vec![crate::network::Peer {
                id: "nABC".into(),
                tls_name: "worker.tail.test".into(),
                addresses: vec!["100.64.0.2:7443".parse().unwrap()],
                online: Some(true),
            }],
        }
    }
    #[test]
    fn selection_requires_unambiguous_discovered_host_and_safe_user() {
        let mut d = discovery();
        assert_eq!(
            select_peer(&d, "alice@worker").unwrap().1,
            "alice@100.64.0.2"
        );
        assert!(select_peer(&d, "root@worker").is_err());
        assert!(select_peer(&d, "alice@unknown").is_err());
        assert!(select_peer(&d, "alice;id@worker").is_err());
        let mut duplicate = d.peers[0].clone();
        duplicate.id = "nOTHER".into();
        duplicate.tls_name = "worker.other.test".into();
        d.peers.push(duplicate);
        assert!(select_peer(&d, "alice@worker").is_err());
        assert!(select_peer(&d, "alice@nABC").is_ok());
        d.peers[0].online = Some(false);
        assert!(select_peer(&d, "alice@nABC").is_err());
    }
    #[test]
    fn identity_is_private_persistent_and_cannot_replace_another_node() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let d = discovery();
        let first = initialize(temp.path(), NetworkConfig::default(), &d).unwrap();
        let key = std::fs::read(&first.identity_key).unwrap();
        let second = initialize(temp.path(), NetworkConfig::default(), &d).unwrap();
        assert_eq!(first.identity_key, second.identity_key);
        assert_eq!(key, std::fs::read(&second.identity_key).unwrap());
        assert_eq!(
            std::fs::metadata(&second.identity_key)
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        let mut changed = d;
        changed.local_id = "different".into();
        assert!(initialize(temp.path(), NetworkConfig::default(), &changed).is_err());
    }
    #[tokio::test]
    async fn generated_certificates_bootstrap_and_complete_mutual_tls_handshake() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("controller");
        let remote = temp.path().join("worker");
        let db = Store::open(&root).unwrap();
        Store::open(&remote).unwrap();
        let d = discovery();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let c = NetworkConfig {
            provider: Provider::Direct,
            port: listener.local_addr().unwrap().port(),
            ..Default::default()
        };
        let parent = initialize(&root, c, &d).unwrap();
        let value = packet(&db, &parent, &d, "ts-worker").unwrap();
        assert!(value.get("credentials").is_none());
        assert!(value.get("settings").is_none());
        assert_eq!(value, packet(&db, &parent, &d, "ts-worker").unwrap());
        let fingerprint = crate::store::hash(
            pem::parse(value["certificate"].as_str().unwrap())
                .unwrap()
                .contents(),
        );
        db.conn
            .execute(
                "INSERT INTO runtime_enrollments VALUES('ts-worker',?,?,?,'pending')",
                rusqlite::params![
                    fingerprint,
                    crate::store::hash(
                        value["network"]["enrollment_token"]
                            .as_str()
                            .unwrap()
                            .as_bytes()
                    ),
                    now() + 900
                ],
            )
            .unwrap();
        crate::enrollment::apply_bootstrap(&remote, &value.to_string()).unwrap();
        let child = NetworkConfig::load(Some(&remote.join("managed-network.toml"))).unwrap();
        let server_parent = parent.clone();
        let server = tokio::spawn(async move {
            crate::network::serve_runtime(
                &server_parent,
                listener,
                std::future::pending::<()>(),
                root,
            )
            .await
        });
        let rogue_root = temp.path().join("unenrolled");
        Store::open(&rogue_root).unwrap();
        let rogue_packet = packet(&db, &parent, &d, "ts-unenrolled").unwrap();
        crate::enrollment::apply_bootstrap(&rogue_root, &rogue_packet.to_string()).unwrap();
        let rogue = NetworkConfig::load(Some(&rogue_root.join("managed-network.toml"))).unwrap();
        assert!(
            crate::network::probe(&rogue, "parent").await.is_err(),
            "CA membership alone must not grant access"
        );
        let control = tokio::spawn(crate::control::connect(remote, child));
        let mut connected = false;
        for _ in 0..100 {
            if let Ok(Some(reply)) =
                crate::control::call(&parent, "ts-worker", "capabilities", &json!({})).await
            {
                assert_eq!(reply["runtime"], "ts-worker");
                assert_eq!(reply["execution_available"], true);
                connected = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        if !connected && control.is_finished() {
            panic!("control failed: {:?}", control.await);
        }
        if !connected && server.is_finished() {
            panic!("server failed: {:?}", server.await);
        }
        assert!(
            connected,
            "generated trust did not establish reverse control"
        );
        assert_eq!(
            db.rows("SELECT state,token_hash FROM runtime_enrollments", &[])
                .unwrap()[0],
            json!({"state":"active","token_hash":""})
        );
        control.abort();
        server.abort();
        let _ = control.await;
        let _ = server.await;
    }
}
