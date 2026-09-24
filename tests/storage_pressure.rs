use horde::{protocol, store::Store};
use serde_json::json;

#[test]
fn pressure_warns_before_critical_and_requires_recovery_headroom() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open_with_config_dir(dir.path(), &dir.path().join("config")).unwrap();
    protocol::dispatch(
        &db,
        "runtime_storage_configure",
        json!({
            "min_free_bytes":1048576,"warning_free_bytes":1024_u64.pow(5),
            "resume_free_bytes":1024_u64.pow(5)+1048576
        }),
        None,
    )
    .unwrap();
    let status = horde::storage::status(&db).unwrap();
    assert_eq!(status["state"], "cleanup_requested");
    assert_eq!(status["pressure"], true);
    assert_eq!(status["paused"], false);
    assert_eq!(
        horde::capabilities::local(&db).unwrap()["capacity"]["available"],
        0
    );
    for args in [
        json!({"warning_free_bytes":0}),
        json!({"resume_free_bytes":1048576}),
        json!({"cleanup_command":["relative-command"]}),
        json!({"cleanup_timeout_seconds":0}),
    ] {
        assert!(protocol::dispatch(&db, "runtime_storage_configure", args, None).is_err());
    }
}
