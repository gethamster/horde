use horde::{config::Settings, protocol, store::Store};
use rusqlite::params;
use serde_json::json;

#[test]
fn inspect_reports_only_task_operator_feedback_ack_without_private_contents() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    for task in ["first", "other"] {
        db.conn.execute("INSERT INTO tasks(id,objective,repo,status,settings,plan,created) VALUES(?,?,?,'succeeded',?,'{}',1)",
            params![task,"feedback smoke",dir.path().to_str(),serde_json::to_string(&Settings::default()).unwrap()]).unwrap();
        db.conn
            .execute(
                "INSERT INTO task_projects(task,project) VALUES(?,'default')",
                [task],
            )
            .unwrap();
    }
    let worker = db.register("first", None).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let peer = db.register("first", None).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let other = db.register("other", None).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    db.steer(
        "first",
        "stable-feedback",
        "private feedback body",
        &json!({"private_source":"private source contents"}),
        true,
        Some(&worker),
    )
    .unwrap();
    db.steer(
        "first",
        "nonactionable",
        "private nonactionable",
        &json!({}),
        false,
        Some(&worker),
    )
    .unwrap();
    db.send(
        "first",
        &peer,
        "worker-message",
        &worker,
        "private worker conversation",
        &json!({}),
        true,
    )
    .unwrap();
    db.steer(
        "other",
        "other-task-feedback",
        "other task private",
        &json!({}),
        true,
        Some(&other),
    )
    .unwrap();
    let inspect = || {
        protocol::dispatch_scoped(
            &db,
            "inspect",
            json!({"task":"first"}),
            None,
            Some("default"),
        )
        .unwrap()["feedback_receipts"]
            .clone()
    };
    let receipts = inspect();
    assert_eq!(receipts.as_array().unwrap().len(), 1);
    assert_eq!(receipts[0]["message_id"], "stable-feedback");
    assert_eq!(receipts[0]["worker_id"], worker);
    assert_eq!(receipts[0]["ack"], 0);
    assert!(receipts[0]["ack_seq"].is_null());
    assert!(receipts[0]["created"].is_i64());
    assert!(!receipts.to_string().contains("private"));
    assert!(receipts[0].get("body").is_none());
    assert!(receipts[0].get("refs").is_none());
    db.event(
        "first",
        "run.checkpoint_verified",
        json!({"validation_id":"baseline","commit_sha":"same-commit"}),
    )
    .unwrap();
    db.acknowledge(&worker, &["stable-feedback".to_owned()])
        .unwrap();
    assert_eq!(inspect()[0]["ack"], 1);
    let ack_seq = inspect()[0]["ack_seq"].as_i64().unwrap();
    let before = protocol::dispatch_scoped(
        &db,
        "inspect",
        json!({"task":"first"}),
        None,
        Some("default"),
    )
    .unwrap()["checkpoint_event"]
        .clone();
    assert!(ack_seq > before["event_seq"].as_i64().unwrap());
    db.event(
        "first",
        "run.checkpoint_verified",
        json!({"validation_id":"after-feedback","commit_sha":"same-commit"}),
    )
    .unwrap();
    let after = protocol::dispatch_scoped(
        &db,
        "inspect",
        json!({"task":"first"}),
        None,
        Some("default"),
    )
    .unwrap()["checkpoint_event"]
        .clone();
    assert!(ack_seq < after["event_seq"].as_i64().unwrap());
    assert_eq!(after["validation_id"], "after-feedback");
    assert_eq!(after["commit_sha"], "same-commit");
    db.acknowledge(&worker, &["stable-feedback".to_owned()])
        .unwrap();
    assert_eq!(inspect()[0]["ack_seq"], ack_seq);
    let ack_count: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE task='first' AND kind='message.acknowledged'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ack_count, 1);
    let other_project =
        horde::projects::dispatch(&db, "project_create", &json!({"slug":"unrelated"}))
            .unwrap()
            .unwrap();
    assert!(
        protocol::dispatch_scoped(
            &db,
            "inspect",
            json!({"task":"first"}),
            None,
            other_project["id"].as_str()
        )
        .is_err()
    );
}
