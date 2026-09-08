//! Optional network discovery and authenticated gRPC connectivity.
//! Discovery is not authorization; runtime execution requires separate enrollment.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{io::AsyncReadExt, net::TcpListener, process::Command};
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint, Identity, Server, ServerTlsConfig};

pub const HEALTH_SERVICE: &str = "task.network.v1";
const MAX_STATUS: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Disabled,
    Direct,
    Tailscale,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkConfig {
    pub provider: Provider,
    pub runtime_id: String,
    pub delegate_peers: Vec<String>,
    pub execution_clients: Vec<String>,
    pub management_clients: Vec<String>,
    pub controller_peer: Option<String>,
    pub enrollment_token: Option<String>,
    pub share_bundles: BTreeMap<String, Vec<String>>,
    pub receive_bundles: BTreeMap<String, Vec<String>>,
    pub port: u16,
    pub timeout_seconds: u64,
    pub tailscale_program: PathBuf,
    pub discovery_tag: String,
    /// Include untagged candidates during user-directed pairing; never grants access.
    pub discover_all: bool,
    pub bind: IpAddr,
    pub ca_cert: PathBuf,
    pub identity_cert: PathBuf,
    pub identity_key: PathBuf,
    /// SHA-256 of allowed client leaf certificates (DER), mapped to runtime IDs.
    pub allowed_clients: BTreeMap<String, String>,
    pub peers: BTreeMap<String, DirectPeer>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectPeer {
    pub address: SocketAddr,
    pub tls_name: String,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            provider: Provider::Disabled,
            runtime_id: "local".into(),
            delegate_peers: vec![],
            execution_clients: vec![],
            management_clients: vec![],
            controller_peer: None,
            enrollment_token: None,
            share_bundles: BTreeMap::new(),
            receive_bundles: BTreeMap::new(),
            port: 7443,
            timeout_seconds: 10,
            tailscale_program: "tailscale".into(),
            discovery_tag: "tag:horde".into(),
            discover_all: false,
            bind: "127.0.0.1".parse().unwrap(),
            ca_cert: PathBuf::new(),
            identity_cert: PathBuf::new(),
            identity_key: PathBuf::new(),
            allowed_clients: BTreeMap::new(),
            peers: BTreeMap::new(),
        }
    }
}

