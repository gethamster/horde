use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false"
            ])
            .args(args)
            .current_dir(repo)
            .status()
            .unwrap()
            .success()
    );
}
fn fixture() -> TempDir {
    let dir = tempfile::Builder::new()
        .prefix("hi-")
        .tempdir_in("/tmp")
        .unwrap();
    let bin = dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    for name in ["codex", "claude", "mock-provider"] {
        use std::os::unix::fs::PermissionsExt;
        let program = bin.join(name);
        fs::write(&program, "#!/bin/sh\nexit 97\n").unwrap();
        fs::set_permissions(program, fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_horde"), bin.join("horde")).unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.name", "Init Test"]);
    git(&repo, &["config", "user.email", "init@example.invalid"]);
    git(&repo, &["commit", "--allow-empty", "-qm", "initial"]);
    dir
}
fn init_command(dir: &TempDir, extra: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_horde"));
    command
        .args(["--data-dir"])
        .arg(dir.path().join("data"))
        .args(["init", "--agent", "codex", "--delegate", "always"])
        .args(extra)
        .current_dir(dir.path().join("repo"))
        .env_clear()
        // Keep coverage output when testing an instrumented subprocess.
        .envs(std::env::var_os("LLVM_PROFILE_FILE").map(|value| ("LLVM_PROFILE_FILE", value)))
        .env(
            "PATH",
            format!("{}:{}", dir.path().join("bin").display(), "/usr/bin:/bin"),
        )
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env_remove("HORDE_WORKER_TOKEN");
    command
}
fn init(dir: &TempDir, extra: &[&str]) -> Output {
    init_command(dir, extra).output().unwrap()
}
#[test]
fn initializes_repository_and_reports_missing_credentials_without_claiming_readiness() {
    let dir = fixture();
    let output = init(&dir, &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["installed"], true);
    assert_eq!(report["simulation"]["status"], "passed");
    assert_eq!(report["ready"], false);
    assert!(!report["next_steps"].as_array().unwrap().is_empty());
    assert!(dir.path().join("repo/AGENTS.md").exists());
    assert!(!dir.path().join("config/horde/config.toml").exists());
    assert!(!dir.path().join("data/daemon.sock").exists());
}
#[test]
fn rejects_non_repository_before_installing_files() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("repo")).unwrap();
    let output = init(&dir, &[]);
    assert!(!output.status.success());
    assert!(!dir.path().join("repo/AGENTS.md").exists());
}
#[test]
fn worker_cannot_install_a_personal_agent_bridge() {
    let dir = fixture();
    let output = Command::new(env!("CARGO_BIN_EXE_horde"))
        .args(["init", "--agent", "codex", "--delegate", "always"])
        .current_dir(dir.path().join("repo"))
        .env_clear()
        // Keep coverage output when testing an instrumented subprocess.
        .envs(std::env::var_os("LLVM_PROFILE_FILE").map(|value| ("LLVM_PROFILE_FILE", value)))
        .env(
            "PATH",
            format!("{}:{}", dir.path().join("bin").display(), "/usr/bin:/bin"),
        )
        .env("HOME", dir.path())
        .env("HORDE_WORKER_TOKEN", "test-worker-token")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!dir.path().join("repo/AGENTS.md").exists());
}

struct RunningDaemon<'a>(&'a TempDir);
impl Drop for RunningDaemon<'_> {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_horde"))
            .arg("--data-dir")
            .arg(self.0.path().join("data"))
            .arg("stop")
            .env_remove("HORDE_WORKER_TOKEN")
            .output();
    }
}

