use horde::{
    fleet_enrollment::{Invitation, ServerConfig, authority, service},
    network::{NetworkConfig, Provider},
    store::Store,
};
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::Duration,
};
use tokio::{net::TcpListener, task::JoinHandle};

struct Fixture {
    dir: tempfile::TempDir,
    network: NetworkConfig,
    server: ServerConfig,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        Store::open(dir.path()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let runtime = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca = params.self_signed(&key).unwrap();
        let issuer = Issuer::new(params, key);
        let controller = KeyPair::generate().unwrap();
        let cert = CertificateParams::new(vec!["controller.test".into()])
            .unwrap()
            .signed_by(&controller, &issuer)
            .unwrap();
        let network = NetworkConfig {
            provider: Provider::Direct,
            runtime_id: format!("controller-{}", horde::store::id()),
            port: runtime.local_addr().unwrap().port(),
            timeout_seconds: 3,
            ca_cert: dir.path().join("ca.pem"),
            identity_cert: dir.path().join("controller.pem"),
            identity_key: dir.path().join("controller.key"),
            ..Default::default()
        };
        let server = ServerConfig {
            listen: listener.local_addr().unwrap(),
            controller_address: runtime.local_addr().unwrap(),
            tls_name: "controller.test".into(),
            issuer_key: dir.path().join("ca.key"),
        };
        for (path, contents) in [
            (&network.ca_cert, ca.pem()),
            (&network.identity_cert, cert.pem()),
            (&network.identity_key, controller.serialize_pem()),
            (&server.issuer_key, issuer.key().serialize_pem()),
        ] {
            horde::secrets::write_private(path, contents.as_bytes()).unwrap();
        }
        horde::federation::configure(dir.path(), &network).unwrap();
        let config = network.clone();
        let enrollment = server.clone();
        let root = dir.path().to_owned();
        let enrollment_task = tokio::spawn(async move {
            service::serve(&config, &enrollment, listener, root, std::future::pending())
                .await
                .unwrap();
        });
        let config = network.clone();
        let root = dir.path().to_owned();
        let runtime_task = tokio::spawn(async move {
            horde::network::serve_runtime(&config, runtime, std::future::pending(), root)
                .await
                .unwrap();
        });
        Self {
            dir,
            network,
            server,
            tasks: vec![enrollment_task, runtime_task],
        }
    }

    fn db(&self) -> Store {
        Store::open(self.dir.path()).unwrap()
    }

    fn invitation(&self) -> Invitation {
        authority::create_key(
            &self.db(),
            &self.network,
            &self.server,
            "sandboxes",
            3600,
            10,
            2,
        )
        .unwrap()
    }

