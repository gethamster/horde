use super::{ServerConfig, admin, authority};
use crate::{network::NetworkConfig, store::Store};
use anyhow::{Context, Result, ensure};
use clap::Subcommand;
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Subcommand)]
pub enum KeyCommands {
    /// Create a fleet credential in a private file for automatic worker startup.
    Create {
        name: String,
        /// Reachable enrollment address; Horde binds this local address.
        #[arg(long)]
        listen: SocketAddr,
        /// Public enrollment address when different from --listen (for example, NAT).
        #[arg(long)]
        enrollment_address: Option<SocketAddr>,
        /// Reachable address of the controller's existing runtime listener.
        #[arg(long)]
        controller_address: SocketAddr,
        /// DNS name in the controller's TLS certificate.
        #[arg(long)]
        tls_name: String,
        /// CA signing key; defaults to ca.key beside the configured CA certificate.
        #[arg(long)]
        issuer_key: Option<PathBuf>,
        /// New private credential file; existing files are never overwritten.
        #[arg(long)]
        output: PathBuf,
        /// Seconds during which new workers may join (default: 30 days).
        #[arg(long, default_value_t = 2_592_000)]
        expires_in: i64,
        /// Total distinct workers this credential may enroll; retries use no extra slot.
        #[arg(long, default_value_t = 100)]
        max_workers: usize,
        /// Maximum concurrent work per enrolled runtime.
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
    },
    /// List fleet credentials and enrollment counts, without their secrets.
    List,
    /// Stop new admissions using this credential; existing workers keep their identities.
    Revoke { id: String },
}

pub fn run(root: &Path, file: Option<&Path>, command: &KeyCommands) -> Result<Value> {
    admin()?;
    let db = Store::open(root)?;
    match command {
        KeyCommands::List => authority::list_keys(&db),
        KeyCommands::Revoke { id } => {
            authority::revoke_key(&db, id)?;
            Ok(json!({"key":id,"revoked":true}))
        }
        KeyCommands::Create {
            name,
            listen,
            enrollment_address,
            controller_address,
            tls_name,
            issuer_key,
            output,
            expires_in,
            max_workers,
            concurrency,
        } => {
            let managed = root.join("managed-network.toml");
            let network = NetworkConfig::load(
                file.or_else(|| managed.exists().then_some(managed.as_path())),
            )?;
            ensure!(
                network.provider != crate::network::Provider::Disabled
                    && network.controller_peer.is_none(),
                "configure a network controller before creating a fleet credential"
            );
            ensure!(
                listen.port() != network.port,
                "enrollment must use a separate port from the runtime listener"
            );
            let server = ServerConfig {
                listen: *listen,
                issuer_key: issuer_key
                    .clone()
                    .unwrap_or_else(|| network.ca_cert.with_file_name("ca.key")),
                controller_address: *controller_address,
                tls_name: tls_name.clone(),
            };
            let server = ServerConfig {
                issuer_key: server
                    .issuer_key
                    .canonicalize()
                    .context("read controller CA signing key")?,
                ..server
            };
            let server_path = root.join("enrollment-server.toml");
            if server_path.exists() {
                let existing: ServerConfig =
                    toml::from_str(&std::fs::read_to_string(&server_path)?)?;
                ensure!(
                    existing == server,
                    "fleet enrollment already uses different listener or trust settings"
                );
            }
            ensure!(
                !output.try_exists()? && std::fs::symlink_metadata(output).is_err(),
                "credential output already exists"
            );
            let invitation = db.atomic(|| {
                let created = authority::create_key(
                    &db,
                    &network,
                    &server,
                    name,
                    *expires_in,
                    *max_workers,
                    *concurrency,
                )?;
                let invitation = super::Invitation {
                    endpoint: enrollment_address.unwrap_or(created.endpoint),
                    ..created
                };
                invitation.validate()?;
                crate::secrets::write_private(output, &serde_json::to_vec(&invitation)?)?;
                if !server_path.exists() {
                    crate::secrets::write_private(
                        &server_path,
                        toml::to_string(&server)?.as_bytes(),
                    )?;
                }
                Ok(invitation)
            })?;
            Ok(
                json!({"key":invitation.key_id,"fleet":name,"credential_file":output,"enrollment_address":invitation.endpoint,"max_workers":max_workers,"next":"Run horde start on the controller. Supply the credential file to workers with HORDE_ENROLLMENT_FILE and start horde daemon."}),
            )
        }
    }
}
