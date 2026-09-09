use horde::{
    fleet, management, runtime_directory,
    store::{Store, now},
};
use rusqlite::params;
use serde_json::json;

fn member(db: &Store, id: &str) {
    db.conn.execute("INSERT OR IGNORE INTO fleet_enrollment_keys VALUES('key','workers','hash',9999999999,10,4,'active',0)", []).unwrap();
    db.conn
        .execute(
            "INSERT INTO fleet_enrollment_members VALUES(?,'key',?,'active',0,?)",
            params![id, format!("pk-{id}"), format!("fp-{id}")],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO fleet_enrollment_certificates VALUES(?,?,'fixture',?, ?,4)",
            params![format!("fp-{id}"), id, now() + 3600, now() + 1800],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO runtime_enrollments VALUES(?,?,'',?,'active')",
            params![id, format!("fp-{id}"), now() + 3600],
        )
        .unwrap();
}

#[test]
fn fleet_member_updates_by_name_pin_identity_and_retry_before_resolving() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    member(&db, "fleet-apollo");
    runtime_directory::rename(&db, "fleet-apollo", "apollo").unwrap();
    let args = json!({"id":"apollo","request_id":"update-apollo","version":"0.6.1"});
    let accepted = fleet::dispatch(&db, "runtime_update", &args)
        .unwrap()
        .unwrap();
    assert_eq!(accepted["state"], "pending");
    runtime_directory::rename(&db, "fleet-apollo", "build-mac").unwrap();
    member(&db, "fleet-other");
    runtime_directory::rename(&db, "fleet-other", "apollo").unwrap();
    let replay = fleet::dispatch(&db, "runtime_update", &args)
        .unwrap()
        .unwrap();
    assert_eq!(replay["runtime"], "fleet-apollo");
    assert!(
        fleet::dispatch(
            &db,
            "runtime_update",
            &json!({"id":"apollo","request_id":"update-apollo","version":"0.6.2"})
        )
        .is_err()
    );
    assert_eq!(
        db.rows("SELECT * FROM managed_runtimes", &[])
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn fleet_updates_reject_revoked_expired_and_provider_operations() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    member(&db, "fleet-apollo");
    for action in [
        "runtime_destroy",
        "runtime_stop",
        "runtime_start",
        "runtime_reconcile",
    ] {
        assert!(
            fleet::dispatch(
                &db,
                action,
                &json!({"id":"fleet-apollo","request_id":action})
            )
            .is_err()
        );
    }
    db.conn
        .execute("UPDATE fleet_enrollment_certificates SET expires=0", [])
        .unwrap();
    assert!(
        fleet::dispatch(
            &db,
            "runtime_update",
            &json!({"id":"fleet-apollo","request_id":"expired","version":"0.6.1"})
        )
        .is_err()
    );
    db.conn
        .execute(
            "UPDATE fleet_enrollment_certificates SET expires=?",
            [now() + 3600],
        )
        .unwrap();
    db.conn
        .execute("UPDATE fleet_enrollment_members SET state='revoked'", [])
        .unwrap();
    assert!(
        fleet::dispatch(
            &db,
            "runtime_restart",
            &json!({"id":"fleet-apollo","request_id":"revoked"})
        )
        .is_err()
    );
    assert!(
        db.rows("SELECT * FROM runtime_operations", &[])
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn revocation_after_acceptance_blocks_queued_update_without_provider_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    member(&db, "fleet-apollo");
    fleet::dispatch(
        &db,
        "runtime_restart",
        &json!({"id":"fleet-apollo","request_id":"restart"}),
    )
    .unwrap();
    db.conn
        .execute("UPDATE fleet_enrollment_members SET state='revoked'", [])
        .unwrap();
    fleet::tick(&db).await.unwrap();
    let op = db
        .rows("SELECT * FROM runtime_operations", &[])
        .unwrap()
        .remove(0);
    assert_ne!(op["state"], "succeeded");
    assert!(op["result"].as_str().unwrap().contains("active"));
    assert!(!management::draining(&db).unwrap());
}

fn packet(text: &str) -> horde::skills::Packet {
    let files = std::collections::BTreeMap::from([(
        "SKILL.md".into(),
        horde::skills::File {
            hex: hex::encode(format!(
                "---\nname: fixture\ndescription: fixture skill\n---\n{text}\n"
            )),
            executable: false,
        },
    )]);
    std::collections::BTreeMap::from([(
        "fixture".into(),
        horde::skills::Bundle {
            hash: horde::store::hash(&serde_json::to_vec(&files).unwrap()),
            files,
        },
    )])
}

#[test]
fn skill_update_installs_without_drain_and_old_receipts_cannot_reactivate_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let first = json!({"request_id":"skills-one","action":"runtime_skills_update","packet":packet("First instructions")});
    let second = json!({"request_id":"skills-two","action":"runtime_skills_update","packet":packet("Second instructions")});
    management::remote_command(&db, "parent", &first).unwrap();
    management::local_commands(&db).unwrap();
    let receipt = management::remote_command(&db, "parent", &first).unwrap();
    assert_eq!(receipt["state"], "succeeded");
    assert_eq!(
        receipt["catalog"]["hash"],
        horde::skill_catalog::summary(&packet("First instructions")).unwrap()["hash"]
    );
    assert!(!management::draining(&db).unwrap());
    assert!(!dir.path().join("shutdown.request").exists());
    management::remote_command(&db, "parent", &second).unwrap();
    management::local_commands(&db).unwrap();
    drop(db);
    let db = Store::open(dir.path()).unwrap();
    assert_eq!(
        management::remote_command(&db, "parent", &first).unwrap()["state"],
        "succeeded"
    );
    management::local_commands(&db).unwrap();
    let current = horde::skill_catalog::load_for(dir.path()).unwrap();
    assert_eq!(
        horde::skill_catalog::summary(&current).unwrap(),
        horde::skill_catalog::summary(&packet("Second instructions")).unwrap()
    );
    assert!(management::remote_command(&db,"parent",&json!({"request_id":"skills-one","action":"runtime_skills_update","packet":packet("Changed intent")})).is_err());
}