impl NetworkConfig {
    /// Network authority is user-owned and never read from repository settings.
    pub fn load(file: Option<&Path>) -> Result<Self> {
        let explicit = file.is_some();
        let default = crate::branding::config_dir().join("network.toml");
        let file = file.unwrap_or(&default);
        if !file.exists() {
            ensure!(
                !explicit,
                "network configuration does not exist: {}",
                file.display()
            );
            return Ok(Self::default());
        }
        let mut config: Self = toml::from_str(&std::fs::read_to_string(file)?)
            .context("invalid network configuration")?;
        let canonical_file = file.canonicalize()?;
        let parent = canonical_file
            .parent()
            .context("network configuration has no parent")?;
        for path in [
            &mut config.ca_cert,
            &mut config.identity_cert,
            &mut config.identity_key,
        ] {
            if !path.as_os_str().is_empty() && path.is_relative() {
                *path = parent.join(&*path);
            }
        }
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.port != 0, "network port must be nonzero");
        ensure!(
            (1..=120).contains(&self.timeout_seconds),
            "network timeout must be between 1 and 120 seconds"
        );
        ensure!(
            self.discovery_tag.starts_with("tag:") && self.discovery_tag.len() > 4,
            "discovery_tag must be a nonempty Tailscale tag"
        );
        ensure!(
            !self.bind.is_unspecified() && !self.bind.is_multicast(),
            "bind must be a specific local address"
        );
        for (fingerprint, runtime) in &self.allowed_clients {
            ensure!(
                fingerprint.len() == 64
                    && fingerprint
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                    && !runtime.is_empty(),
                "allowed_clients requires lowercase SHA-256 certificate fingerprints and nonempty runtime IDs"
            );
        }
        for peer in self.peers.values() {
            ensure!(
                peer.address.port() != 0
                    && !peer.address.ip().is_unspecified()
                    && !peer.address.ip().is_multicast(),
                "invalid direct peer address"
            );
            ensure!(valid_dns(&peer.tls_name), "invalid peer TLS DNS name");
        }
        Ok(())
    }
    fn duration(&self) -> Duration {
        Duration::from_secs(self.timeout_seconds)
    }
    fn identity(&self) -> Result<Identity> {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            std::fs::metadata(&self.identity_key)
                .context("read network identity key metadata")?
                .permissions()
                .mode()
                & 0o077
                == 0,
            "network identity key must not be accessible to group or other users (use chmod 600)"
        );
        Ok(Identity::from_pem(
            std::fs::read(&self.identity_cert).context("read network identity certificate")?,
            std::fs::read(&self.identity_key).context("read network identity key")?,
        ))
    }
    fn ca(&self) -> Result<Certificate> {
        Ok(Certificate::from_pem(
            std::fs::read(&self.ca_cert).context("read network CA certificate")?,
        ))
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Peer {
    pub id: String,
    pub tls_name: String,
    pub addresses: Vec<SocketAddr>,
    pub online: Option<bool>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Discovery {
    pub provider: Provider,
    pub local_addresses: Vec<IpAddr>,
    pub local_id: String,
    pub local_tls_name: String,
    pub peers: Vec<Peer>,
}

#[derive(Deserialize)]
struct TailStatus {
    #[serde(rename = "BackendState")]
    state: String,
    #[serde(rename = "Self")]
    local: TailPeer,
    #[serde(rename = "Peer")]
    peers: Option<BTreeMap<String, TailPeer>>,
}
#[derive(Deserialize)]
struct TailPeer {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "DNSName", default)]
    dns: String,
    #[serde(rename = "TailscaleIPs", default)]
    ips: Vec<IpAddr>,
    #[serde(rename = "Tags", default)]
    tags: Option<Vec<String>>,
    #[serde(rename = "Online", default)]
    online: bool,
}
fn tailnet_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let b = ip.octets();
            b[0] == 100 && (64..=127).contains(&b[1])
        }
        IpAddr::V6(ip) => ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}
fn valid_dns(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
}
pub fn parse_tailscale(bytes: &[u8], config: &NetworkConfig) -> Result<Discovery> {
    config.validate()?;
    ensure!(
        bytes.len() as u64 <= MAX_STATUS,
        "Tailscale status exceeds size limit"
    );
    let status: TailStatus =
        serde_json::from_slice(bytes).context("unsupported or malformed Tailscale status JSON")?;
    ensure!(
        status.state == "Running",
        "Tailscale is not running (state: {}); connect it before using Horde networking",
        status.state
    );
    let local_addresses: Vec<_> = status.local.ips.into_iter().filter(tailnet_ip).collect();
    ensure!(
        !local_addresses.is_empty(),
        "Tailscale has no usable local tailnet address"
    );
    let mut peers = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    for peer in status.peers.unwrap_or_default().into_values() {
        if peer.id == status.local.id
            || !config.discover_all
                && !peer
                    .tags
                    .unwrap_or_default()
                    .contains(&config.discovery_tag)
        {
            continue;
        }
        let tls_name = peer.dns.trim_end_matches('.').to_lowercase();
        ensure!(
            !peer.id.is_empty() && ids.insert(peer.id.clone()),
            "missing or duplicate Tailscale node identity"
        );
        ensure!(
            valid_dns(&tls_name),
            "Tailscale candidate has no valid MagicDNS name"
        );
        let addresses: Vec<_> = peer
            .ips
            .into_iter()
            .filter(tailnet_ip)
            .map(|ip| SocketAddr::new(ip, config.port))
            .collect();
        if !addresses.is_empty() {
            peers.push(Peer {
                id: peer.id,
                tls_name,
                addresses,
                online: Some(peer.online),
            });
        }
    }
    peers.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(Discovery {
        provider: Provider::Tailscale,
        local_id: status.local.id,
        local_tls_name: status.local.dns.trim_end_matches('.').to_lowercase(),
        local_addresses,
        peers,
    })
}

