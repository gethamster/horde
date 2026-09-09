#[path = "defaults.rs"]
mod defaults;

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
        /// Local enrollment address (default: saved listener or controller address, next port).
        #[arg(long)]
        listen: Option<SocketAddr>,
        /// Public enrollment address when different from --listen (for example, NAT).
        #[arg(long)]
        enrollment_address: Option<SocketAddr>,
        /// Reachable runtime address (default: saved settings or configured controller).
        #[arg(long)]
        controller_address: Option<SocketAddr>,
        /// Controller TLS DNS name (default: saved settings or unambiguous certificate name).
        #[arg(long)]
        tls_name: Option<String>,
        /// CA signing key; defaults to ca.key beside the configured CA certificate.
        #[arg(long)]
        issuer_key: Option<PathBuf>,
        /// New private credential file (default: NAME.json); never overwrites existing files.
        #[arg(long)]
        output: Option<PathBuf>,
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

pub async fn run(root: &Path, file: Option<&Path>, command: &KeyCommands) -> Result<Value> {
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
            let output = defaults::output_path(name, output.as_deref())?;
            let server_path = root.join("enrollment-server.toml");
            let existing = if server_path.exists() {
                Some(toml::from_str::<ServerConfig>(&std::fs::read_to_string(
                    &server_path,
                )?)?)
            } else {
                None
            };
            let server = defaults::resolve(
                &network,
                existing.as_ref(),
                defaults::Overrides {
                    listen: *listen,
                    controller_address: *controller_address,
                    tls_name: tls_name.as_deref(),
                    issuer_key: issuer_key.as_deref(),
                },
            )
            .await?;
            ensure!(
                server.listen.port() != network.port,
                "enrollment must use a separate port from the runtime listener"
            );
            let server = ServerConfig {
                issuer_key: server
                    .issuer_key
                    .canonicalize()
                    .context("read controller CA signing key")?,
                ..server
            };
            if let Some(existing) = existing {
                ensure!(
                    existing == server,
                    "fleet enrollment already uses different listener or trust settings"
                );
            }
            ensure!(
                !output.try_exists()? && std::fs::symlink_metadata(&output).is_err(),
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
                crate::secrets::write_private(&output, &serde_json::to_vec(&invitation)?)?;
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
