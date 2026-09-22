use horde::{protocol, store::Store};
use serde_json::json;

#[test]
fn bound_project_rejects_switching_and_fleet_administration() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    for (method, args) in [
        ("list_tasks", json!({"project":"other"})),
        ("list_tasks", json!({"all_projects":true})),
        ("runtime_config_set", json!({"concurrency":100})),
        ("project_create", json!({"name":"unauthorized"})),
        ("account_grant", json!({"account":"other"})),
    ] {
        assert!(
            protocol::dispatch_scoped(&db, method, args, None, Some("default")).is_err(),
            "{method}"
        );
    }
}

#[test]
fn default_listing_is_scoped_and_project_tools_are_described() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    assert_eq!(
        protocol::dispatch_scoped(&db, "list_tasks", json!({}), None, Some("default")).unwrap(),
        json!([])
    );
    for name in [
        "project_create",
        "project_list",
        "project_repo_add",
        "account_create",
        "account_grant",
    ] {
        assert!(
            protocol::OPERATIONS
                .iter()
                .any(|(operation, _)| *operation == name)
        );
        assert!(protocol::admin_schema(name)["properties"].is_object());
    }
}

fn task(db: &Store, root: &std::path::Path, project: &str) -> String {
    let repo = root.join(project);
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    ] {
        horde::git::run(&repo, &args).unwrap();
    }
    let plan = horde::template::compile(
        "simulated",
        &horde::template::load_templates(std::path::Path::new("absent")).unwrap(),
        std::collections::BTreeMap::from([("task".into(), "test".into())]),
    )
    .unwrap();
    db.submit_project(
        project,
        "test",
        &repo,
        &horde::config::Settings::default(),
        &plan,
    )
    .unwrap()
}

#[test]
fn project_binding_protects_task_ids_artifact_hashes_and_workers() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(&dir.path().join("data")).unwrap();
    let hamster = protocol::dispatch(&db, "project_create", json!({"name":"hamster"}), None)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let own = task(&db, dir.path(), "default");
    let foreign = task(&db, dir.path(), &hamster);
    let hash = db
        .artifact(
            &foreign,
            None,
            "secret",
            b"private project data",
            &json!({}),
            false,
        )
        .unwrap();
    for method in [
        "inspect",
        "events",
        "metrics",
        "cancel",
        "knowledge",
        "get_artifact",
    ] {
        assert!(
            protocol::dispatch_scoped(
                &db,
                method,
                json!({"task":foreign,"hash":hash}),
                None,
                Some("default")
            )
            .is_err(),
            "{method}"
        );
    }
    assert!(
        protocol::dispatch_scoped(
            &db,
            "get_artifact",
            json!({"task":own,"hash":hash}),
            None,
            Some("default")
        )
        .is_err()
    );
    let worker = db.register(&foreign, None).unwrap();
    assert!(
        protocol::dispatch_scoped(
            &db,
            "knowledge_options",
            json!({}),
            worker["token"].as_str(),
            Some("default")
        )
        .is_err()
    );
    let tasks =
        protocol::dispatch_scoped(&db, "list_tasks", json!({}), None, Some("default")).unwrap();
    assert_eq!(tasks.as_array().unwrap().len(), 1);
    assert_eq!(tasks[0]["id"], own);
    assert_eq!(
        protocol::dispatch(&db, "list_tasks", json!({"all_projects":true}), None)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let projects =
        protocol::dispatch_scoped(&db, "project_list", json!({}), None, Some("default")).unwrap();
    assert_eq!(projects.as_array().unwrap().len(), 1);
    assert_eq!(projects[0]["id"], "default");
}

#[test]
fn submission_receipts_are_project_scoped_and_keep_legacy_default_retries() {
    let dir = tempfile::tempdir().unwrap();
    // An unused configuration directory keeps the developer's own settings out.
    let db =
        Store::open_with_config_dir(&dir.path().join("data"), &dir.path().join("config")).unwrap();
    let legacy_task = task(&db, dir.path(), "default");
    let legacy_args = json!({"request_id":"legacy","objective":"old request","repo":dir.path().join("default"),"template":"simulated"});
    let legacy_hash = horde::store::hash(&serde_json::to_vec(&legacy_args).unwrap());
    db.conn
        .execute(
            "INSERT INTO submission_receipts VALUES(?,?,?,?)",
            rusqlite::params![
                "legacy",
                legacy_hash,
                legacy_task,
                json!({"id":legacy_task}).to_string()
            ],
        )
        .unwrap();
    let retried = protocol::dispatch(&db, "submit_task", legacy_args, None).unwrap();
    assert_eq!(retried["id"], legacy_task);

    let hamster = protocol::dispatch(&db, "project_create", json!({"name":"hamster"}), None)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    task(&db, dir.path(), &hamster);
    let mut results = vec![];
    for project in ["default", hamster.as_str()] {
        let args = json!({"request_id":"shared-client-id","project":project,"objective":"new request","repo":dir.path().join(project),"template":"simulated"});
        let first = protocol::dispatch(&db, "submit_task", args.clone(), None).unwrap();
        assert_eq!(
            first,
            protocol::dispatch(&db, "submit_task", args, None).unwrap()
        );
        results.push(first["id"].clone());
    }
    assert_ne!(results[0], results[1]);
}

