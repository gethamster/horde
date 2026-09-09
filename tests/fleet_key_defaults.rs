use horde::{
    fleet_enrollment::{Invitation, ServerConfig},
    network::{NetworkConfig, Provider},
};
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use std::process::{Command, Output};

struct Fixture {
    dir: tempfile::TempDir,
    network: NetworkConfig,
}
impl Fixture {
    fn new(names: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let key = KeyPair::generate().unwrap();
        let mut parameters = CertificateParams::default();
        parameters.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        parameters.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca = parameters.self_signed(&key).unwrap();
        let issuer = Issuer::new(parameters, key);
        let controller = KeyPair::generate().unwrap();
        let leaf = CertificateParams::new(names.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .unwrap()
            .signed_by(&controller, &issuer)
            .unwrap();
        let network = NetworkConfig {
            provider: Provider::Direct,
            runtime_id: "controller".into(),
            bind: "192.0.2.5".parse().unwrap(),
            port: 7443,
            ca_cert: dir.path().join("ca.pem"),
            identity_cert: dir.path().join("controller.pem"),
            identity_key: dir.path().join("controller.key"),
            ..Default::default()
        };
        for (path, bytes) in [
            (&network.ca_cert, ca.pem()),
            (&network.identity_cert, leaf.pem()),
            (&network.identity_key, controller.serialize_pem()),
            (&dir.path().join("ca.key"), issuer.key().serialize_pem()),
        ] {
            horde::secrets::write_private(path, bytes.as_bytes()).unwrap();
        }
        Self { dir, network }
    }
    fn run(&self, args: &[&str]) -> Output {
        std::fs::write(
            self.dir.path().join("managed-network.toml"),
            toml::to_string(&self.network).unwrap(),
        )
        .unwrap();
        Command::new(env!("CARGO_BIN_EXE_horde"))
            .current_dir(self.dir.path())
            .env_remove("HORDE_WORKER_TOKEN")
            .env_remove("TUARA_WORKER_TOKEN")
            .args([
                "--data-dir",
                self.dir.path().to_str().unwrap(),
                "network",
                "key",
                "create",
            ])
            .args(args)
            .output()
            .unwrap()
    }
    fn invitation(&self, name: &str) -> Invitation {
        serde_json::from_slice(&std::fs::read(self.dir.path().join(name)).unwrap()).unwrap()
    }
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn rejected(output: &Output, flag: &str) {
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(flag),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn name_only_key_creation_infers_direct_controller_and_private_output() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new(&["controller.test"]);
    let result = f.run(&["workers"]);
    success(&result);
    let invite = f.invitation("workers.json");
    assert_eq!(invite.endpoint, "192.0.2.5:7444".parse().unwrap());
    assert_eq!(invite.controller_address, "192.0.2.5:7443".parse().unwrap());
    assert_eq!(invite.tls_name, "controller.test");
    assert_eq!(
        std::fs::metadata(f.dir.path().join("workers.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(!String::from_utf8_lossy(&result.stdout).contains(&invite.token));
    let duplicate = f.run(&["workers"]);
    rejected(&duplicate, "already exists");
}

#[test]
fn existing_server_settings_are_authoritative_defaults_without_discovery() {
    let mut f = Fixture::new(&["controller.test"]);
    f.network.provider = Provider::Tailscale;
    f.network.tailscale_program = "/does/not/exist".into();
    std::fs::rename(
        f.dir.path().join("ca.key"),
        f.dir.path().join("custom-issuer.key"),
    )
    .unwrap();
    let server = ServerConfig {
        listen: "127.0.0.1:8888".parse().unwrap(),
        controller_address: "192.0.2.7:9999".parse().unwrap(),
        tls_name: "controller.test".into(),
        issuer_key: f
            .dir
            .path()
            .join("custom-issuer.key")
            .canonicalize()
            .unwrap(),
    };
    let raw = toml::to_string(&server).unwrap();
    std::fs::write(f.dir.path().join("enrollment-server.toml"), &raw).unwrap();
    success(&f.run(&["workers"]));
    let invite = f.invitation("workers.json");
    assert_eq!(invite.endpoint, server.listen);
    assert_eq!(invite.controller_address, server.controller_address);
    assert_eq!(
        std::fs::read_to_string(f.dir.path().join("enrollment-server.toml")).unwrap(),
        raw
    );
    rejected(
        &f.run(&["other", "--listen", "127.0.0.1:8999"]),
        "different",
    );
}

#[test]
fn explicit_overrides_resolve_ambiguous_bind_name_and_port() {
    let mut f = Fixture::new(&["one.test", "two.test"]);
    f.network.port = u16::MAX;
    success(&f.run(&[
        "workers",
        "--listen",
        "127.0.0.1:8444",
        "--controller-address",
        "192.0.2.8:8443",
        "--tls-name",
        "two.test",
        "--output",
        "custom.json",
        "--enrollment-address",
        "192.0.2.9:9444",
    ]));
    let invite = f.invitation("custom.json");
    assert_eq!(invite.endpoint, "192.0.2.9:9444".parse().unwrap());
    assert_eq!(invite.tls_name, "two.test");
    assert!(!f.dir.path().join("workers.json").exists());
}

#[test]
fn ambiguous_defaults_require_actionable_flags() {
    let mut bind = Fixture::new(&["controller.test"]);
    bind.network.bind = "0.0.0.0".parse().unwrap();
    rejected(
        &bind.run(&["workers"]),
        "bind must be a specific local address",
    );
    let names = Fixture::new(&["one.test", "two.test"]);
    rejected(&names.run(&["workers"]), "--tls-name");
    let mut port = Fixture::new(&["controller.test"]);
    port.network.port = u16::MAX;
    rejected(&port.run(&["workers"]), "--listen");
    let f = Fixture::new(&["controller.test"]);
    rejected(&f.run(&["../workers"]), "--output");
}

#[test]
fn tailscale_defaults_use_mocked_local_discovery_without_ssh() {
    use std::os::unix::fs::PermissionsExt;
    let mut f = Fixture::new(&["node.example.ts.net", "other.test"]);
    f.network.provider = Provider::Tailscale;
    let mock = f.dir.path().join("tailscale");
    std::fs::write(&mock,"#!/bin/sh\n[ \"$1\" = status ] && [ \"$2\" = --json ] || exit 9\nprintf '%s' '{\"BackendState\":\"Running\",\"Self\":{\"ID\":\"self\",\"DNSName\":\"node.example.ts.net.\",\"TailscaleIPs\":[\"100.64.0.7\",\"fd7a:115c:a1e0::7\"]}}'\n").unwrap();
    std::fs::set_permissions(&mock, std::fs::Permissions::from_mode(0o700)).unwrap();
    f.network.tailscale_program = mock;
    success(&f.run(&["workers"]));
    let invite = f.invitation("workers.json");
    assert_eq!(invite.endpoint, "100.64.0.7:7444".parse().unwrap());
    assert_eq!(
        invite.controller_address,
        "100.64.0.7:7443".parse().unwrap()
    );
    assert_eq!(invite.tls_name, "node.example.ts.net");
}

#[test]
fn explicit_config_remains_rejected_for_controller_key_creation() {
    let f = Fixture::new(&["controller.test"]);
    let explicit = f.dir.path().join("explicit.toml");
    std::fs::write(&explicit, toml::to_string(&f.network).unwrap()).unwrap();
    rejected(
        &f.run(&["workers", "--config", explicit.to_str().unwrap()]),
        "omit --config",
    );
    assert!(!f.dir.path().join("workers.json").exists());
}