#[test]
fn controller_captures_skill_packet_once_and_recovers_interrupted_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    member(&db, "fleet-apollo");
    horde::skill_catalog::install(dir.path(), &packet("Captured instructions")).unwrap();
    let args = json!({"id":"fleet-apollo","request_id":"skills-update"});
    fleet::dispatch(&db, "runtime_skills_update", &args).unwrap();
    horde::skill_catalog::install(dir.path(), &packet("Edited instructions")).unwrap();
    let replay = fleet::dispatch(&db, "runtime_skills_update", &args)
        .unwrap()
        .unwrap();
    let public_payload: serde_json::Value =
        serde_json::from_str(replay["args"].as_str().unwrap()).unwrap();
    assert!(public_payload.get("packet").is_none());
    assert_eq!(
        replay["requested_catalog"],
        horde::skill_catalog::summary(&packet("Captured instructions")).unwrap()
    );
    let stored = db.rows("SELECT args FROM runtime_operations", &[]).unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(stored[0]["args"].as_str().unwrap()).unwrap();
    assert_eq!(payload["packet"], json!(packet("Captured instructions")));
    db.conn
        .execute("UPDATE runtime_operations SET state='running'", [])
        .unwrap();
    fleet::recover_operations(&db).unwrap();
    assert_eq!(
        db.rows("SELECT state FROM runtime_operations", &[])
            .unwrap()[0]["state"],
        "waiting"
    );
    assert!(
        fleet::dispatch(
            &db,
            "runtime_skills_update",
            &json!({"id":"fleet-apollo","request_id":"spoofed","packet":packet("Arbitrary")})
        )
        .is_err()
    );
}

