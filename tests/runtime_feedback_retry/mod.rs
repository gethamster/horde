#[test]
fn retry_prompt_preserves_acknowledged_operator_feedback_without_redelivery() {
    let (dir, db, task, other_task) = fixture();
    let initial = db.steps(&task).unwrap().remove(0);
    let (initial_attempt, worker, _) = begin(&db, &initial).unwrap();
    db.steer(
        &task,
        "completed-feedback",
        "OLD_COMPLETED_INTENT",
        &json!({}),
        true,
        Some(&worker),
    )
    .unwrap();
    db.acknowledge(&worker, &["completed-feedback".into()])
        .unwrap();
    db.finish(
        initial["id"].as_str().unwrap(),
        &initial_attempt,
        &worker,
        Ok(json!({"accepted":true})),
    )
    .unwrap();
    let peer = db.register(&task, initial["id"].as_str()).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let other = db.register(&other_task, None).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    db.steer(
        &task,
        "stable-feedback",
        "Exact revised Brief: preserve the release and add the requested link.",
        &json!({"private_ref":"NOT_IN_RETRY_CONTEXT"}),
        true,
        Some(&worker),
    )
    .unwrap();
    wake_notified(&db).unwrap();
    let followup = db.worker(&worker).unwrap()["step"]
        .as_str()
        .unwrap()
        .to_owned();
    let row = db
        .rows("SELECT * FROM steps WHERE id=?", &[&followup])
        .unwrap()
        .remove(0);
    let (attempt, same_worker, _) = begin(&db, &row).unwrap();
    assert_eq!(same_worker, worker);
    assert!(invocation_context(&db,&task,&followup,&worker,&attempt,vec![]).unwrap()["acknowledged_operator_feedback"]["records"].as_array().unwrap().is_empty());
    // Simulated workers do not allocate Git workspaces; preserve a temporary
    // workspace registration while exercising the real claim/recovery APIs.
    db.conn
        .execute(
            "UPDATE workers SET workspace=? WHERE id=?",
            rusqlite::params![dir.path().to_str().unwrap(), worker],
        )
        .unwrap();
    db.claim(&task, &worker, &["src".into()]).unwrap();
    db.acknowledge(&worker, &["stable-feedback".into()])
        .unwrap();
    db.steer(
        &task,
        "peer-feedback",
        "PRIVATE_OTHER_WORKER",
        &json!({}),
        true,
        Some(&peer),
    )
    .unwrap();
    db.acknowledge(&peer, &["peer-feedback".into()]).unwrap();
    db.steer(
        &other_task,
        "other-feedback",
        "PRIVATE_OTHER_TASK",
        &json!({}),
        true,
        Some(&other),
    )
    .unwrap();
    db.acknowledge(&other, &["other-feedback".into()]).unwrap();
    db.steer(
        &task,
        "informational",
        "NON_ACTIONABLE",
        &json!({}),
        false,
        Some(&worker),
    )
    .unwrap();
    db.send(
        &task,
        &peer,
        "peer-chat",
        &worker,
        "PRIVATE_PEER_CHAT",
        &json!({}),
        true,
    )
    .unwrap();
    db.acknowledge(&worker, &["informational".into(), "peer-chat".into()])
        .unwrap();
    db.conn
        .execute(
            "UPDATE attempts SET state='uncertain' WHERE id=?",
            [&attempt],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE workers SET status='uncertain' WHERE id=?",
            [&worker],
        )
        .unwrap();
    db.conn
        .execute("UPDATE steps SET state='failed' WHERE id=?", [&followup])
        .unwrap();
    drop(db);
    let db = Store::open_with_config_dir(dir.path(), &dir.path().join("config")).unwrap();
    let before = db
        .rows("SELECT * FROM receipts ORDER BY message,worker", &[])
        .unwrap();
    let ack_events = db
        .rows(
            "SELECT seq FROM events WHERE kind='message.acknowledged'",
            &[],
        )
        .unwrap();
    crate::protocol::dispatch(
        &db,
        "reconcile_worker",
        json!({"task":task,"worker":worker}),
        None,
    )
    .unwrap();
    crate::protocol::dispatch(
        &db,
        "resume",
        json!({"task":task,"request_id":"stable-recovery"}),
        None,
    )
    .unwrap();
    let row = db
        .rows("SELECT * FROM steps WHERE id=?", &[&followup])
        .unwrap()
        .remove(0);
    let (retry, same_worker, token) = begin(&db, &row).unwrap();
    assert_eq!(same_worker, worker);
    assert_ne!(retry, attempt);
    let context = invocation_context(&db, &task, &followup, &worker, &retry, vec![]).unwrap();
    assert!(context["messages"].as_array().unwrap().is_empty());
    let feedback = context["acknowledged_operator_feedback"]["records"]
        .as_array()
        .unwrap();
    assert_eq!(feedback.len(), 1);
    assert_eq!(feedback[0]["id"], "stable-feedback");
    assert!(feedback[0]["ack_seq"].as_i64().unwrap() > 0);
    let spec = Store::step(&row).unwrap();
    let settings = Settings::default();
    let prompt = Invocation {
        db: &db,
        task: &task,
        step: &followup,
        attempt: &retry,
        worker: &worker,
        token: &token,
        workspace: dir.path(),
        spec: &spec,
        settings: &settings,
        context,
    }
    .prompt()
    .unwrap();
    assert!(
        prompt.contains("Exact revised Brief: preserve the release and add the requested link.")
    );
    assert!(prompt.contains("stable-feedback"));
    for excluded in [
        "OLD_COMPLETED_INTENT",
        "PRIVATE_OTHER_WORKER",
        "PRIVATE_OTHER_TASK",
        "NON_ACTIONABLE",
        "PRIVATE_PEER_CHAT",
        "NOT_IN_RETRY_CONTEXT",
    ] {
        assert!(!prompt.contains(excluded), "leaked {excluded}");
    }
    assert_eq!(
        db.rows("SELECT * FROM receipts ORDER BY message,worker", &[])
            .unwrap(),
        before
    );
    assert_eq!(
        db.rows(
            "SELECT seq FROM events WHERE kind='message.acknowledged'",
            &[]
        )
        .unwrap(),
        ack_events
    );
    assert_eq!(
        db.rows("SELECT path FROM claims WHERE worker=?", &[&worker])
            .unwrap()[0]["path"],
        "src"
    );
    assert!(
        db.retry_operator_feedback(&other_task, &followup, &worker, &retry)
            .is_err()
    );
    db.steer(
        &task,
        "oversized",
        "x".repeat(256 * 1024).as_str(),
        &json!({}),
        true,
        Some(&worker),
    )
    .unwrap();
    db.acknowledge(&worker, &["oversized".into()]).unwrap();
    assert!(
        invocation_context(&db, &task, &followup, &worker, &retry, vec![])
            .unwrap_err()
            .to_string()
            .contains("256 KiB")
    );
}
