use serde_json::json;
use std::process::Command;

#[test]
fn compiled_release_compatibility_never_opens_or_creates_runtime_state() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("absent");
    let read = || {
        let output = Command::new(env!("CARGO_BIN_EXE_horde"))
            .arg("--data-dir")
            .arg(&root)
            .arg("release-compatibility")
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            value,
            json!({"version": env!("CARGO_PKG_VERSION"), "protocol": 1,
            "schema_min": 2, "schema_max": horde::store::SCHEMA_VERSION})
        );
    };
    read();
    assert!(!root.exists());
    std::fs::create_dir(&root).unwrap();
    let path = root.join("state.sqlite3");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.pragma_update(None, "user_version", horde::store::SCHEMA_VERSION + 1)
        .unwrap();
    db.close().unwrap();
    let sentinel = std::fs::read(&path).unwrap();
    read();
    assert_eq!(std::fs::read(path).unwrap(), sentinel);
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 1);
}