#[tokio::test]
async fn old_worker_skill_update_reports_binary_upgrade_without_retry_loop() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    member(&db, "fleet-apollo");
    horde::skill_catalog::install(dir.path(), &packet("Instructions")).unwrap();
    fleet::dispatch(
        &db,
        "runtime_skills_update",
        &json!({"id":"fleet-apollo","request_id":"skills-update"}),
    )
    .unwrap();
    fleet::tick(&db).await.unwrap();
    let op = db
        .rows("SELECT * FROM runtime_operations", &[])
        .unwrap()
        .remove(0);
    assert_eq!(op["state"], "failed");
    assert!(op["result"].as_str().unwrap().contains("binary first"));
}

#[test]
fn rejected_skill_packet_and_failed_install_leave_runtime_operable() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let mut malformed = packet("Instructions");
    malformed.get_mut("fixture").unwrap().hash = "invalid".into();
    assert!(
        management::remote_command(
            &db,
            "parent",
            &json!({"request_id":"malformed","action":"runtime_skills_update","packet":malformed})
        )
        .is_err()
    );
    assert!(
        db.rows("SELECT * FROM runtime_operations", &[])
            .unwrap()
            .is_empty()
    );
    std::fs::write(dir.path().join("skill-packs"), b"not a directory").unwrap();
    let request = json!({"request_id":"cannot-install","action":"runtime_skills_update","packet":packet("Instructions")});
    management::remote_command(&db, "parent", &request).unwrap();
    management::local_commands(&db).unwrap();
    assert_eq!(
        management::remote_command(&db, "parent", &request).unwrap()["state"],
        "failed"
    );
    assert!(!management::draining(&db).unwrap());
    assert!(!dir.path().join("shutdown.request").exists());
    // Even a broken catalog remains visible through runtime status for recovery.
    assert!(management::status(&db).is_ok());
}

#[test]
fn local_install_orders_after_unacknowledged_parent_install_without_running_binary_commands() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    management::remote_command(
        &db,
        "parent",
        &json!({"action":"runtime_restart","request_id":"restart-later"}),
    )
    .unwrap();
    let first = json!({"action":"runtime_skills_update","request_id":"parent-update","packet":packet("Earlier parent instructions")});
    management::remote_command(&db, "parent", &first).unwrap();
    // Reproduce a crash after activation but before its SQLite receipt commits.
    horde::skill_catalog::install(dir.path(), &packet("Earlier parent instructions")).unwrap();
    drop(db);
    let db = Store::open(dir.path()).unwrap();
    let report = management::install_catalog(&db, &packet("Later local instructions")).unwrap();
    assert_eq!(
        report,
        horde::skill_catalog::summary(&packet("Later local instructions")).unwrap()
    );
    assert_eq!(
        management::remote_command(&db, "parent", &first).unwrap()["state"],
        "succeeded"
    );
    let current = horde::skill_catalog::load_for(dir.path()).unwrap();
    assert_eq!(horde::skill_catalog::summary(&current).unwrap(), report);
    assert_eq!(
        db.rows(
            "SELECT state FROM runtime_operations WHERE id='parent:restart-later'",
            &[]
        )
        .unwrap()[0]["state"],
        "local_pending"
    );
    assert!(!management::draining(&db).unwrap());
    assert!(!dir.path().join("shutdown.request").exists());
}

#[test]
fn progress_receipts_omit_instruction_bytes_but_preserve_reviewable_catalog_identity() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    member(&db, "fleet-apollo");
    let secret_text = "Large unique fixture content";
    let captured = packet(secret_text);
    horde::skill_catalog::install(dir.path(), &captured).unwrap();
    let intent = json!({"id":"fleet-apollo","request_id":"inspect-update"});
    fleet::dispatch(&db, "runtime_skills_update", &intent).unwrap();
    let inspect = fleet::dispatch(&db, "runtime_inspect", &json!({"id":"fleet-apollo"}))
        .unwrap()
        .unwrap();
    assert!(!inspect.to_string().contains(&hex::encode(secret_text)));
    assert_eq!(
        inspect["operations"][0]["requested_catalog"],
        horde::skill_catalog::summary(&captured).unwrap()
    );
    let receiver =
        json!({"action":"runtime_skills_update","request_id":"remote","packet":captured});
    management::remote_command(&db, "parent", &receiver).unwrap();
    let receipt = management::remote_command(&db, "parent", &receiver).unwrap();
    assert!(!receipt.to_string().contains(&hex::encode(secret_text)));
    assert_eq!(
        receipt["requested_catalog"],
        inspect["operations"][0]["requested_catalog"]
    );
}

