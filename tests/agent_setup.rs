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
    if let Ok(expected) = std::env::var("HORDE_EXPECT_EFFECTIVE_KEY") {
        assert_eq!(
            horde::config::credential("TUARA_API_KEY").unwrap(),
            expected
        );
    }
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
fn missing_tuara_key_offers_mcp_wallet_onboarding() {
    let f = Fixture::new();
    let directory = f.dir.path().join("config/horde");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("config.toml"),
        "[providers.default]\nkind='tuara'\nauth_mode='api'\nbase_url='https://tuara.com/router/v1'\napi_key_env='TUARA_API_KEY'\nmodel='auto'\n[executors.worker]\nprovider='default'\n").unwrap();
    let report = f.run(json!({"action":"inspect"}), &[]);
    assert!(
        report["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action["tool"] == "provider_wallet"
                && action["arguments"]["action"] == "inspect"),
        "{report}"
    );
    assert_eq!(report["parent"]["connection"], "admin_mcp");
    assert_eq!(report["parent"]["client"], "any_mcp_client");
    assert_eq!(report["billing"]["provider"], "default");
    assert!(report["billing"]["wallet"]["installed"].is_boolean());
    assert!(
        report["children"]["roles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|role| role["role"] == "worker" && role["provider"] == "default")
    );
}