pub async fn discover(config: &NetworkConfig) -> Result<Discovery> {
    config.validate()?;
    match config.provider {
        Provider::Disabled => bail!(
            "networking is disabled; configure the direct or tailscale provider in network.toml"
        ),
        Provider::Direct => Ok(Discovery {
            provider: Provider::Direct,
            local_id: config.runtime_id.clone(),
            local_tls_name: String::new(),
            local_addresses: vec![config.bind],
            peers: config
                .peers
                .iter()
                .map(|(id, p)| Peer {
                    id: id.clone(),
                    tls_name: p.tls_name.clone(),
                    addresses: vec![p.address],
                    online: None,
                })
                .collect(),
        }),
        Provider::Tailscale => {
            // No shell, enrollment key, provider credentials, or raw status in output.
            let mut command = Command::new(&config.tailscale_program);
            command
                .args(["status", "--json"])
                .env_clear()
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true);
            for name in ["PATH", "HOME", "USER", "TMPDIR", "XDG_RUNTIME_DIR"] {
                if let Some(value) = std::env::var_os(name) {
                    command.env(name, value);
                }
            }
            let mut child = command
                .spawn()
                .context("cannot run Tailscale; install its CLI or set tailscale_program")?;
            let mut stdout = child
                .stdout
                .take()
                .context("missing Tailscale stdout")?
                .take(MAX_STATUS + 1);
            let operation = async {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).await?;
                ensure!(
                    bytes.len() as u64 <= MAX_STATUS,
                    "Tailscale status exceeds size limit"
                );
                ensure!(
                    child.wait().await?.success(),
                    "Tailscale status failed; verify the client is connected and accessible"
                );
                parse_tailscale(&bytes, config)
            };
            tokio::time::timeout(config.duration(), operation)
                .await
                .context("Tailscale discovery timed out")?
        }
    }
}

pub async fn bind_listener(config: &NetworkConfig) -> Result<TcpListener> {
    let discovery = discover(config).await?;
    let ip = match config.provider {
        Provider::Tailscale => *discovery
            .local_addresses
            .first()
            .context("missing local Tailscale address")?,
        _ => config.bind,
    };
    TcpListener::bind(SocketAddr::new(ip, config.port)).await
        .context("cannot bind network listener; Tailscale requires host networking or a TUN-enabled sidecar")
}

pub async fn serve(
    config: &NetworkConfig,
    listener: TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    serve_inner(config, listener, shutdown, None).await
}
pub async fn serve_runtime(
    config: &NetworkConfig,
    listener: TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    root: PathBuf,
) -> Result<()> {
    serve_inner(config, listener, shutdown, Some(root)).await
}
async fn serve_inner(
    config: &NetworkConfig,
    listener: TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    root: Option<PathBuf>,
) -> Result<()> {
    config.validate()?;
    ensure!(
        config.provider != Provider::Disabled,
        "networking is disabled"
    );
    let address = listener.local_addr()?;
    ensure!(
        match config.provider {
            Provider::Tailscale => tailnet_ip(&address.ip()),
            Provider::Direct => address.ip() == config.bind,
            Provider::Disabled => false,
        },
        "listener address does not match network provider"
    );
    ensure!(
        !config.allowed_clients.is_empty() || root.is_some(),
        "configure allowed_clients before starting a standalone network listener"
    );
    let tls = ServerTlsConfig::new()
        .identity(config.identity()?)
        .client_ca_root(config.ca()?)
        .client_auth_optional(false)
        .timeout(config.duration());
    let (reporter, health) = tonic_health::server::health_reporter();
    reporter
        .set_service_status(HEALTH_SERVICE, tonic_health::ServingStatus::Serving)
        .await;
    let allowed = config.allowed_clients.clone();
    let enrollment_root = root.clone();
    let guard = move |mut request: tonic::Request<()>| -> std::result::Result<_, tonic::Status> {
        let connection = request.extensions().get::<tonic::transport::server::TlsConnectInfo<tonic::transport::server::TcpConnectInfo>>()
            .ok_or_else(|| tonic::Status::unauthenticated("mutual TLS required"))?;
        let certs = connection
            .peer_certs()
            .ok_or_else(|| tonic::Status::unauthenticated("client certificate required"))?;
        let cert = certs
            .first()
            .ok_or_else(|| tonic::Status::unauthenticated("client certificate required"))?;
        let fingerprint = hex::encode(Sha256::digest(cert.as_ref()));
        let identity = allowed.get(&fingerprint).cloned().or_else(|| {
            enrollment_root
                .as_ref()
                .and_then(|root| crate::store::Store::open(root).ok())
                .and_then(|db| {
                    crate::enrollment::identity(&db, &fingerprint)
                        .ok()
                        .flatten()
                })
        });
        let identity = identity
            .ok_or_else(|| tonic::Status::permission_denied("runtime certificate not enrolled"))?;
        request
            .extensions_mut()
            .insert(crate::federation::PeerIdentity(identity));
        Ok(request)
    };
    let federation =
        root.map(|root| crate::federation::service(root, config.clone(), guard.clone()));
    Server::builder()
        .tls_config(tls)?
        .timeout(config.duration())
        .concurrency_limit_per_connection(16)
        .add_service(tonic::service::interceptor::InterceptedService::new(
            health, guard,
        ))
        .add_optional_service(federation)
        .serve_with_incoming_shutdown(
            tokio_stream::wrappers::TcpListenerStream::new(listener),
            shutdown,
        )
        .await?;
    Ok(())
}

