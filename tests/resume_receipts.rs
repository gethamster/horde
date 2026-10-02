use horde::{config::Settings, protocol, store::Store, template};
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn lost_resume_response_replays_durably_without_resetting_a_later_failure() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let db = Store::open(&root).unwrap();
    let pack = horde::skill_catalog::load_from(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("skills"),
    )
    .unwrap();
    horde::skill_catalog::install(&root, &pack).unwrap();
    let plan = template::compile(
        "simulated",
        &template::load_templates(dir.path()).unwrap(),
        BTreeMap::from([("task".into(), "receipt smoke".into())]),
    )
    .unwrap();
    let task = db
        .submit("receipt smoke", dir.path(), &Settings::default(), &plan)
        .unwrap();
    db.conn
        .execute("UPDATE steps SET state='failed' WHERE task=?", [&task])
        .unwrap();
    let input = json!({"task":task,"request_id":"foundry:stable-operation"});
    let first =
        protocol::dispatch_scoped(&db, "resume", input.clone(), None, Some("default")).unwrap();
    assert_eq!(first, json!({"resumed":true}));
    assert!(
        db.steps(&task)
            .unwrap()
            .iter()
            .all(|s| s["state"] == "pending")
    );
    // A new attempt fails after the caller loses the first HTTP reply.
    db.conn
        .execute("UPDATE steps SET state='failed' WHERE task=?", [&task])
        .unwrap();
    db.conn
        .execute("UPDATE tasks SET status='failed' WHERE id=?", [&task])
        .unwrap();
    drop(db);
    let db = Store::open(&root).unwrap();
    assert_eq!(
        protocol::dispatch_scoped(&db, "resume", input.clone(), None, Some("default")).unwrap(),
        first
    );
    assert!(
        db.steps(&task)
            .unwrap()
            .iter()
            .all(|s| s["state"] == "failed")
    );
    assert_eq!(db.task(&task).unwrap()["status"], "failed");
    let count: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE task=? AND kind='task.resumed'",
            [&task],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    let mut changed = input.clone();
    changed["extra"] = json!(true);
    assert!(protocol::dispatch_scoped(&db, "resume", changed, None, Some("default")).is_err());
    let other = horde::projects::dispatch(&db, "project_create", &json!({"slug":"other"}))
        .unwrap()
        .unwrap();
    assert!(protocol::dispatch_scoped(&db, "resume", input, None, other["id"].as_str()).is_err());
    let schema = protocol::admin_schema("resume");
    assert_eq!(schema["properties"]["request_id"]["type"], "string");
    assert!(
        !schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("request_id"))
    );
    // A genuinely new operation can retry this new failure normally.
    protocol::dispatch_scoped(
        &db,
        "resume",
        json!({"task":task,"request_id":"new-operation"}),
        None,
        Some("default"),
    )
    .unwrap();
    assert!(
        db.steps(&task)
            .unwrap()
            .iter()
            .all(|s| s["state"] == "pending")
    );
    // The old API remains available without caller receipt identity.
    db.conn
        .execute("UPDATE steps SET state='failed' WHERE task=?", [&task])
        .unwrap();
    assert_eq!(
        protocol::dispatch_scoped(&db, "resume", json!({"task":task}), None, Some("default"))
            .unwrap(),
        first
    );
}
