use horde::store::Store;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

#[tokio::test]
async fn child_action() {
    let Ok(root) = std::env::var("HORDE_SETUP_TEST_ROOT") else {
        return;
    };
    let args: Value =
        serde_json::from_str(&std::env::var("HORDE_SETUP_TEST_ARGS").unwrap()).unwrap();
    let result = horde::agent_setup::run(Path::new(&root), &args).await;
    let output = match result {
        Ok(value) => value,
        Err(error) => json!({"error":error.to_string()}),
    };
    std::fs::write(
        Path::new(&root).join("result.json"),
        serde_json::to_vec(&output).unwrap(),
    )
    .unwrap();
}

struct Fixture {
    dir: tempfile::TempDir,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let root = dir.path().join("data");
        Store::open(&root).unwrap();
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        Self { dir, root }
    }
    fn run(&self, args: Value, environment: &[(&str, &str)]) -> Value {
        let result = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child_action"])
            .env("HORDE_SETUP_TEST_ROOT", &self.root)
            .env("HORDE_SETUP_TEST_ARGS", args.to_string())
            .env("HOME", self.dir.path().join("home"))
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("PATH", self.dir.path().join("bin"))
            .env_remove("HORDE_WORKER_TOKEN")
            .env_remove("TUARA_API_KEY")
            .env_remove("HORDE_ENROLLMENT_FILE")
            .env_remove("HORDE_ENROLLMENT_JSON")
            .envs(environment.iter().copied())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice(&std::fs::read(self.root.join("result.json")).unwrap()).unwrap()
    }
    fn tailscale(&self, status: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = self.dir.path().join("bin/tailscale");
        std::fs::write(&path, format!("#!/bin/sh\n[ \"$1 $2\" = 'status --json' ] || exit 2\ncat <<'JSON'\n{status}\nJSON\n").replace("cat <<", "/bin/cat <<")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[test]
fn discovery_reports_missing_access_and_agent_execution_options_without_secret_values() {
    let f = Fixture::new();
    let report = f.run(json!({"action":"inspect"}), &[]);
    assert_eq!(report["status"], "blocked");
    assert_eq!(report["access"]["ssh_required"], false);
    assert!(!report["next_actions"].as_array().unwrap().is_empty());
    assert!(!f.dir.path().join("config/horde/config.toml").exists());
}

#[test]
fn provider_setup_consumes_existing_secret_reference_and_preserves_unrelated_configuration() {
    let f = Fixture::new();
    let config_dir = f.dir.path().join("config/horde");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.toml"), "# preserve this local note\n[providers.local-fixture]\nkind='simulated'\nauth_mode='login'\nmodel='local-model'\n").unwrap();
    let configured = f.run(json!({"action":"configure_provider","provider":"tuara","credential_env":"FIXTURE_PROVIDER_KEY","roles":["worker"]}), &[("FIXTURE_PROVIDER_KEY", "fixture-secret-never-output")]);
    assert_eq!(configured["status"], "configured", "{configured}");
    assert!(
        !configured
            .to_string()
            .contains("fixture-secret-never-output")
    );
    let config = std::fs::read_to_string(f.dir.path().join("config/horde/config.toml")).unwrap();
    assert!(!config.contains("fixture-secret-never-output"));
    assert!(config.contains("# preserve this local note"));
    assert!(config.contains("[providers.local-fixture]"));
    let credentials =
        std::fs::read_to_string(f.dir.path().join("config/horde/credentials.env")).unwrap();
    assert!(credentials.contains("fixture-secret-never-output"));
    let verify = f.run(json!({"action":"verify"}), &[]);
    assert!(!verify.to_string().contains("fixture-secret-never-output"));
    assert_eq!(verify["checks"]["provider_api"], "not_probed");
}

#[test]
fn missing_credential_and_unrecognized_secret_argument_write_no_configuration() {
    let f = Fixture::new();
    let report = f.run(
        json!({"action":"configure_provider","provider":"tuara","credential_env":"ABSENT_KEY"}),
        &[],
    );
    assert_eq!(report["status"], "blocked");
    assert!(!f.dir.path().join("config/horde/config.toml").exists());
    let rejected = f.run(
        json!({"action":"configure_provider","provider":"tuara","key":"do-not-expose"}),
        &[],
    );
    assert!(rejected.get("error").is_some());
    assert!(!rejected.to_string().contains("do-not-expose"));
}

#[test]
fn authenticated_discovery_configures_controller_and_returns_generic_private_bootstrap() {
    let f = Fixture::new();
    f.tailscale(r#"{"BackendState":"Running","Self":{"ID":"controller-fixture","DNSName":"controller.test.","TailscaleIPs":["100.64.0.7"]}}"#);
    let report = f.run(json!({"action":"create_fleet_key","name":"sandboxes"}), &[]);
    assert_eq!(report["status"], "configured", "{report}");
    assert_eq!(report["bootstrap"]["argv"], json!(["horde", "daemon"]));
    let file = PathBuf::from(report["credential_file"].as_str().unwrap());
    let invitation: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    assert!(
        !report
            .to_string()
            .contains(invitation["token"].as_str().unwrap())
    );
    assert_eq!(report["controller_ready"], false);
    assert!(f.root.join("managed-network.toml").exists());
}

#[test]
fn unavailable_tailnet_authentication_reports_blocker_without_creating_trust() {
    let f = Fixture::new();
    f.tailscale(r#"{"BackendState":"NeedsLogin","Self":{"ID":"","TailscaleIPs":[]}}"#);
    let report = f.run(json!({"action":"configure_controller"}), &[]);
    assert_eq!(report["status"], "blocked");
    assert!(!f.root.join("managed-network.toml").exists());
    assert!(!report["next_actions"].as_array().unwrap().is_empty());
}

#[test]
fn custom_native_provider_uses_discovered_endpoint_model_and_secret_reference() {
    let f = Fixture::new();
    let report = f.run(json!({"action":"configure_provider","provider":"glm","kind":"tuara","auth_mode":"api","base_url":"https://models.example.invalid/v1","api_key_env":"GLM_API_KEY","model":"glm-4.7","credential_env":"SETUP_KEY","roles":["worker"]}), &[("SETUP_KEY","custom-provider-secret")]);
    assert_eq!(report["status"], "configured", "{report}");
    let config = std::fs::read_to_string(f.dir.path().join("config/horde/config.toml")).unwrap();
    assert!(config.contains("https://models.example.invalid/v1"));
    assert!(config.contains("glm-4.7"));
    assert!(!report.to_string().contains("custom-provider-secret"));
    assert!(!config.contains("custom-provider-secret"));
}

#[test]
fn constrained_setup_reports_unpinned_model_without_claiming_provider_verification() {
    let f = Fixture::new();
    let report = f.run(
        json!({"action":"inspect","model":"specific-required-model"}),
        &[],
    );
    assert!(
        report["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["code"] == "model_not_configured")
    );
    assert!(
        report["presets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == "tuara")
    );
    assert_ne!(report["status"], "verified");
}

#[test]
fn setup_refuses_task_scoped_credentials_and_missing_worker_invitation() {
    let f = Fixture::new();
    let denied = f.run(
        json!({"action":"inspect"}),
        &[("HORDE_WORKER_TOKEN", "task-token")],
    );
    assert!(denied.get("error").is_some());
    let missing = f.run(
        json!({"action":"join_worker","invitation_file":"/missing/fleet.json"}),
        &[],
    );
    assert_eq!(missing["blockers"][0]["code"], "invitation_missing");
}

#[test]
fn custom_harness_program_is_used_for_preflight_and_authentication_instructions() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let program = f.dir.path().join("my-codex");
    std::fs::write(&program, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let report = f.run(json!({"action":"configure_provider","provider":"custom-codex","kind":"codex","auth_mode":"login","program":program,"model":"specific-model","roles":["worker"]}), &[]);
    assert_eq!(report["status"], "configured", "{report}");
    let inspect = f.run(json!({"action":"inspect","provider":"custom-codex"}), &[]);
    assert!(
        !inspect["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["code"] == "harness_missing"),
        "{inspect}"
    );
    assert!(
        inspect["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["argv"] == json!([program, "login", "status"]))
    );
}

#[test]
fn private_credential_file_is_consumed_but_public_or_invalid_file_is_not_written() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let secret = f.dir.path().join("secret");
    std::fs::write(&secret, "file-secret\n").unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();
    let args = json!({"action":"configure_provider","provider":"tuara","credential_file":secret});
    assert!(f.run(args.clone(), &[]).get("error").is_some());
    assert!(!f.dir.path().join("config/horde/config.toml").exists());
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    let result = f.run(args, &[]);
    assert_eq!(result["status"], "configured");
    assert!(!result.to_string().contains("file-secret"));
}

#[test]
fn controller_setup_preserves_invalid_existing_and_worker_network_identities() {
    for (body, code) in [
        ("invalid = [".to_owned(), "existing_network_invalid"),
        (
            toml::to_string(&horde::network::NetworkConfig::default()).unwrap(),
            "existing_network_identity",
        ),
        (
            toml::to_string(&horde::network::NetworkConfig {
                controller_peer: Some("upstream".into()),
                ..Default::default()
            })
            .unwrap(),
            "worker_identity_present",
        ),
    ] {
        let f = Fixture::new();
        let file = f.root.join("managed-network.toml");
        std::fs::write(&file, &body).unwrap();
        let report = f.run(json!({"action":"configure_controller"}), &[]);
        assert_eq!(report["blockers"][0]["code"], code, "{report}");
        assert_eq!(std::fs::read_to_string(file).unwrap(), body);
    }
}

#[test]
fn restart_is_an_external_plan_and_refuses_active_or_uncertain_work() {
    let f = Fixture::new();
    let idle = f.run(json!({"action":"restart_local"}), &[]);
    assert_eq!(idle["status"], "action_required");
    assert!(
        idle["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action["kind"] == "exec")
    );
    let db = Store::open(&f.root).unwrap();
    let plan = horde::template::compile(
        "simulated",
        &horde::template::load_templates(f.dir.path()).unwrap(),
        [("task".into(), "fixture".into())].into(),
    )
    .unwrap();
    let task = db
        .submit("fixture", f.dir.path(), &Default::default(), &plan)
        .unwrap();
    let step = db.steps(&task).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    for state in ["running", "uncertain"] {
        db.conn
            .execute(
                "INSERT OR REPLACE INTO attempts(id,step,state,started) VALUES('fixture',?,?,0)",
                rusqlite::params![step, state],
            )
            .unwrap();
        let report = f.run(json!({"action":"restart_local"}), &[]);
        assert_eq!(report["blockers"][0]["code"], "runtime_busy");
    }
}

#[test]
fn setup_rpc_keeps_the_serving_daemon_responsive_and_returns_restart_actions() {
    struct Daemon(std::process::Child);
    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let f = Fixture::new();
    let mut daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_horde"))
            .arg("--data-dir")
            .arg(&f.root)
            .arg("daemon")
            .env("HOME", f.dir.path().join("home"))
            .env("XDG_CONFIG_HOME", f.dir.path().join("config"))
            .env("PATH", f.dir.path().join("bin"))
            .env_remove("HORDE_WORKER_TOKEN")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !horde::daemon_client::running(&f.root) {
        assert!(daemon.0.try_wait().unwrap().is_none());
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let report =
        horde::daemon_client::request(&f.root, "agent_setup", json!({"action":"inspect"})).unwrap();
    assert_eq!(report["checks"]["local_daemon"], "responsive");
    assert_eq!(report["daemon"]["pid"], daemon.0.id());
    let setup = horde::daemon_client::request(
        &f.root,
        "agent_setup",
        json!({"action":"configure_controller"}),
    )
    .unwrap();
    assert_eq!(setup["blockers"][0]["code"], "controller_restart_required");
    assert!(
        setup["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action["argv"]
                .as_array()
                .is_some_and(|args| args.contains(&json!("setup"))))
    );
    assert!(
        daemon.0.try_wait().unwrap().is_none(),
        "setup stopped its own serving daemon"
    );
}

#[test]
fn existing_runtime_controller_identity_is_reused_when_issuing_a_fleet_key() {
    let f = Fixture::new();
    f.tailscale(r#"{"BackendState":"Running","Self":{"ID":"controller-fixture","DNSName":"controller.test.","TailscaleIPs":["100.64.0.7"]}}"#);
    assert_eq!(
        f.run(json!({"action":"configure_controller"}), &[])["status"],
        "configured"
    );
    let managed = f.root.join("managed-network.toml");
    let runtime = f.root.join("network-runtime.toml");
    std::fs::rename(&managed, &runtime).unwrap();
    let original = std::fs::read(&runtime).unwrap();
    let result = f.run(json!({"action":"create_fleet_key","name":"portable"}), &[]);
    assert_eq!(result["status"], "configured", "{result}");
    assert_eq!(std::fs::read(runtime).unwrap(), original);
    assert!(!managed.exists());
}