#[test]
fn ready_setup_starts_and_reuses_daemon_without_calling_a_provider() {
    let dir = fixture();
    let config =
        "[providers.default]\nkind = 'codex'\nauth_mode = 'login'\nprogram = 'mock-provider'\n";
    fs::write(dir.path().join("repo/.horde.toml"), config).unwrap();
    let _daemon = RunningDaemon(&dir);
    let first = init(&dir, &[]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(report["ready"], true);
    assert_eq!(report["daemon"]["running"], true);
    assert_eq!(report["provider_authentication"], "not_probed");
    assert!(dir.path().join("data/daemon.sock").exists());
    let second = init(&dir, &[]);
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(report["files"]["changed_files"], serde_json::json!([]));
    assert_eq!(
        fs::read_to_string(dir.path().join("repo/.horde.toml")).unwrap(),
        config
    );
}

#[test]
fn subdirectory_setup_targets_git_root_and_invalid_settings_write_nothing() {
    let dir = fixture();
    let sub = dir.path().join("repo/nested");
    fs::create_dir(&sub).unwrap();
    let output = init(&dir, &["--repo", sub.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(dir.path().join("repo/AGENTS.md").exists());
    assert!(!sub.join("AGENTS.md").exists());
    let invalid = fixture();
    fs::write(
        invalid.path().join("repo/.horde.toml"),
        "unknown_option = true\n",
    )
    .unwrap();
    let output = init(&invalid, &[]);
    assert!(!output.status.success());
    assert!(!invalid.path().join("repo/AGENTS.md").exists());
}

#[test]
fn known_executor_configuration_failures_are_reported_as_not_ready() {
    for (config, expected) in [
        (
            "allow_commands = false\n[providers.default]\nkind = 'codex'\nauth_mode = 'login'\nprogram = 'mock-provider'\n",
            "allow_commands",
        ),
        (
            "[providers.default]\nkind = 'codex'\nauth_mode = 'login'\nprogram = 'mock-provider'\nmax_api_cost_usd = 1.0\n",
            "max_api_cost_usd",
        ),
        (
            "[providers.default]\nkind = 'tuara'\nauth_mode = 'login'\n",
            "credentials",
        ),
        (
            "[providers.default]\nkind = 'unknown-provider'\nauth_mode = 'login'\n",
            "unsupported",
        ),
    ] {
        let dir = fixture();
        fs::write(dir.path().join("repo/.horde.toml"), config).unwrap();
        let _daemon = RunningDaemon(&dir);
        let output = init(&dir, &[]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["ready"], false, "{config}: {report}");
        assert!(
            report["next_steps"].to_string().contains(expected),
            "{report}"
        );
        assert!(!dir.path().join("data/daemon.sock").exists());
    }
}

#[test]
fn absolute_binary_invocation_requires_horde_on_agent_path() {
    let dir = fixture();
    fs::remove_file(dir.path().join("bin/horde")).unwrap();
    fs::write(
        dir.path().join("repo/.horde.toml"),
        "[providers.default]\nkind = 'codex'\nauth_mode = 'login'\nprogram = 'mock-provider'\n",
    )
    .unwrap();
    let _daemon = RunningDaemon(&dir);
    let output = init(&dir, &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ready"], false);
    assert!(report["next_steps"].to_string().contains("horde on PATH"));
}

#[test]
fn environment_data_directory_is_pinned_in_agent_config() {
    let dir = fixture();
    let data = dir.path().join("environment-data");
    // Exercise the environment-only path, without the helper's --data-dir flag.
    let output = Command::new(env!("CARGO_BIN_EXE_horde"))
        .args(["init", "--agent", "codex", "--delegate", "always"])
        .current_dir(dir.path().join("repo"))
        .env_clear()
        .envs(std::env::var_os("LLVM_PROFILE_FILE").map(|value| ("LLVM_PROFILE_FILE", value)))
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", dir.path().join("bin").display()),
        )
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("HORDE_DATA_DIR", &data)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config: toml::Value =
        toml::from_str(&fs::read_to_string(dir.path().join("repo/.codex/config.toml")).unwrap())
            .unwrap();
    assert_eq!(
        config["mcp_servers"]["horde"]["args"].as_array().unwrap(),
        &vec![
            toml::Value::String("--data-dir".into()),
            toml::Value::String(data.to_str().unwrap().into()),
            toml::Value::String("mcp".into())
        ]
    );
}

#[test]
fn invalid_runtime_skill_pack_prevents_readiness_and_daemon_start() {
    let dir = fixture();
    fs::write(
        dir.path().join("repo/.horde.toml"),
        "[providers.default]\nkind = 'codex'\nauth_mode = 'login'\nprogram = 'mock-provider'\n",
    )
    .unwrap();
    fs::create_dir_all(dir.path().join("data/skill-packs")).unwrap();
    fs::write(
        dir.path().join("data/skill-packs/CURRENT"),
        "invalid digest",
    )
    .unwrap();
    let _daemon = RunningDaemon(&dir);
    let output = init(&dir, &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ready"], false, "{report}");
    assert!(
        report["next_steps"].to_string().contains("skill pack"),
        "{report}"
    );
    assert!(!dir.path().join("data/daemon.sock").exists());
}