#[test]
fn worker_roles_can_be_assigned_over_mcp_before_billing_without_replacing_credentials() {
    let f = Fixture::new();
    let config_dir = f.dir.path().join("config/horde");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.toml"), "# keep this note\n[providers.default]\nkind='tuara'\nauth_mode='api'\nbase_url='https://tuara.com/router/v1'\napi_key_env='TUARA_API_KEY'\nmodel='existing-model'\n[executors.worker]\n").unwrap();
    let assigned = f.run(json!({"action":"configure_workers","provider":"default","roles":["planner","worker","reviewer"]}), &[]);
    assert_eq!(assigned["status"], "configured", "{assigned}");
    assert_eq!(assigned["credential"], "missing");
    let saved = std::fs::read_to_string(config_dir.join("config.toml")).unwrap();
    assert!(saved.contains("# keep this note"));
    assert!(saved.contains("model='existing-model'"));
    assert!(saved.contains("[executors.planner]"));
    assert!(saved.contains("[executors.reviewer]"));
    assert!(!config_dir.join("credentials.env").exists());
    use std::os::unix::fs::PermissionsExt;
    let credential_file = config_dir.join("credentials.env");
    std::fs::write(&credential_file, "TUARA_API_KEY='existing-secret'\n").unwrap();
    std::fs::set_permissions(&credential_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let reassigned = f.run(
        json!({"action":"configure_workers","provider":"default","roles":["worker"]}),
        &[],
    );
    assert_eq!(reassigned["status"], "configured", "{reassigned}");
    assert_eq!(
        std::fs::read_to_string(&credential_file).unwrap(),
        "TUARA_API_KEY='existing-secret'\n"
    );
    assert!(!reassigned.to_string().contains("existing-secret"));
    let invalid = f.run(
        json!({"action":"configure_workers","provider":"default","roles":["../escape"]}),
        &[],
    );
    assert!(invalid.get("error").is_some());
    assert!(
        !std::fs::read_to_string(config_dir.join("config.toml"))
            .unwrap()
            .contains("escape")
    );
}

#[test]
fn parent_repository_setup_is_guided_through_admin_mcp() {
    let f = Fixture::new();
    let repo = f.dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let git = f.dir.path().join("bin/git");
    std::fs::write(&git, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o700)).unwrap();
    let project_config = repo.join(".horde");
    std::fs::create_dir(&project_config).unwrap();
    std::fs::write(
        project_config.join("horde.toml"),
        "[executors.worker]\nprovider='claude'\n",
    )
    .unwrap();
    let first = f.run(json!({"action":"inspect","repo":repo}), &[]);
    assert_eq!(
        first["parent"]["repository"]["status"], "unregistered",
        "{first}"
    );
    assert!(
        first["children"]["roles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|role| role["role"] == "worker" && role["provider"] == "claude")
    );
    assert!(
        first["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| {
                action["tool"] == "project_repo_add" && action["arguments"]["project"] == "default"
            })
    );
    let db = Store::open(&f.root).unwrap();
    horde::protocol::dispatch(
        &db,
        "project_repo_add",
        json!({"project":"default","path":repo}),
        None,
    )
    .unwrap();
    let second = f.run(json!({"action":"inspect","repo":repo}), &[]);
    assert_eq!(second["parent"]["repository"]["status"], "registered");
    assert!(
        !second["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action["tool"] == "project_repo_add")
    );
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
fn direct_provider_key_addition_and_replacement_are_private_and_preserve_other_accounts() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let config_dir = f.dir.path().join("config/horde");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("config.toml"), "# local settings\n[providers.local-fixture]\nkind='simulated'\nauth_mode='login'\nmodel='local-model'\n").unwrap();
    std::fs::write(
        config_dir.join("credentials.env"),
        "# another account\nOTHER_API_KEY='keep-this-key'\n",
    )
    .unwrap();
    std::fs::set_permissions(
        config_dir.join("credentials.env"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    for secret in ["first-direct-key", "replacement-direct-key"] {
        let report = f.run(json!({"action":"configure_provider","provider":"tuara","credential":secret,"roles":["worker"]}), &[]);
        assert_eq!(report["status"], "configured", "{report}");
        assert_eq!(report["credential_activation"], "next_invocation");
        assert_eq!(report["restart_required"], false);
        assert_eq!(report["provider_api"], "not_probed");
        assert!(!report.to_string().contains(secret));
        let config = std::fs::read_to_string(config_dir.join("config.toml")).unwrap();
        assert!(config.contains("# local settings"));
        assert!(config.contains("[providers.local-fixture]"));
        assert!(!config.contains(secret));
        let credentials = std::fs::read_to_string(config_dir.join("credentials.env")).unwrap();
        assert!(credentials.contains("# another account"));
        let values = horde::secrets::parse(&credentials).unwrap();
        assert_eq!(values["OTHER_API_KEY"], "keep-this-key");
        assert_eq!(values["TUARA_API_KEY"], secret);
        assert_eq!(
            std::fs::metadata(config_dir.join("credentials.env"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        assert!(
            !f.run(json!({"action":"inspect"}), &[])
                .to_string()
                .contains(secret)
        );
    }
}

#[test]
fn direct_provider_key_rejects_invalid_content_and_conflicting_sources_without_writes() {
    let f = Fixture::new();
    for credential in [
        String::new(),
        "   ".into(),
        "invalid\nkey".into(),
        "invalid\rkey".into(),
        "invalid\0key".into(),
        "x".repeat(16385),
    ] {
        let report = f.run(
            json!({"action":"configure_provider","provider":"tuara","credential":credential}),
            &[],
        );
        assert!(report.get("error").is_some(), "{report}");
        assert!(!f.dir.path().join("config/horde/config.toml").exists());
    }
    for source in [
        json!({"credential_env":"FIXTURE_KEY"}),
        json!({"credential_file":"/not/read"}),
    ] {
        let mut args = json!({"action":"configure_provider","provider":"tuara","credential":"never-echo-this"});
        args.as_object_mut()
            .unwrap()
            .extend(source.as_object().unwrap().clone());
        let report = f.run(args, &[]);
        assert!(report.get("error").is_some(), "{report}");
        assert!(!report.to_string().contains("never-echo-this"));
        assert!(!f.dir.path().join("config/horde/config.toml").exists());
    }
}

#[test]
fn credential_fields_are_rejected_on_unrelated_setup_actions() {
    let f = Fixture::new();
    for source in [
        json!({"credential":"never-echo-this"}),
        json!({"credential_env":"FIXTURE_KEY"}),
        json!({"credential_file":"/not/read"}),
    ] {
        let mut args = json!({"action":"inspect"});
        args.as_object_mut()
            .unwrap()
            .extend(source.as_object().unwrap().clone());
        let report = f.run(args, &[]);
        assert!(report.get("error").is_some(), "{report}");
        assert!(!report.to_string().contains("never-echo-this"));
    }
}

#[test]
fn supplied_key_overrides_inherited_environment_without_restart_or_echo() {
    let f = Fixture::new();
    let report = f.run(json!({"action":"configure_provider","provider":"tuara","credential":"replacement-direct-key"}), &[("TUARA_API_KEY", "old-inherited-key")]);
    assert_eq!(report["status"], "configured", "{report}");
    assert_eq!(report["restart_required"], false);
    assert_eq!(report["credential_activation"], "next_invocation");
    assert!(!report.to_string().contains("replacement-direct-key"));
    assert!(!report.to_string().contains("old-inherited-key"));
    f.run(
        json!({"action":"inspect"}),
        &[
            ("TUARA_API_KEY", "old-inherited-key"),
            ("HORDE_EXPECT_EFFECTIVE_KEY", "replacement-direct-key"),
        ],
    );
    let matching = f.run(json!({"action":"configure_provider","provider":"tuara","credential":"replacement-direct-key"}), &[("TUARA_API_KEY", "replacement-direct-key")]);
    assert_eq!(matching["restart_required"], false);
}

#[test]
fn only_an_effective_key_change_invalidates_previous_provider_capacity() {
    let f = Fixture::new();
    let args =
        json!({"action":"configure_provider","provider":"tuara","credential":"first-direct-key"});
    assert_eq!(f.run(args.clone(), &[])["status"], "configured");
    let db = Store::open(&f.root).unwrap();
    let account = "tuara:api:https://tuara.com/router/v1:TUARA_API_KEY";
    db.conn.execute(
        "INSERT INTO account_capacity(account,provider,window,used,reset,observed,source) VALUES(?,'tuara','provider-window',100,?,?,'provider')",
        rusqlite::params![account, horde::store::now() + 3600, horde::store::now()],
    ).unwrap();
    db.conn.execute(
        "INSERT INTO account_capacity(account,provider,window,used,reset,observed,source) VALUES(?,'tuara','budget-window',100,?,?,'local_budget')",
        rusqlite::params![account, horde::store::now() + 3600, horde::store::now()],
    ).unwrap();
    let provider_used = || -> Option<f64> {
        use rusqlite::OptionalExtension;
        db.conn
            .query_row(
                "SELECT used FROM account_capacity WHERE account=? AND window='provider-window'",
                [account],
                |row| row.get(0),
            )
            .optional()
            .unwrap()
    };
    assert_eq!(f.run(args, &[])["status"], "configured");
    assert_eq!(provider_used(), Some(100.0));
    let replacement = json!({"action":"configure_provider","provider":"tuara","credential":"replacement-direct-key"});
    assert_eq!(
        f.run(
            replacement.clone(),
            &[("TUARA_API_KEY", "first-direct-key")]
        )["status"],
        "configured"
    );
    assert_eq!(provider_used(), None);
    // Restore the stored old key so the final request changes the effective key.
    assert_eq!(f.run(json!({"action":"configure_provider","provider":"tuara","credential":"first-direct-key"}), &[("TUARA_API_KEY", "first-direct-key")])["status"], "configured");
    assert_eq!(f.run(replacement, &[])["status"], "configured");
    assert_eq!(provider_used(), None);
    let budget: f64 = db
        .conn
        .query_row(
            "SELECT used FROM account_capacity WHERE account=? AND window='budget-window'",
            [account],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(budget, 100.0);
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
