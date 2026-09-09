//! Provider-independent enrollment. Fleet credentials authorize admission only.
pub mod authority;
pub mod cli;
pub mod service;
pub mod worker;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

pub const MAX_PACKET: usize = 64 * 1024;
pub const CERT_LIFETIME: i64 = 24 * 60 * 60;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invitation {
    pub version: u32,
    pub key_id: String,
    pub token: String,
    pub endpoint: SocketAddr,
    pub tls_name: String,
    pub ca_pem: String,
    pub controller_id: String,
    pub controller_address: SocketAddr,
    pub controller_fingerprint: String,
}

impl Invitation {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == 1,
            "unsupported enrollment invitation version"
        );
        ensure!(
            !self.key_id.is_empty() && self.key_id.len() <= 128,
            "invalid enrollment key ID"
        );
        ensure!(self.token.len() <= 256, "invalid enrollment credential");
        ensure!(
            !self.controller_id.is_empty() && self.controller_id.len() <= 128,
            "invalid controller ID"
        );
        ensure!(
            !self.tls_name.is_empty()
                && self.tls_name.len() <= 253
                && self
                    .tls_name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b".-".contains(&c)),
            "invalid controller TLS name"
        );
        for address in [self.endpoint, self.controller_address] {
            ensure!(
                address.port() != 0
                    && !address.ip().is_unspecified()
                    && !address.ip().is_multicast(),
                "invalid controller address"
            );
        }
        ensure!(
            self.controller_fingerprint.len() == 64
                && self
                    .controller_fingerprint
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit()),
            "invalid controller fingerprint"
        );
        ensure!(
            self.ca_pem.len() <= 16 * 1024 && pem::parse(&self.ca_pem)?.tag() == "CERTIFICATE",
            "invalid controller CA"
        );
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub listen: SocketAddr,
    pub issuer_key: PathBuf,
    pub controller_address: SocketAddr,
    pub tls_name: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Certificate {
    pub runtime_id: String,
    pub certificate_pem: String,
    pub expires: i64,
    pub renew_after: i64,
    pub concurrency: usize,
}

pub fn admin() -> Result<()> {
    ensure!(
        crate::branding::var_os("HORDE_WORKER_TOKEN").is_none(),
        "fleet enrollment requires administrative access"
    );
    Ok(())
}

pub(crate) fn replace_private(path: &Path, data: &[u8]) -> Result<()> {
    let temp = path.with_extension(format!("{}.tmp", crate::store::id()));
    crate::secrets::write_private(&temp, data)?;
    std::fs::rename(&temp, path)?;
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}
