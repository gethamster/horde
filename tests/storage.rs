use horde::{protocol, store::Store};
use serde_json::json;

#[test]
fn storage_policy_is_admin_only_persistent_and_validated() {
    let temp = tempfile::tempdir().unwrap();
    let db = Store::open_with_config_dir(temp.path(), &temp.path().join("config")).unwrap();
    let initial = protocol::dispatch(&db, "runtime_storage_status", json!({}), None).unwrap();
    assert_eq!(initial["policy"]["min_free_bytes"], 2 * 1024_u64.pow(3));
    assert_eq!(initial["pressure"], false);
    assert!(initial["volumes"][0]["available_bytes"].as_u64().unwrap() > 0);
    protocol::dispatch(
        &db,
        "runtime_storage_configure",
        json!({"min_free_bytes": 1048576, "retention_seconds":86400,"automatic_cleanup":false}),
        None,
    )
    .unwrap();
    for args in [
        json!({"min_free_bytes":0}),
        json!({"retention_seconds":0}),
        json!({"automatic_cleanup":"yes"}),
        json!({"unknown":true}),
    ] {
        assert!(protocol::dispatch(&db, "runtime_storage_configure", args, None).is_err());
    }
    for name in [
        "runtime_storage_status",
        "runtime_storage_configure",
        "runtime_storage_cleanup",
    ] {
        assert!(!protocol::worker_allowed(name));
        assert!(!protocol::project_allowed(name));
        assert!(protocol::dispatch_scoped(&db, name, json!({}), None, Some("default")).is_err());
    }
    drop(db);
    let db = Store::open_with_config_dir(temp.path(), &temp.path().join("config")).unwrap();
    let status = protocol::dispatch(&db, "runtime_storage_status", json!({}), None).unwrap();
    assert_eq!(status["policy"]["min_free_bytes"], 1048576);
    assert_eq!(status["policy"]["automatic_cleanup"], false);
}

#[test]
fn cleanup_defaults_to_preview_and_rejects_invalid_limits() {
    let temp = tempfile::tempdir().unwrap();
    let db = Store::open_with_config_dir(temp.path(), &temp.path().join("config")).unwrap();
    let report = protocol::dispatch(&db, "runtime_storage_cleanup", json!({}), None).unwrap();
    assert_eq!(report["dry_run"], true);
    for args in [
        json!({"limit":0}),
        json!({"limit":1001}),
        json!({"dry_run":"false"}),
    ] {
        assert!(protocol::dispatch(&db, "runtime_storage_cleanup", args, None).is_err());
    }
}
