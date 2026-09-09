//! Resolve missing CLI settings from the configured controller without changing its trust.
use super::super::ServerConfig;
use crate::network::{Discovery, NetworkConfig, Provider};
use anyhow::{Context, Result, ensure};
use std::{
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};
use x509_parser::{
    extensions::GeneralName,
    prelude::{FromDer, X509Certificate},
};

pub struct Overrides<'a> {
    pub listen: Option<SocketAddr>,
    pub controller_address: Option<SocketAddr>,
    pub tls_name: Option<&'a str>,
    pub issuer_key: Option<&'a Path>,
}

pub fn output_path(name: &str, explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_owned());
    }
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name != "."
            && name != ".."
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)),
        "use --output for fleet names containing spaces, path separators, or special characters"
    );
    Ok(PathBuf::from(format!("{name}.json")))
}

pub async fn resolve(
    network: &NetworkConfig,
    existing: Option<&ServerConfig>,
    overrides: Overrides<'_>,
) -> Result<ServerConfig> {
    let needs_discovery = existing.is_none()
        && network.provider == Provider::Tailscale
        && (overrides.listen.is_none()
            || overrides.controller_address.is_none()
            || overrides.tls_name.is_none());
    let discovery = if needs_discovery {
        Some(crate::network::discover(network).await.context("cannot infer controller defaults; check Tailscale or provide --listen, --controller-address, and --tls-name")?)
    } else {
        None
    };
    let listen = match overrides.listen.or(existing.map(|server| server.listen)) {
        Some(address) => address,
        None => {
            let port = network
                .port
                .checked_add(1)
                .context("runtime port has no following enrollment port; provide --listen")?;
            SocketAddr::new(
                local_address(network, discovery.as_ref(), "--listen")?,
                port,
            )
        }
    };
    let controller_address = match overrides
        .controller_address
        .or(existing.map(|server| server.controller_address))
    {
        Some(address) => address,
        None => SocketAddr::new(
            local_address(network, discovery.as_ref(), "--controller-address")?,
            network.port,
        ),
    };
    let tls_name = match overrides
        .tls_name
        .or(existing.map(|server| server.tls_name.as_str()))
    {
        Some(name) => name.to_owned(),
        None => certificate_name(network, discovery.as_ref())?,
    };
    let issuer_key = overrides
        .issuer_key
        .map(Path::to_owned)
        .or_else(|| existing.map(|server| server.issuer_key.clone()))
        .unwrap_or_else(|| network.ca_cert.with_file_name("ca.key"));
    Ok(ServerConfig {
        listen,
        controller_address,
        tls_name,
        issuer_key,
    })
}

fn local_address(
    network: &NetworkConfig,
    discovery: Option<&Discovery>,
    flag: &str,
) -> Result<IpAddr> {
    let address = if network.provider == Provider::Tailscale {
        *discovery
            .and_then(|status| status.local_addresses.first())
            .with_context(|| format!("no local Tailscale address; provide {flag}"))?
    } else {
        network.bind
    };
    ensure!(
        !address.is_unspecified() && !address.is_multicast(),
        "cannot infer a reachable address from the controller bind; provide {flag}"
    );
    Ok(address)
}

fn certificate_name(network: &NetworkConfig, discovery: Option<&Discovery>) -> Result<String> {
    let pem = pem::parse(std::fs::read(&network.identity_cert)?)?;
    let (_, certificate) = X509Certificate::from_der(pem.contents()).map_err(|_| {
        anyhow::anyhow!(
            "cannot read controller certificate; provide --tls-name after configuring its identity"
        )
    })?;
    let names: std::collections::BTreeSet<String> = certificate
        .subject_alternative_name()?
        .into_iter()
        .flat_map(|san| san.value.general_names.iter())
        .filter_map(|name| match name {
            GeneralName::DNSName(name) if !name.contains('*') => Some((*name).to_owned()),
            _ => None,
        })
        .collect();
    if let Some(discovery) = discovery
        && names.contains(&discovery.local_tls_name)
    {
        return Ok(discovery.local_tls_name.clone());
    }
    ensure!(
        names.len() == 1,
        "controller certificate has no unambiguous DNS name; provide --tls-name"
    );
    names.into_iter().next().context("provide --tls-name")
}