    fn write_invitation(&self, invitation: &Invitation) -> std::path::PathBuf {
        let path = self.dir.path().join(format!("{}.json", invitation.key_id));
        horde::secrets::write_private(&path, &serde_json::to_vec(invitation).unwrap()).unwrap();
        path
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_root_preserves_existing_network_identity() {
    let f = Fixture::new().await;
    let invitation = f.write_invitation(&f.invitation());
    let root = f.dir.path().join("existing");
    Store::open(&root).unwrap();
    let network = NetworkConfig {
        runtime_id: "existing-runtime".into(),
        ..Default::default()
    };
    horde::federation::configure(&root, &network).unwrap();
    let before = std::fs::read(root.join("network-runtime.toml")).unwrap();
    let result = invoke_join(&f, Some(&root), &invitation, &["--no-start"]).await;
    assert!(!result.status.success());
    assert_eq!(
        std::fs::read(root.join("network-runtime.toml")).unwrap(),
        before
    );
}

struct Cleanup(Vec<PathBuf>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        for root in &self.0 {
            let _ = horde::daemon_client::request(root, "shutdown", serde_json::json!({}));
        }
        for _ in 0..100 {
            if self
                .0
                .iter()
                .all(|root| !horde::daemon_client::running(root))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn default_root(f: &Fixture) -> PathBuf {
    f.dir.path().join("home/.local/share/horde")
}
fn command(f: &Fixture, explicit_root: Option<&Path>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_horde"));
    command
        .env("HOME", f.dir.path().join("home"))
        .env("XDG_CONFIG_HOME", f.dir.path().join("user-config"))
        .env_remove("HORDE_WORKER_TOKEN")
        .env_remove("HORDE_BOOTSTRAP_JSON")
        .env_remove("HORDE_ENROLLMENT_JSON")
        .env_remove("HORDE_ENROLLMENT_FILE")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    if let Some(root) = explicit_root {
        command.arg("--data-dir").arg(root);
    }
    command
}
async fn invoke_join(
    f: &Fixture,
    root: Option<&Path>,
    invitation: &Path,
    flags: &[&str],
) -> Output {
    tokio::time::timeout(
        Duration::from_secs(25),
        command(f, root)
            .args(["network", "join"])
            .arg(invitation)
            .args(flags)
            .output(),
    )
    .await
    .unwrap()
    .unwrap()
}
fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
async fn wait_socket(root: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !horde::daemon_client::running(root) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn positional_join_connects_and_repeated_join_reuses_running_named_worker() {
    let f = Fixture::new().await;
    let user_config = f.dir.path().join("user-config/horde");
    std::fs::create_dir_all(&user_config).unwrap();
    std::fs::write(user_config.join("config.toml"), "[providers.fixture]\nkind='simulated'\nauth_mode='login'\nmodel='fixture-model'\n[executors.worker]\nprovider='fixture'\n").unwrap();
    let path = f.write_invitation(&f.invitation());
    let root = f.dir.path().join("worker");
    let _cleanup = Cleanup(vec![root.clone()]);
    let first = success(invoke_join(&f, Some(&root), &path, &["--name", "apollo"]).await);
    assert_eq!(first["connected"], true);
    assert_eq!(first["running"], true);
    assert_eq!(first["name"], "apollo");
    let capability_report = horde::management::value(
        &f.db(),
        &format!(
            "runtime_capabilities:{}",
            first["runtime"].as_str().unwrap()
        ),
    )
    .unwrap()
    .expect("capabilities must be stored before join is acknowledged");
    let capability_report: Value = serde_json::from_str(&capability_report).unwrap();
    assert!(capability_report["observed_at"].as_i64().unwrap() >= horde::store::now() - 2);
    assert!(
        capability_report["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|capability| capability["kind"] == "simulated"
                && capability["model"] == "fixture-model")
    );
    let certificate = std::fs::read(root.join("fleet-worker.json")).unwrap();
    let original_pid = horde::daemon_client::request(
        &root,
        "runtime_status",
        serde_json::json!({}),
    )
    .unwrap()["pid"]
        .clone();
    let again = success(invoke_join(&f, Some(&root), &path, &[]).await);
    assert_eq!(again["runtime"], first["runtime"]);
    assert_eq!(again["connected"], true);
    assert_eq!(again["name"], "apollo");
    assert_eq!(
        std::fs::read(root.join("fleet-worker.json")).unwrap(),
        certificate
    );
    assert_eq!(
        horde::daemon_client::request(&root, "runtime_status", serde_json::json!({})).unwrap()["pid"],
        original_pid
    );
    let setup = horde::agent_setup::run(&root, &serde_json::json!({"action":"join_worker","invitation_file":path,"explicit_root":true,"no_start":true})).await.unwrap();
    assert_eq!(setup["runtime"], first["runtime"]);
    assert_eq!(setup["connected"], true);
    let renamed = success(invoke_join(&f, Some(&root), &path, &["--name", "zephyr"]).await);
    assert_eq!(renamed["name"], "zephyr");
    assert_eq!(renamed["runtime"], first["runtime"]);
    assert_eq!(
        horde::runtime_directory::display_name(&f.db(), first["runtime"].as_str().unwrap())
            .unwrap()
            .as_deref(),
        Some("zephyr"),
        "join returned before the controller acknowledged its new name"
    );
    assert_eq!(
        std::fs::read(root.join("fleet-worker.json")).unwrap(),
        certificate
    );
    let other_invitation = f.write_invitation(&f.invitation());
    let rejected = invoke_join(&f, Some(&root), &other_invitation, &[]).await;
    assert!(!rejected.status.success());
    assert_eq!(
        std::fs::read(root.join("fleet-worker.json")).unwrap(),
        certificate
    );
    assert!(horde::daemon_client::running(&root));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_start_enrolls_without_daemon_and_invalid_name_enrolls_nothing() {
    let f = Fixture::new().await;
    let path = f.write_invitation(&f.invitation());
    let root = f.dir.path().join("worker");
    let invalid = invoke_join(
        &f,
        Some(&root),
        &path,
        &["--name", "../escape", "--no-start"],
    )
    .await;
    assert!(!invalid.status.success());
    assert!(!root.join("fleet-worker.json").exists());
    let result = success(invoke_join(&f, Some(&root), &path, &["--no-start"]).await);
    assert_eq!(result["enrolled"], true);
    assert_eq!(result["connected"], false);
    assert!(root.join("fleet-worker.json").exists());
    assert!(!horde::daemon_client::running(&root));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_join_creates_and_reuses_isolated_worker_preserving_existing_runtime() {
    let f = Fixture::new().await;
    let invitation = f.invitation();
    let path = f.write_invitation(&invitation);
    let root = default_root(&f);
    Store::open(&root).unwrap();
    let original = NetworkConfig {
        runtime_id: "personal-runtime".into(),
        ..Default::default()
    };
    horde::federation::configure(&root, &original).unwrap();
    let original_config = std::fs::read(root.join("network-runtime.toml")).unwrap();
    let isolated = root
        .join("fleet-workers")
        .join(&horde::store::hash(invitation.controller_id.as_bytes())[..24]);
    let _cleanup = Cleanup(vec![root.clone(), isolated.clone()]);
    let mut original_daemon = command(&f, None)
        .arg("daemon")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_socket(&root).await;
    let result = success(invoke_join(&f, None, &path, &["--name", "apollo"]).await);
    assert_eq!(
        Path::new(result["data_dir"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        isolated.canonicalize().unwrap()
    );
    assert_eq!(result["connected"], true);
    assert_eq!(
        std::fs::read(root.join("network-runtime.toml")).unwrap(),
        original_config
    );
    assert!(original_daemon.try_wait().unwrap().is_none());
    let repeated = success(invoke_join(&f, None, &path, &[]).await);
    assert_eq!(repeated["runtime"], result["runtime"]);
    assert_eq!(repeated["data_dir"], result["data_dir"]);
    assert!(!root.join("fleet-worker.json").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_join_preserves_user_network_and_explicit_root_refuses_it() {
    let f = Fixture::new().await;
    let invitation = f.invitation();
    let path = f.write_invitation(&invitation);
    let root = default_root(&f);
    let user = f.dir.path().join("user-config/horde");
    std::fs::create_dir_all(&user).unwrap();
    let original = toml::to_string(&f.network).unwrap();
    std::fs::write(user.join("network.toml"), &original).unwrap();
    let explicit = invoke_join(&f, Some(&root), &path, &["--no-start"]).await;
    assert!(!explicit.status.success());
    let result = success(invoke_join(&f, None, &path, &["--no-start"]).await);
    assert_ne!(result["data_dir"].as_str().unwrap(), root.to_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(user.join("network.toml")).unwrap(),
        original
    );
    assert!(!root.join("fleet-worker.json").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreachable_controller_reports_failure_after_successful_enrollment() {
    let f = Fixture::new().await;
    let invitation = f.invitation();
    // Keep enrollment reachable while removing the authenticated control service.
    f.tasks[1].abort();
    let path = f.write_invitation(&invitation);
    let root = f.dir.path().join("worker");
    let _cleanup = Cleanup(vec![root.clone()]);
    let output = invoke_join(&f, Some(&root), &path, &[]).await;
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("\"connected\":true"));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("could not connect") && error.contains("daemon.log"),
        "{error}"
    );
    assert!(root.join("fleet-worker.json").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_worker_name_and_controller_alias_conflict_are_reported_after_connection() {
    let f = Fixture::new().await;
    let invitation = f.write_invitation(&f.invitation());
    let first = f.dir.path().join("first");
    let second = f.dir.path().join("second");
    let _cleanup = Cleanup(vec![first.clone(), second.clone()]);
    let joined = success(invoke_join(&f, Some(&first), &invitation, &["--name", "apollo"]).await);
    let collision = invoke_join(&f, Some(&second), &invitation, &["--name", "apollo"]).await;
    assert!(!collision.status.success());
    assert!(String::from_utf8_lossy(&collision.stderr).contains("name apollo conflicts"));
    assert!(second.join("fleet-worker.json").exists());
    assert!(horde::daemon_client::running(&second));
    let recovered =
        success(invoke_join(&f, Some(&second), &invitation, &["--name", "zephyr"]).await);
    assert_eq!(recovered["connected"], true);
    horde::runtime_directory::rename(
        &f.db(),
        joined["runtime"].as_str().unwrap(),
        "controller-label",
    )
    .unwrap();
    let aliased = invoke_join(&f, Some(&first), &invitation, &["--name", "different-name"]).await;
    assert!(!aliased.status.success());
    assert!(String::from_utf8_lossy(&aliased.stderr).contains("controller alias"));
}
