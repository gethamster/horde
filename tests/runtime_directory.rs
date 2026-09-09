use horde::{
    fleet, management, runtime_directory as directory,
    store::{Store, now},
};
use rusqlite::params;
use serde_json::json;

fn runtime(db: &Store, id: &str, state: &str) {
    db.conn
        .execute(
            "INSERT INTO managed_runtimes(id,profile,spec,state,created) VALUES(?,'test','{}',?,?)",
            params![id, state, now()],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO runtime_enrollments VALUES(?,?, '', ?, 'active')",
            params![id, format!("fingerprint-{id}"), now() + 300],
        )
        .unwrap();
}

#[test]
fn names_resolve_offline_and_controller_renames_override_worker_reports() {
    let root = tempfile::tempdir().unwrap();
    let db = Store::open(root.path()).unwrap();
    runtime(&db, "ts-one", "ready");
    directory::observe_name(&db, "ts-one", "apollo").unwrap();
    assert_eq!(directory::resolve(&db, "apollo").unwrap(), "ts-one");
    directory::rename(&db, "ts-one", "build-mac").unwrap();
    directory::observe_name(&db, "ts-one", "renamed-remotely").unwrap();
    assert_eq!(directory::resolve(&db, "build-mac").unwrap(), "ts-one");
    assert_eq!(
        directory::display_name(&db, "ts-one").unwrap().as_deref(),
        Some("build-mac")
    );
    assert!(directory::resolve(&db, "apollo").is_err());
    let listed = fleet::dispatch(&db, "runtime_list", &json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(listed[0]["name"], "build-mac");
    runtime(&db, "ts-other", "ready");
    directory::observe_name(&db, "ts-other", "build-mac").unwrap();
    assert_eq!(directory::resolve(&db, "build-mac").unwrap(), "ts-one");
}

#[test]
fn duplicate_advertised_names_require_exact_id_and_names_cannot_spoof_ids() {
    let root = tempfile::tempdir().unwrap();
    let db = Store::open(root.path()).unwrap();
    runtime(&db, "ts-one", "ready");
    runtime(&db, "ts-two", "ready");
    directory::observe_name(&db, "ts-one", "apollo").unwrap();
    directory::observe_name(&db, "ts-two", "apollo").unwrap();
    assert!(
        directory::resolve(&db, "apollo")
            .unwrap_err()
            .to_string()
            .contains("ambiguous")
    );
    assert_eq!(directory::resolve(&db, "ts-one").unwrap(), "ts-one");
    for name in [
        "ts-two",
        "fleet-pretend",
        "local",
        "Has Capitals",
        "a.b",
        "-prefix",
        "",
    ] {
        assert!(
            directory::rename(&db, "ts-one", name).is_err(),
            "accepted {name}"
        );
    }
    directory::rename(&db, "ts-one", "first").unwrap();
    assert!(directory::rename(&db, "ts-two", "first").is_err());
}

#[test]
fn stale_removal_revokes_locally_preserves_history_and_hides_entry() {
    let root = tempfile::tempdir().unwrap();
    let db = Store::open(root.path()).unwrap();
    runtime(&db, "ts-stale", "provisioned");
    directory::rename(&db, "ts-stale", "old-mac").unwrap();
    let result = directory::forget(&db, "ts-stale").unwrap();
    assert_eq!(result["removed"], true);
    assert_eq!(
        db.rows("SELECT state FROM runtime_enrollments", &[])
            .unwrap()[0]["state"],
        "revoked"
    );
    assert_eq!(
        db.rows("SELECT state FROM managed_runtimes", &[]).unwrap()[0]["state"],
        "removed"
    );
    assert!(directory::resolve(&db, "old-mac").is_err());
    assert!(
        fleet::dispatch(&db, "runtime_list", &json!({}))
            .unwrap()
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.rows(
            "SELECT kind FROM management_events WHERE kind='runtime.removed'",
            &[]
        )
        .unwrap()
        .len(),
        1
    );
}

#[test]
fn removal_refuses_recent_presence_and_unfinished_operations() {
    let root = tempfile::tempdir().unwrap();
    let db = Store::open(root.path()).unwrap();
    runtime(&db, "ts-one", "ready");
    db.conn
        .execute(
            "INSERT INTO runtime_presence VALUES(?,?, '{}')",
            params!["ts-one", now()],
        )
        .unwrap();
    assert!(directory::forget(&db, "ts-one").is_err());
    db.conn.execute("DELETE FROM runtime_presence", []).unwrap();
    db.conn.execute("INSERT INTO runtime_operations(id,runtime,action,args,state,created) VALUES('op','ts-one','runtime_create','{}','uncertain',?)",[now()]).unwrap();
    assert!(directory::forget(&db, "ts-one").is_err());
    assert_eq!(
        db.rows("SELECT state FROM runtime_enrollments", &[])
            .unwrap()[0]["state"],
        "active"
    );
}

#[test]
fn revoked_identity_never_resolves_for_submission_and_local_name_is_durable() {
    let root = tempfile::tempdir().unwrap();
    let db = Store::open(root.path()).unwrap();
    runtime(&db, "ts-one", "ready");
    directory::rename(&db, "ts-one", "apollo").unwrap();
    db.conn
        .execute("UPDATE runtime_enrollments SET state='revoked'", [])
        .unwrap();
    assert!(directory::resolve(&db, "ts-one").is_err());
    assert!(directory::resolve(&db, "apollo").is_err());
    assert_eq!(directory::resolve_known(&db, "apollo").unwrap(), "ts-one");
    directory::set_local_name(&db, "desktop").unwrap();
    assert_eq!(directory::local_name(&db).unwrap(), "desktop");
    assert_eq!(
        management::value(&db, "runtime_name").unwrap().as_deref(),
        Some("desktop")
    );
}

#[test]
fn removal_refuses_unfinished_remote_work_and_releases_names() {
    let root = tempfile::tempdir().unwrap();
    let db = Store::open(root.path()).unwrap();
    runtime(&db, "ts-one", "provisioned");
    directory::rename(&db, "ts-one", "apollo").unwrap();
    db.conn.execute("INSERT INTO remote_links(task,peer,state,request) VALUES('child','ts-one','running','{}')",[]).unwrap();
    assert!(directory::forget(&db, "ts-one").is_err());
    db.conn
        .execute("UPDATE remote_links SET state='done'", [])
        .unwrap();
    directory::forget(&db, "ts-one").unwrap();
    runtime(&db, "ts-next", "ready");
    directory::rename(&db, "ts-next", "apollo").unwrap();
    assert_eq!(directory::resolve(&db, "apollo").unwrap(), "ts-next");
}

#[test]
fn forgotten_independent_fleet_members_are_hidden_without_deleting_cert_history() {
    let root = tempfile::tempdir().unwrap();
    let db = Store::open(root.path()).unwrap();
    db.conn
        .execute(
            "INSERT INTO fleet_enrollment_keys VALUES('key','test','hash',?,10,1,'active',?)",
            params![now() + 60, now()],
        )
        .unwrap();
    db.conn.execute("INSERT INTO fleet_enrollment_members(runtime,key_id,public_key_hash,state,created) VALUES('fleet-one','key','public','active',?)",[now()]).unwrap();
    directory::rename(&db, "fleet-one", "sandbox").unwrap();
    directory::forget(&db, "fleet-one").unwrap();
    assert!(
        fleet::dispatch(&db, "runtime_list", &json!({}))
            .unwrap()
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.rows("SELECT state FROM fleet_enrollment_members", &[])
            .unwrap()[0]["state"],
        "revoked"
    );
}