#[tokio::test]
async fn managed_alias_checks_resolved_peer_skill_capabilities() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    member(&db, "fleet-actual-peer");
    let profile = horde::fleet::Profile {
        provider: "tailscale".into(),
        peer: Some("fleet-actual-peer".into()),
        ..Default::default()
    };
    db.conn.execute("INSERT INTO managed_runtimes(id,profile,spec,state,created) VALUES('logical-worker','fixture',?,'ready',0)",[serde_json::to_string(&profile).unwrap()]).unwrap();
    let mut inventory = horde::capabilities::local(&db).unwrap();
    inventory["runtime"] = json!("fleet-actual-peer");
    horde::capabilities::observe(&db, "fleet-actual-peer", &inventory).unwrap();
    db.conn
        .execute(
            "INSERT INTO runtime_presence VALUES('fleet-actual-peer',?, '{}')",
            [now()],
        )
        .unwrap();
    horde::skill_catalog::install(dir.path(), &packet("Instructions")).unwrap();
    fleet::dispatch(
        &db,
        "runtime_skills_update",
        &json!({"id":"logical-worker","request_id":"peer-update"}),
    )
    .unwrap();
    fleet::tick(&db).await.unwrap();
    let operation = db
        .rows(
            "SELECT result FROM runtime_operations WHERE id='peer-update'",
            &[],
        )
        .unwrap();
    assert!(
        !operation[0]["result"]
            .as_str()
            .unwrap()
            .contains("binary first")
    );
}

#[test]
fn default_bootstrap_preserves_explicit_and_pending_catalog_choices() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    horde::skill_catalog::install(dir.path(), &packet("User instructions")).unwrap();
    assert_eq!(
        management::bootstrap_catalog(&db, &packet("Defaults")).unwrap()["skipped"],
        true
    );
    assert_eq!(
        horde::skill_catalog::summary(&horde::skill_catalog::load_for(dir.path()).unwrap())
            .unwrap(),
        horde::skill_catalog::summary(&packet("User instructions")).unwrap()
    );
    std::fs::remove_file(dir.path().join("skill-packs/CURRENT")).unwrap();
    management::remote_command(&db,"parent",&json!({"action":"runtime_skills_update","request_id":"pending","packet":packet("Parent choice")})).unwrap();
    assert_eq!(
        management::bootstrap_catalog(&db, &packet("Defaults")).unwrap()["skipped"],
        true
    );
    assert!(!dir.path().join("skill-packs/CURRENT").exists());
}

#[test]
fn revoked_provider_runtime_can_still_be_selected_for_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    db.conn.execute("INSERT INTO managed_runtimes(id,profile,spec,state,created) VALUES('old-worker','fixture','{}','provisioned',0)",[]).unwrap();
    db.conn
        .execute(
            "INSERT INTO runtime_enrollments VALUES('old-worker','revoked-fixture','',0,'revoked')",
            [],
        )
        .unwrap();
    runtime_directory::rename(&db, "old-worker", "retired").unwrap();
    let cleanup = fleet::dispatch(
        &db,
        "runtime_destroy",
        &json!({"id":"retired","request_id":"cleanup"}),
    )
    .unwrap()
    .unwrap();
    assert_eq!(cleanup["state"], "pending");
    assert!(
        fleet::dispatch(
            &db,
            "runtime_update",
            &json!({"id":"retired","request_id":"update","version":"0.6.1"})
        )
        .is_err()
    );
}
