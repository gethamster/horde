use horde::network::{self, NetworkConfig, Provider};
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, path::Path};

fn status() -> serde_json::Value {
    json!({"BackendState":"Running", "Self":{"ID":"self","TailscaleIPs":["100.64.0.1"]},
        "Peer":{
            "nodekey:a":{"ID":"n-a","DNSName":"worker.tail.test.","TailscaleIPs":["100.64.0.2","fd7a:115c:a1e0::2","192.168.1.9"],"Tags":["tag:horde"],"Online":true},
            "nodekey:b":{"ID":"n-b","DNSName":"private.tail.test.","TailscaleIPs":["100.64.0.3"],"Tags":null,"Online":true},
            "nodekey:c":{"ID":"n-c","DNSName":"offline.tail.test.","TailscaleIPs":["100.64.0.4"],"Tags":["tag:horde"],"Online":false}
        },"User":{"secret":"not returned"}})
}
#[test]
fn tailscale_discovery_filters_tags_and_routes_without_granting_authority() {
    let config = NetworkConfig {
        provider: Provider::Tailscale,
        ..Default::default()
    };
    let result =
        network::parse_tailscale(&serde_json::to_vec(&status()).unwrap(), &config).unwrap();
    assert_eq!(result.peers.len(), 2);
    assert_eq!(result.peers[0].id, "n-a");
    assert_eq!(result.peers[0].tls_name, "worker.tail.test");
    assert_eq!(result.peers[0].addresses.len(), 2);
    assert_eq!(result.peers[1].online, Some(false));
    assert!(!serde_json::to_string(&result).unwrap().contains("secret"));
    assert!(config.allowed_clients.is_empty());
}
#[test]
fn tailscale_discovery_fails_closed_on_invalid_status() {
    let config = NetworkConfig::default();
    for mutate in [
        |s: &mut serde_json::Value| s["BackendState"] = json!("NeedsLogin"),
        |s: &mut serde_json::Value| s["Self"]["TailscaleIPs"] = json!(["127.0.0.1"]),
        |s: &mut serde_json::Value| s["Peer"]["nodekey:a"]["DNSName"] = json!("evil.test/path"),
        |s: &mut serde_json::Value| s["Peer"]["nodekey:c"]["ID"] = json!("n-a"),
    ] {
        let mut value = status();
        mutate(&mut value);
        assert!(network::parse_tailscale(&serde_json::to_vec(&value).unwrap(), &config).is_err());
    }
    assert!(network::parse_tailscale(b"{}", &config).is_err());
    let mut empty = status();
    empty["Peer"] = serde_json::Value::Null;
    assert!(
        network::parse_tailscale(&serde_json::to_vec(&empty).unwrap(), &config)
            .unwrap()
            .peers
            .is_empty()
    );
}
#[test]
fn network_config_keeps_trust_separate_and_resolves_certificate_paths() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("network.toml");
    std::fs::write(&file, "provider = 'direct'\nidentity_key = 'tls/key.pem'\n").unwrap();
    let config = NetworkConfig::load(Some(&file)).unwrap();
    assert_eq!(
        config.identity_key,
        temp.path().canonicalize().unwrap().join("tls/key.pem")
    );
    assert!(NetworkConfig::load(Some(&temp.path().join("missing"))).is_err());
    for invalid in [
        "bind = '0.0.0.0'",
        "timeout_seconds = 0",
        "port = 0",
        "provider = 'automatic'",
        "unknown = true",
    ] {
        std::fs::write(&file, invalid).unwrap();
        assert!(NetworkConfig::load(Some(&file)).is_err());
    }
}
fn script(root: &Path, content: &str) -> std::path::PathBuf {
    let file = root.join("tailscale");
    std::fs::write(&file, format!("#!/bin/sh\n{content}\n")).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700)).unwrap();
    file
}
#[test]
fn network_cli_discovery_uses_only_user_selected_config_and_hides_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let program = script(
        temp.path(),
        &format!(
            "test -z \"$TUARA_API_KEY\" || exit 8\ncat <<'STATUS'\n{}\nSTATUS",
            status()
        ),
    );
    let config = NetworkConfig {
        provider: Provider::Tailscale,
        tailscale_program: program,
        ..Default::default()
    };
    let file = temp.path().join("network.toml");
    std::fs::write(&file, toml::to_string(&config).unwrap()).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_horde"))
        .args([
            "--data-dir",
            temp.path().to_str().unwrap(),
            "network",
            "--config",
            file.to_str().unwrap(),
            "peers",
        ])
        .env("TUARA_API_KEY", "fixture-secret")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["provider"], "tailscale");
    assert_eq!(result["peers"].as_array().unwrap().len(), 2);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("fixture-secret"));
}
#[tokio::test]
async fn tailscale_cli_is_read_only_and_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let program = script(
        temp.path(),
        &format!(
            "test \"$1\" = status && test \"$2\" = --json && test \"$#\" = 2 || exit 9\ncat <<'STATUS'\n{}\nSTATUS",
            status()
        ),
    );
    let config = NetworkConfig {
        provider: Provider::Tailscale,
        tailscale_program: program,
        timeout_seconds: 1,
        ..Default::default()
    };
    assert_eq!(network::discover(&config).await.unwrap().peers.len(), 2);
    script(temp.path(), "exit 1");
    assert!(
        network::discover(&config)
            .await
            .unwrap_err()
            .to_string()
            .contains("failed")
    );
    script(temp.path(), "exec sleep 10");
    assert!(
        network::discover(&config)
            .await
            .unwrap_err()
            .to_string()
            .contains("timed out")
    );
    script(temp.path(), "head -c 4194305 /dev/zero");
    assert!(
        network::discover(&config)
            .await
            .unwrap_err()
            .to_string()
            .contains("size limit")
    );
}
fn authority() -> (rcgen::Certificate, Issuer<'static, KeyPair>) {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let key = KeyPair::generate().unwrap();
    let certificate = params.self_signed(&key).unwrap();
    (certificate, Issuer::new(params, key))
}
fn identity(
    root: &Path,
    name: &str,
    ca: &rcgen::Certificate,
    issuer: &Issuer<'_, KeyPair>,
) -> (NetworkConfig, String) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec![name.into()]).unwrap();
    params.extended_key_usages = vec![
        rcgen::ExtendedKeyUsagePurpose::ServerAuth,
        rcgen::ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let certificate = params.signed_by(&key, issuer).unwrap();
    let cert = root.join(format!("{name}.pem"));
    let keyfile = root.join(format!("{name}.key"));
    let cafile = root.join(format!("{name}.ca.pem"));
    std::fs::write(&cert, certificate.pem()).unwrap();
    std::fs::write(&keyfile, key.serialize_pem()).unwrap();
    std::fs::set_permissions(&keyfile, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&cafile, ca.pem()).unwrap();
    (
        NetworkConfig {
            provider: Provider::Direct,
            ca_cert: cafile,
            identity_cert: cert,
            identity_key: keyfile,
            timeout_seconds: 2,
            ..Default::default()
        },
        hex::encode(Sha256::digest(certificate.der())),
    )
}
#[tokio::test]
async fn mtls_requires_enrollment_and_verifies_server_identity() {
    let temp = tempfile::tempdir().unwrap();
    let (ca, issuer) = authority();
    let (mut server, _) = identity(temp.path(), "server.test", &ca, &issuer);
    let (mut client, fingerprint) = identity(temp.path(), "client.test", &ca, &issuer);
    let (unenrolled, _) = identity(temp.path(), "unenrolled.test", &ca, &issuer);
    server.allowed_clients = BTreeMap::from([(fingerprint, "client-runtime".into())]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, wait) = tokio::sync::oneshot::channel();
    let step = tokio::spawn(async move {
        network::serve(&server, listener, async {
            let _ = wait.await;
        })
        .await
    });
    network::probe_address(&client, "server.test", address)
        .await
        .unwrap();
    assert!(
        network::probe_address(&client, "wrong.test", address)
            .await
            .is_err()
    );
    assert!(
        format!(
            "{:#}",
            network::probe_address(&unenrolled, "server.test", address)
                .await
                .unwrap_err()
        )
        .contains("not enrolled")
    );
    let (other_ca, other_issuer) = authority();
    let (mut untrusted, _) = identity(temp.path(), "untrusted.test", &other_ca, &other_issuer);
    untrusted.ca_cert = client.ca_cert.clone();
    assert!(
        network::probe_address(&untrusted, "server.test", address)
            .await
            .is_err()
    );
    // A CA-trusting client without a certificate cannot use even the health RPC.
    let no_identity = tonic::transport::Endpoint::from_shared(format!("https://{address}"))
        .unwrap()
        .tls_config(
            tonic::transport::ClientTlsConfig::new()
                .ca_certificate(tonic::transport::Certificate::from_pem(ca.pem()))
                .domain_name("server.test"),
        )
        .unwrap()
        .connect_timeout(std::time::Duration::from_secs(2))
        .timeout(std::time::Duration::from_secs(2))
        .connect()
        .await;
    if let Ok(channel) = no_identity {
        assert!(
            tonic_health::pb::health_client::HealthClient::new(channel)
                .check(tonic_health::pb::HealthCheckRequest {
                    service: network::HEALTH_SERVICE.into()
                })
                .await
                .is_err()
        );
    }
    client.peers.insert(
        "server".into(),
        network::DirectPeer {
            address,
            tls_name: "server.test".into(),
        },
    );
    let result = network::probe(&client, "server").await.unwrap();
    assert_eq!(result["mutual_tls"], true);
    assert_eq!(result["execution_available"], false);
    assert!(network::probe(&client, "missing").await.is_err());
    std::fs::set_permissions(&client.identity_key, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        network::probe_address(&client, "server.test", address)
            .await
            .unwrap_err()
            .to_string()
            .contains("chmod 600")
    );
    stop.send(()).unwrap();
    step.await.unwrap().unwrap();
}
#[tokio::test]
async fn network_is_disabled_and_listener_requires_explicit_enrollment() {
    assert!(network::discover(&NetworkConfig::default()).await.is_err());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = NetworkConfig {
        provider: Provider::Direct,
        ..Default::default()
    };
    assert!(
        network::serve(&config, listener, async {})
            .await
            .unwrap_err()
            .to_string()
            .contains("allowed_clients")
    );
}

#[test]
fn setup_discovery_includes_untagged_candidates_without_enrolling_them() {
    let config = NetworkConfig {
        provider: Provider::Tailscale,
        discover_all: true,
        ..Default::default()
    };
    let result =
        network::parse_tailscale(&serde_json::to_vec(&status()).unwrap(), &config).unwrap();
    assert_eq!(result.peers.len(), 3);
    assert_eq!(result.local_id, "self");
    assert!(config.allowed_clients.is_empty());
    assert!(config.execution_clients.is_empty());
}
