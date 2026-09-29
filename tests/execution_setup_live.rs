//! Real binaries, private temporary state, loopback HTTP, and no provider calls.
use serde_json::json;
use std::{
    io::Write,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn command(directory: &Path, home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_horde"));
    command
        .arg("--data-dir")
        .arg(directory.join("data"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", directory.join("config"))
        .env("HORDE_EXECUTION_HOME", home)
        .env(
            "HORDE_EXECUTION_WORKSPACE_ROOT",
            directory.join("workspace"),
        )
        .env("HORDE_EXECUTION_RUSTUP_ROOT", directory.join("rustup"))
        .env("HORDE_EXECUTION_TARGET_ROOT", directory.join("target"))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

#[tokio::test]
async fn first_admin_apply_is_visible_to_running_controller_without_restart() {
    let temp = tempfile::Builder::new()
        .prefix("horde-setup-")
        .tempdir_in("/tmp")
        .unwrap();
    let directory = temp.path();
    let root = directory.join("data");
    let controller_home = directory.join("controller-home");
    std::fs::create_dir_all(directory.join("config/horde")).unwrap();
    std::fs::write(
        directory.join("config/horde/config.toml"),
        "concurrency = 1\n",
    )
    .unwrap();
    let mut daemon = Process(
        command(directory, &controller_home)
            .arg("daemon")
            .spawn()
            .unwrap(),
    );
    let original_pid = daemon.0.id();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !horde::daemon_client::running(&root) {
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "daemon exited during bootstrap"
        );
        assert!(Instant::now() < deadline, "daemon startup timeout");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let project = horde::daemon_client::request(&root, "project_create", json!({"slug":"example"}))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!root.join("private/execution.json").exists());
    assert!(controller_home.join(".gitconfig").is_symlink());

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let token_path = directory.join("admin.token");
    let token = "temporary-admin-credential-at-least-32-bytes";
    std::fs::write(&token_path, token).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut admin = Process(
        command(directory, &directory.join("sidecar-home"))
            .args(["setup-serve", "--listen", &address.to_string()])
            .env("HORDE_SETUP_ADMIN_TOKEN_FILE", &token_path)
            .spawn()
            .unwrap(),
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();
    let base = format!("http://{address}/v1/setup");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if client
            .get(format!("{base}/capabilities"))
            .bearer_auth(token)
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            break;
        }
        assert!(admin.0.try_wait().unwrap().is_none(), "setup server exited");
        assert!(Instant::now() < deadline, "setup server startup timeout");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let receipt: serde_json::Value = client.put(format!("{base}/operations/first-profile"))
        .bearer_auth(token)
        .json(&json!({"kind":"execution-profile","config":{"scope":"local","projects":[{"id":project,"slug":"example","git_proxy_token":"test-project-credential-at-least-32-bytes"}]}}))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(receipt["state"], "succeeded");
    assert!(daemon.0.try_wait().unwrap().is_none());
    assert_eq!(daemon.0.id(), original_pid);

    let mut git = Command::new("git")
        .args(["credential", "fill"])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &controller_home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    git.stdin
        .take()
        .unwrap()
        .write_all(b"protocol=http\nhost=deliver-bridge:8090\npath=git/example\n\n")
        .unwrap();
    let output = git.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let credential = String::from_utf8(output.stdout).unwrap();
    assert!(credential.contains("password=test-project-credential-at-least-32-bytes"));
    assert!(horde::daemon_client::running(&root));
}