pub async fn probe(config: &NetworkConfig, id: &str) -> Result<serde_json::Value> {
    let discovery = discover(config).await?;
    let peer = discovery.peers.into_iter().find(|p| p.id == id).context(
        "peer not found in configured discovery; use its stable node ID from network peers",
    )?;
    let mut last = None;
    for address in &peer.addresses {
        let result = probe_address(config, &peer.tls_name, *address).await;
        match result {
            Ok(()) => {
                let capabilities =
                    crate::federation::call(config, id, "capabilities", serde_json::json!({}))
                        .await
                        .ok();
                return Ok(
                    serde_json::json!({"peer":peer.id,"address":address,"tls_name":peer.tls_name,"mutual_tls":true,"service":HEALTH_SERVICE,"status":"serving","execution_available":capabilities.as_ref().is_some_and(|v|v["execution_available"]==true)}),
                );
            }
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("peer has no usable address")))
}

pub async fn probe_address(
    config: &NetworkConfig,
    tls_name: &str,
    address: SocketAddr,
) -> Result<()> {
    config.validate()?;
    ensure!(
        config.provider != Provider::Disabled,
        "networking is disabled"
    );
    ensure!(valid_dns(tls_name), "invalid peer TLS DNS name");
    ensure!(
        config.provider != Provider::Tailscale || tailnet_ip(&address.ip()),
        "Tailscale transport requires a tailnet address"
    );
    let tls = ClientTlsConfig::new()
        .ca_certificate(config.ca()?)
        .identity(config.identity()?)
        .domain_name(tls_name);
    let operation = async {
        let channel = Endpoint::from_shared(format!("https://{address}"))?
            .tls_config(tls)?
            .connect_timeout(config.duration())
            .timeout(config.duration())
            .connect()
            .await?;
        let mut client = tonic_health::pb::health_client::HealthClient::new(channel);
        let result = client
            .check(tonic_health::pb::HealthCheckRequest {
                service: HEALTH_SERVICE.into(),
            })
            .await?
            .into_inner();
        ensure!(
            result.status == tonic_health::pb::health_check_response::ServingStatus::Serving as i32,
            "peer network service is not serving"
        );
        Ok(())
    };
    tokio::time::timeout(config.duration(), operation)
        .await
        .context("network probe timed out")?
}

pub async fn channel(config: &NetworkConfig, peer_id: &str) -> Result<tonic::transport::Channel> {
    let peer = discover(config)
        .await?
        .peers
        .into_iter()
        .find(|p| p.id == peer_id)
        .context("configured peer not discovered")?;
    let mut last = None;
    for address in peer.addresses {
        let tls = ClientTlsConfig::new()
            .ca_certificate(config.ca()?)
            .identity(config.identity()?)
            .domain_name(&peer.tls_name);
        match Endpoint::from_shared(format!("https://{address}"))?
            .tls_config(tls)?
            .connect_timeout(config.duration())
            .timeout(config.duration())
            .connect()
            .await
        {
            Ok(c) => return Ok(c),
            Err(e) => last = Some(e),
        }
    }
    Err(last
        .map(anyhow::Error::from)
        .unwrap_or_else(|| anyhow::anyhow!("peer has no address")))
}