#[test]
fn bound_submission_requires_repository_registration() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(&dir.path().join("data")).unwrap();
    let repo = dir.path().join("unregistered");
    std::fs::create_dir(&repo).unwrap();
    let error = protocol::dispatch_scoped(
        &db,
        "submit_task",
        json!({"objective":"test","repo":repo,"template":"simulated"}),
        None,
        Some("default"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("not registered"), "{error:#}");
    assert!(horde::projects::infer(&db, &repo).unwrap().is_none());
}

#[tokio::test]
async fn cancelled_prelaunch_leases_reconcile_but_uncertain_attempts_keep_capacity() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(&dir.path().join("data")).unwrap();
    let orphan = task(&db, dir.path(), "default");
    let uncertain = task(&db, dir.path(), "default");
    let pending = task(&db, dir.path(), "default");
    for task in [&orphan, &uncertain, &pending] {
        let step = db.steps(task).unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        db.conn
            .execute(
                "INSERT INTO remote_account_leases VALUES(?,?,?,?,?,'active')",
                rusqlite::params![
                    step,
                    format!("lease-{task}"),
                    "controller",
                    format!("owner-{task}"),
                    task
                ],
            )
            .unwrap();
        if task != &pending {
            db.conn
                .execute("UPDATE tasks SET status='cancelled' WHERE id=?", [task])
                .unwrap();
        }
        if task == &uncertain {
            let worker = db.register(task, Some(&step)).unwrap();
            db.conn.execute("INSERT INTO attempts(id,step,worker,state,started) VALUES('uncertain-attempt',?,?,'uncertain',0)",rusqlite::params![step,worker["id"].as_str().unwrap()]).unwrap();
        }
    }
    // Make reconciliation fail locally before any network I/O, retaining retry state.
    std::fs::write(db.root.join("network-runtime.toml"), "invalid TOML !").unwrap();
    assert!(
        horde::project_runtime::reconcile_releases(&db)
            .await
            .is_err()
    );
    for (task, expected) in [
        (&orphan, "release_pending"),
        (&uncertain, "active"),
        (&pending, "active"),
    ] {
        let state: String = db
            .conn
            .query_row(
                "SELECT state FROM remote_account_leases WHERE task=?",
                [task],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, expected);
    }
}

#[test]
fn positive_worker_reconciliation_releases_account_capacity_and_queues_remote_release() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(&dir.path().join("data")).unwrap();
    let task = task(&db, dir.path(), "default");
    let step = db.steps(&task).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut config = horde::config::Settings::default()
        .executor("worker")
        .unwrap();
    let account = horde::accounts::dispatch(&db,"account_create",&json!({"project":"default","name":"single-slot","provider":config.kind,"auth_mode":config.auth_mode,"base_url":config.base_url,"concurrency":1})).unwrap().unwrap()["id"].as_str().unwrap().to_owned();
    config.account = Some(account.clone());
    horde::accounts::set_credential(
        &db,
        "default",
        &account,
        &horde::accounts::Credential {
            kind: "api_key".into(),
            secret: "test-key".into(),
            expires_at: None,
            metadata: json!({}),
        },
    )
    .unwrap();
    let profile: String = db
        .conn
        .query_row(
            "SELECT id FROM auth_profiles WHERE account=?",
            [&account],
            |row| row.get(0),
        )
        .unwrap();
    horde::accounts::reserve(
        &db,
        &step,
        &horde::accounts::Binding {
            role: "worker".into(),
            account: Some(account.clone()),
            profile: Some(profile),
            credential_version: Some(1),
        },
    )
    .unwrap();
    let worker = db.register(&task, Some(&step)).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    db.conn.execute("INSERT INTO attempts(id,step,worker,state,pid,started) VALUES('reconcile-attempt',?,?,'uncertain',?,0)",rusqlite::params![step,worker,std::process::id()]).unwrap();
    db.conn
        .execute(
            "INSERT INTO remote_account_leases VALUES(?,'lease','controller','owner',?,'active')",
            rusqlite::params![step, task],
        )
        .unwrap();
    let args = json!({"task":task,"worker":worker});
    assert!(protocol::dispatch(&db, "reconcile_worker", args.clone(), None).is_err());
    assert!(
        horde::accounts::select_account(&db, "default", &config)
            .unwrap()
            .is_none()
    );
    let state: String = db
        .conn
        .query_row(
            "SELECT state FROM account_reservations WHERE step=?",
            [&step],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "active");
    db.conn
        .execute(
            "UPDATE attempts SET pid=NULL WHERE id='reconcile-attempt'",
            [],
        )
        .unwrap();
    protocol::dispatch(&db, "reconcile_worker", args, None).unwrap();
    assert_eq!(
        horde::accounts::select_account(&db, "default", &config).unwrap(),
        Some(account)
    );
    let state: String = db
        .conn
        .query_row(
            "SELECT state FROM remote_account_leases WHERE step=?",
            [&step],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "release_pending");
}
