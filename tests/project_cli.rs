use std::process::Command;

#[test]
fn installed_binary_can_print_its_exact_bundled_lima_guard() {
    let root = tempfile::tempdir().unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_horde"))
        .args([
            "--data-dir",
            root.path().join("unused").to_str().unwrap(),
            "runtime",
            "guard-script",
        ])
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        horde::lima::guard_script()
    );
    assert!(!root.path().join("unused/state.sqlite3").exists());
}

#[test]
fn project_and_account_commands_publish_selection_and_administration() {
    let config = tempfile::tempdir().unwrap();
    for args in [
        vec!["--help"],
        vec!["project", "--help"],
        vec!["project", "configure", "--help"],
        vec!["account", "--help"],
        vec!["--project", "hamster", "project", "inspect", "--help"],
        vec!["list", "--help"],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_horde"))
            .args(&args)
            .env("XDG_CONFIG_HOME", config.path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let text = String::from_utf8(result.stdout).unwrap();
        assert!(text.contains("--project"));
    }
}

#[test]
fn all_projects_conflicts_with_selected_project_before_connecting() {
    let config = tempfile::tempdir().unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_horde"))
        .args(["--project", "hamster", "list", "--all-projects"])
        .env("XDG_CONFIG_HOME", config.path())
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("cannot be used with"));
}

#[test]
fn mcp_bridge_keeps_project_binding_outside_caller_arguments() {
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixListener;
    use std::process::Stdio;
    let dir = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(dir.path().join("daemon.sock")).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        std::io::BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["project"], "hamster");
        assert_eq!(request["args"]["project"], "horde");
        writeln!(
            stream,
            "{}",
            serde_json::json!({"error":"project scope violation"})
        )
        .unwrap();
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_horde"))
        .args([
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--project",
            "hamster",
            "mcp",
        ])
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env_remove("HORDE_WORKER_TOKEN")
        .env_remove("CODEHORDE_WORKER_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(),"{}",serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_tasks","arguments":{"project":"horde"}}})).unwrap();
    let result = child.wait_with_output().unwrap();
    server.join().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let response: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(response["result"]["isError"], true);
}

#[test]
fn selected_project_cannot_silently_modify_host_configuration() {
    let config = tempfile::tempdir().unwrap();
    for command in ["config", "network", "skills"] {
        let mut args = vec!["--project", "hamster", command];
        args.push(match command {
            "config" => "init",
            "network" => "config",
            _ => "list",
        });
        let result = Command::new(env!("CARGO_BIN_EXE_horde"))
            .args(args)
            .env("XDG_CONFIG_HOME", config.path())
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("project configure"));
    }
}

#[test]
fn project_update_sends_only_explicit_changes() {
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixListener;
    let dir = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(dir.path().join("daemon.sock")).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        std::io::BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        writeln!(stream, "{}", serde_json::json!({"result":{}})).unwrap();
        request
    });
    let result = Command::new(env!("CARGO_BIN_EXE_horde"))
        .args([
            "--data-dir",
            dir.path().to_str().unwrap(),
            "project",
            "update",
            "hamster",
            "--concurrency",
            "2",
        ])
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let request = server.join().unwrap();
    assert_eq!(request["method"], "project_update");
    assert_eq!(
        request["args"],
        serde_json::json!({"project":"hamster","concurrency":2})
    );
}
