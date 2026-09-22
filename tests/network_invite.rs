use horde::network::{NetworkConfig, Provider};
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use std::{
    os::unix::fs::PermissionsExt,
    process::{Command, Output},
};

struct Fixture {
    dir: tempfile::TempDir,
    network: NetworkConfig,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let key = KeyPair::generate().unwrap();
        let mut parameters = CertificateParams::default();
        parameters.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        parameters.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca = parameters.self_signed(&key).unwrap();
        let issuer = Issuer::new(parameters, key);
        let controller = KeyPair::generate().unwrap();
        let leaf = CertificateParams::new(vec!["controller.example.ts.net".to_string()])
            .unwrap()
            .signed_by(&controller, &issuer)
            .unwrap();
        let network = NetworkConfig {
            provider: Provider::Tailscale,
            runtime_id: "controller".into(),
            discover_all: true,
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
    /// A mock `tailscale` that answers `status --json` with one online peer
    /// (`worker.example.ts.net`) and `file cp` per `outcome` ("ok" or "fail").
    fn mock_tailscale(&mut self, outcome: &str) {
        let mock = self.dir.path().join("tailscale");
        std::fs::write(
            &mock,
            format!(
                "#!/bin/sh\n\
                 if [ \"$1\" = status ] && [ \"$2\" = --json ]; then\n\
                 printf '%s' '{{\"BackendState\":\"Running\",\"Self\":{{\"ID\":\"self\",\"DNSName\":\"controller.example.ts.net.\",\"TailscaleIPs\":[\"100.64.0.7\"]}},\"Peer\":{{\"w\":{{\"ID\":\"worker-id\",\"DNSName\":\"worker.example.ts.net.\",\"TailscaleIPs\":[\"100.64.0.8\"],\"Online\":true}}}}}}'\n\
                 exit 0\n\
                 fi\n\
                 if [ \"$1\" = file ] && [ \"$2\" = cp ]; then\n\
                 [ \"{outcome}\" = ok ] && exit 0 || exit 1\n\
                 fi\n\
                 exit 9\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&mock, std::fs::Permissions::from_mode(0o700)).unwrap();
        self.network.tailscale_program = mock;
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
                "invite",
            ])
            .args(args)
            .output()
            .unwrap()
    }
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn rejected(output: &Output, needle: &str) {
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(needle),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn successful_invite_sends_the_credential_and_deletes_the_local_copy() {
    let mut f = Fixture::new();
    f.mock_tailscale("ok");
    let result = f.run(&["worker"]);
    success(&result);
    let reply: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(reply["peer"], "worker.example.ts.net");
    assert!(
        !reply["next"]
            .as_str()
            .unwrap()
            .contains("horde network join")
    );
    assert_eq!(
        reply["join_command"],
        "horde network join ~/Downloads/horde-invite-worker.json"
    );
    assert!(
        !f.dir
            .path()
            .join("invites")
            .join("horde-invite-worker.json")
            .exists()
    );
    assert!(f.dir.path().join("enrollment-server.toml").exists());
}

#[test]
fn failed_send_keeps_the_credential_and_reports_the_manual_retry_command() {
    let mut f = Fixture::new();
    f.mock_tailscale("fail");
    let result = f.run(&["worker"]);
    rejected(&result, "tailscale file cp");
    assert!(
        f.dir
            .path()
            .join("invites")
            .join("horde-invite-worker.json")
            .exists()
    );
}

#[test]
fn short_hostname_and_full_name_both_resolve_the_same_peer() {
    let mut f = Fixture::new();
    f.mock_tailscale("ok");
    success(&f.run(&["worker.example.ts.net"]));
    success(&f.run(&["worker"]));
}

#[test]
fn unknown_peer_is_rejected_before_any_credential_is_created() {
    let mut f = Fixture::new();
    f.mock_tailscale("ok");
    let result = f.run(&["nobody"]);
    rejected(&result, "no peer matching 'nobody'");
    assert!(!f.dir.path().join("invites").exists());
}

#[test]
fn direct_provider_is_rejected_before_any_tailscale_call() {
    let mut f = Fixture::new();
    f.network.provider = Provider::Direct;
    f.network.tailscale_program = "/does/not/exist".into();
    rejected(&f.run(&["worker"]), "requires the tailscale provider");
}
