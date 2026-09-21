//! Operator acceptance through the installed CLI and a real local daemon.
use serde_json::Value;
use std::{
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};
mod support;

struct Daemon {
    child: Child,
    root: PathBuf,
    directory: tempfile::TempDir,
}
impl Drop for Daemon {
    fn drop(&mut self) {
        support::stop_daemon(&mut self.child, &self.root);
    }
}
impl Daemon {
    fn new() -> Self {
        let directory = tempfile::tempdir_in("/tmp").unwrap();
        let root = directory.path().join("data");
        let child = Command::new(env!("CARGO_BIN_EXE_horde"))
            .arg("--data-dir")
            .arg(&root)
            .arg("daemon")
            .env("XDG_CONFIG_HOME", directory.path().join("config"))
            .env_remove("HORDE_WORKER_TOKEN")
            .env_remove("CODEHORDE_WORKER_TOKEN")
            .env_remove("HORDE_BOOTSTRAP_JSON")
            .env_remove("HORDE_ENROLLMENT_JSON")
            .env_remove("HORDE_ENROLLMENT_FILE")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let daemon = Self {
            child,
            root,
            directory,
        };
        let start = Instant::now();
        while !horde::daemon_client::running(&daemon.root) {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "daemon startup timeout"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        daemon
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_horde"))
            .arg("--data-dir")
            .arg(&self.root)
            .args(args)
            .env("XDG_CONFIG_HOME", self.directory.path().join("config"))
            .env_remove("HORDE_WORKER_TOKEN")
            .env_remove("CODEHORDE_WORKER_TOKEN")
            .output()
            .unwrap()
    }
    fn call(&self, args: &[&str]) -> Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "administrative CLI operation failed"
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(!text.contains("fixture-private-key"));
        serde_json::from_str(&text).unwrap()
    }
}

#[test]
fn operator_manages_two_projects_repositories_and_shared_account_without_leaking_credentials() {
    let daemon = Daemon::new();
    for slug in ["horde", "hamster"] {
        let project = daemon.call(&["project", "create", slug, "--slug", slug]);
        assert_eq!(project["slug"], slug);
        for repo in ["app", "docs"] {
            let path = daemon.directory.path().join(format!("{slug}-{repo}"));
            std::fs::create_dir(&path).unwrap();
            let registered = daemon.call(&["project", "repo-add", slug, path.to_str().unwrap()]);
            assert_eq!(registered["project"], project["id"]);
        }
        daemon.call(&["project", "runtime-grant", slug, "local"]);
        let inspected = daemon.call(&["project", "inspect", slug]);
        assert_eq!(inspected["repositories"].as_array().unwrap().len(), 2);
        assert_eq!(inspected["runtime_grants"].as_array().unwrap().len(), 1);
    }
    assert_eq!(
        daemon.call(&["project", "list"]).as_array().unwrap().len(),
        3
    );
    let updated = daemon.call(&[
        "project",
        "update",
        "horde",
        "--concurrency",
        "2",
        "--isolation",
        "vm",
    ]);
    assert_eq!(updated["concurrency"], 2);
    assert_eq!(updated["isolation"], "vm");
    let config = daemon.directory.path().join("approved.toml");
    std::fs::write(&config, "concurrency = 2\n").unwrap();
    assert_eq!(
        daemon.call(&[
            "project",
            "configure",
            "horde",
            "--file",
            config.to_str().unwrap()
        ])["configured"],
        true
    );
    daemon.call(&[
        "project",
        "runtime-grant",
        "horde",
        "dedicated-host",
        "--dedicated",
    ]);
    let denied = daemon.run(&["project", "runtime-grant", "hamster", "dedicated-host"]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("exclusively"));
    daemon.call(&["project", "runtime-revoke", "hamster", "local"]);
    assert!(
        daemon.call(&["project", "inspect", "hamster"])["runtime_grants"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    daemon.call(&["project", "runtime-grant", "hamster", "local"]);

    let account = daemon.call(&[
        "--project",
        "horde",
        "account",
        "create",
        "shared",
        "--provider",
        "codex",
        "--auth-mode",
        "api",
        "--base-url",
        "https://api.example.invalid/v1",
        "--concurrency",
        "2",
    ]);
    let id = account["id"].as_str().unwrap();
    assert!(
        daemon.call(&["--project", "hamster", "account", "list"])["accounts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let credentials = daemon.directory.path().join("credential.json");
    std::fs::write(
        &credentials,
        r#"{"kind":"api_key","secret":"fixture-private-key"}"#,
    )
    .unwrap();
    assert_eq!(
        daemon.call(&[
            "--project",
            "horde",
            "account",
            "credential-set",
            id,
            credentials.to_str().unwrap()
        ])["credential_version"],
        1
    );
    daemon.call(&["account", "grant", id, "hamster"]);
    for project in ["horde", "hamster"] {
        let account = daemon.call(&["--project", project, "account", "inspect", id]);
        assert_eq!(account["credential_version"], 1);
        assert_eq!(account["active"], 0);
        assert_eq!(
            daemon.call(&["--project", project, "account", "list"])["accounts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(
            daemon.call(&["--project", project, "account", "delivery-list", id])["deliveries"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(
        daemon.call(&["account", "revoke", id, "hamster"])["revoked"],
        true
    );
    assert!(
        !daemon
            .run(&["--project", "hamster", "account", "inspect", id])
            .status
            .success()
    );
    assert_eq!(
        daemon.call(&["--project", "horde", "account", "inspect", id])["credential_version"],
        1
    );
    let db = horde::store::Store::open(&daemon.root).unwrap();
    assert_eq!(db.rows("SELECT * FROM accounts", &[]).unwrap().len(), 1);
    assert_eq!(
        db.rows("SELECT * FROM account_grants", &[]).unwrap().len(),
        1
    );
    assert_eq!(
        db.rows("SELECT * FROM project_repositories", &[])
            .unwrap()
            .len(),
        4
    );
    assert!(
        !serde_json::to_string(&db.rows("SELECT * FROM auth_profiles", &[]).unwrap())
            .unwrap()
            .contains("fixture-private-key")
    );
}
