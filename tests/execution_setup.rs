use horde::{execution_setup, projects, store::Store};
use serde_json::json;

#[test]
fn invalid_execution_configuration_has_no_filesystem_effects() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let project = projects::dispatch(&db, "project_create", &json!({"slug":"example"}))
        .unwrap()
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    for payload in [
        json!({"scope":"remote","projects":[]}),
        json!({"scope":"local","projects":[{"id":project,"slug":"../escape","git_proxy_token":"a".repeat(40)}]}),
        json!({"scope":"local","projects":[{"id":project,"slug":"example","git_proxy_token":"short"}]}),
        json!({"scope":"local","projects":[{"id":project,"slug":"other","git_proxy_token":"a".repeat(40)}]}),
    ] {
        assert!(execution_setup::configure(&db, &payload).is_err());
    }
    assert!(!dir.path().join("private/execution.json").exists());
}

#[test]
fn absent_execution_profile_is_a_noop() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    execution_setup::restore(&db).unwrap();
    assert!(!dir.path().join("private/execution.json").exists());
}

#[test]
fn credential_helper_ignores_non_get_before_reading_secrets() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        execution_setup::credential_helper(dir.path(), "invalid", "store", "").unwrap(),
        ""
    );
    assert_eq!(
        execution_setup::credential_helper(dir.path(), "invalid", "erase", "").unwrap(),
        ""
    );
}

#[test]
fn native_cli_implements_git_protocol_without_daemon() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let dir = tempfile::tempdir().unwrap();
    let project = uuid::Uuid::new_v4().to_string();
    let private = dir.path().join("private");
    let git = private.join("git").join(&project);
    std::fs::create_dir_all(&git).unwrap();
    std::fs::write(
        private.join("execution.json"),
        json!({"schema_version":1,"projects":[{"id":project,"slug":"example"}]}).to_string(),
    )
    .unwrap();
    std::fs::write(git.join("token"), "test-secret-value-at-least-32-bytes").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_horde"))
        .arg("--data-dir")
        .arg(dir.path())
        .args(["git-credential", "--project-id", &project, "get"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"protocol=http\nhost=deliver-bridge:8090\npath=git/example\n\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "username=horde\npassword=test-secret-value-at-least-32-bytes\n\n"
    );
    assert!(!dir.path().join("state.sqlite3").exists());
}
