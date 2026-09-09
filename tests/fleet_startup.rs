use std::process::Command;

#[test]
fn conflicting_bootstrap_credentials_are_rejected_before_writing_state() {
    for fleet_variable in ["HORDE_ENROLLMENT_FILE", "HORDE_ENROLLMENT_JSON"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("worker");
        let output = Command::new(env!("CARGO_BIN_EXE_horde"))
            .args(["--data-dir", root.to_str().unwrap(), "daemon"])
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .env("HORDE_BOOTSTRAP_JSON", "{\"credential\":\"DO-NOT-PRINT\"}")
            .env(fleet_variable, "DO-NOT-PRINT")
            .output()
            .unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("cannot be combined"), "{error}");
        assert!(!error.contains("DO-NOT-PRINT"));
        assert!(!root.join("state.sqlite3").exists());
        assert!(!root.join("managed-network.toml").exists());
    }
}

#[test]
fn legacy_bootstrap_cannot_replace_a_pending_fleet_worker() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("fleet-worker-pending.json"), "pending").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_horde"))
        .args(["--data-dir", temp.path().to_str().unwrap(), "daemon"])
        .env("HORDE_BOOTSTRAP_JSON", "{}")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be combined"));
    assert!(!temp.path().join("state.sqlite3").exists());
    assert_eq!(
        std::fs::read_to_string(temp.path().join("fleet-worker-pending.json")).unwrap(),
        "pending"
    );
}
