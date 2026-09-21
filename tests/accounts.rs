use horde::{accounts, config::Settings, store::Store};
use serde_json::json;

fn setup() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    accounts::migrate(&db.conn).unwrap();
    (dir, db)
}
fn create(db: &Store, name: &str) -> String {
    accounts::dispatch(db, "account_create", &json!({"project":"default","name":name,"provider":"tuara","auth_mode":"api","base_url":"https://tuara.com/router/v1","concurrency":2})).unwrap().unwrap()["id"].as_str().unwrap().to_owned()
}
#[test]
fn private_credentials_are_versioned_and_never_returned_by_inspect() {
    let (_dir, db) = setup();
    let id = create(&db, "one");
    accounts::set_credential(
        &db,
        "default",
        &id,
        &accounts::Credential {
            kind: "api_key".into(),
            secret: "test-secret-one".into(),
            expires_at: None,
            metadata: json!({}),
        },
    )
    .unwrap();
    let value = accounts::dispatch(
        &db,
        "account_inspect",
        &json!({"project":"default","account":id}),
    )
    .unwrap()
    .unwrap();
    assert!(!value.to_string().contains("test-secret"));
    assert_eq!(value["credential_version"], 1);
    assert_eq!(
        accounts::credential(&db, "default", &id).unwrap().secret,
        "test-secret-one"
    );
    assert!(accounts::credential(&db, "ungranted", &id).is_err());
    accounts::dispatch(
        &db,
        "account_revoke",
        &json!({"project":"default","account":id}),
    )
    .unwrap();
    assert!(accounts::credential(&db, "default", &id).is_err());
}

#[test]
fn received_shared_accounts_do_not_collide_with_local_account_names() {
    let (_controller_dir, controller) = setup();
    let account = create(&controller, "primary");
    key(&controller, &account);
    let envelope =
        accounts::provision(&controller, "default", &account, "local", "delivery").unwrap();
    let (_worker_dir, worker) = setup();
    let local = create(&worker, "primary");
    key(&worker, &local);
    accounts::receive(&worker, &envelope).unwrap();
    accounts::receive(&worker, &envelope).unwrap();
    assert_ne!(account, local);
    assert!(accounts::credential(&worker, "default", &account).is_ok());
    assert!(accounts::credential(&worker, "default", &local).is_ok());
}
#[test]
fn account_selection_balances_and_honors_limits_and_exhaustion() {
    let (_dir, db) = setup();
    let first = create(&db, "one");
    let second = create(&db, "two");
    for id in [&first, &second] {
        accounts::set_credential(
            &db,
            "default",
            id,
            &accounts::Credential {
                kind: "api_key".into(),
                secret: "test-key".into(),
                expires_at: None,
                metadata: json!({}),
            },
        )
        .unwrap();
    }
    let settings = Settings::default();
    let config = settings.executor("worker").unwrap();
    let one = accounts::select_account(&db, "default", &config)
        .unwrap()
        .unwrap();
    db.conn
        .execute("UPDATE accounts SET last_dispatch=100 WHERE id=?", [&one])
        .unwrap();
    let two = accounts::select_account(&db, "default", &config)
        .unwrap()
        .unwrap();
    assert_ne!(one, two);
    db.conn
        .execute("UPDATE accounts SET authenticated=0 WHERE id=?", [&two])
        .unwrap();
    assert_eq!(
        accounts::select_account(&db, "default", &config).unwrap(),
        Some(one)
    );
}
#[test]
fn profile_paths_reject_traversal() {
    let dir = tempfile::tempdir().unwrap();
    assert!(accounts::profile_directory(dir.path(), "../escape", "account").is_err());
    assert!(accounts::profile_directory(dir.path(), "project", "../escape").is_err());
    assert!(
        accounts::profile_directory(dir.path(), "project", "account")
            .unwrap()
            .starts_with(dir.path())
    );
}

fn task(db: &Store, id: &str, project: &str) -> String {
    db.conn
        .execute(
            "INSERT INTO tasks VALUES(?, 'test', '/temporary', 'running','{}','{}',0)",
            [id],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO task_projects VALUES(?,?,NULL)",
            rusqlite::params![id, project],
        )
        .unwrap();
    let step = format!("{id}-step");
    db.conn
        .execute(
            "INSERT INTO steps(id,task,name,spec,state) VALUES(?,?,'work','{}','pending')",
            rusqlite::params![step, id],
        )
        .unwrap();
    step
}
fn key(db: &Store, id: &str) {
    accounts::set_credential(
        db,
        "default",
        id,
        &accounts::Credential {
            kind: "api_key".into(),
            secret: "private-test-key".into(),
            expires_at: None,
            metadata: json!({}),
        },
    )
    .unwrap();
}
#[test]
fn reservations_span_projects_and_remote_hosts_and_survive_reopen() {
    let (dir, db) = setup();
    let account = create(&db, "shared");
    key(&db, &account);
    let hamster = horde::projects::dispatch(&db, "project_create", &json!({"slug":"hamster"}))
        .unwrap()
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    accounts::dispatch(
        &db,
        "account_grant",
        &json!({"project":hamster,"account":account}),
    )
    .unwrap();
    let first = task(&db, "first", "default");
    let second = task(&db, "second", &hamster);
    let settings = Settings::default();
    let binding = accounts::choose_for_step(&db, "first", &settings, "worker", &first)
        .unwrap()
        .unwrap();
    accounts::reserve(&db, &first, &binding).unwrap();
    accounts::reserve(&db, &first, &binding).unwrap();
    let remote = accounts::reserve_remote(
        &db,
        "default",
        "first",
        "remote",
        "attempt-1",
        "request-1",
        &settings,
        "worker",
    )
    .unwrap()
    .unwrap();
    assert_eq!(remote.account, Some(account.clone()));
    assert!(
        accounts::choose_for_step(&db, "second", &settings, "worker", &second)
            .unwrap()
            .is_none()
    );
    let again = accounts::reserve_remote(
        &db,
        "default",
        "first",
        "remote",
        "attempt-1",
        "request-1",
        &settings,
        "worker",
    )
    .unwrap()
    .unwrap();
    assert_eq!(again.account, remote.account);
    assert!(
        accounts::reserve_remote(
            &db,
            "default",
            "first",
            "remote",
            "attempt-2",
            "request-1",
            &settings,
            "worker"
        )
        .is_err()
    );
    accounts::mark_runtime_uncertain(&db, "remote").unwrap();
    drop(db);
    let db = Store::open(dir.path()).unwrap();
    assert!(
        accounts::choose_for_step(&db, "second", &settings, "worker", &second)
            .unwrap()
            .is_none()
    );
    accounts::release_remote(&db, "remote", "request-1").unwrap();
    assert!(
        accounts::choose_for_step(&db, "second", &settings, "worker", &second)
            .unwrap()
            .is_some()
    );
    db.conn
        .execute(
            "INSERT INTO attempts(id,step,state,started) VALUES('a',?,'uncertain',0)",
            [&first],
        )
        .unwrap();
    assert!(accounts::release_step(&db, &first).is_err());
}
#[test]
fn delivery_requires_lease_is_idempotent_and_cannot_change_or_regress() {
    let (_dir, db) = setup();
    let id = create(&db, "source");
    key(&db, &id);
    task(&db, "first", "default");
    assert!(accounts::provision(&db, "default", &id, "remote", "delivery").is_err());
    accounts::reserve_remote(
        &db,
        "default",
        "first",
        "remote",
        "attempt",
        "delivery",
        &Settings::default(),
        "worker",
    )
    .unwrap()
    .unwrap();
    let mut envelope = accounts::provision(&db, "default", &id, "remote", "delivery").unwrap();
    let (_receiver_dir, receiver) = setup();
    accounts::receive(&receiver, &envelope).unwrap();
    accounts::receive(&receiver, &envelope).unwrap();
    assert_eq!(
        accounts::credential(&receiver, "default", &id)
            .unwrap()
            .secret,
        "private-test-key"
    );
    envelope.credential.as_mut().unwrap().secret = "changed-secret".into();
    assert!(accounts::receive(&receiver, &envelope).is_err());
    key(&db, &id);
    assert!(accounts::provision(&db, "default", &id, "remote", "delivery").is_err());
    assert!(accounts::credential(&receiver, "other-project", &id).is_err());
}
#[test]
fn refresh_tokens_never_leave_controller_and_exhaustion_blocks_dispatch() {
    let (_dir, db) = setup();
    let id=accounts::dispatch(&db,"account_create",&json!({"project":"default","name":"codex","provider":"codex","auth_mode":"login","base_url":"https://api.openai.com/v1"})).unwrap().unwrap()["id"].as_str().unwrap().to_owned();
    accounts::set_credential(
        &db,
        "default",
        &id,
        &accounts::Credential {
            kind: "codex_refresh_token".into(),
            secret: "refresh-secret".into(),
            expires_at: None,
            metadata: json!({}),
        },
    )
    .unwrap();
    task(&db, "one", "default");
    let mut settings = Settings::default();
    settings.executors.get_mut("worker").unwrap().provider = Some("codex".into());
    accounts::reserve_remote(
        &db, "default", "one", "remote", "attempt", "delivery", &settings, "worker",
    )
    .unwrap()
    .unwrap();
    let envelope = accounts::provision(&db, "default", &id, "remote", "delivery").unwrap();
    assert!(envelope.credential.is_none());
    assert!(
        !serde_json::to_string(&envelope)
            .unwrap()
            .contains("refresh-secret")
    );
    accounts::release_remote(&db, "remote", "delivery").unwrap();
    horde::capacity::observe(
        &db,
        &horde::capacity::Snapshot {
            account: id.clone(),
            provider: "codex".into(),
            window: "primary".into(),
            used_percent: Some(100.0),
            reset_at: Some(horde::store::now() + 3600),
            observed_at: horde::store::now(),
            source: "provider".into(),
        },
    )
    .unwrap();
    assert!(
        accounts::select_account(&db, "default", &settings.executor("worker").unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn revoked_received_envelope_cannot_restore_access_and_reports_are_scoped() {
    let (_dir, db) = setup();
    let account = create(&db, "private");
    key(&db, &account);
    let envelope =
        accounts::provision(&db, "default", &account, "local", "local-delivery").unwrap();
    let (_remote, receiver) = setup();
    accounts::receive(&receiver, &envelope).unwrap();
    let hamster = horde::projects::dispatch(&db, "project_create", &json!({"slug":"hamster"}))
        .unwrap()
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        accounts::report_project(&db, &hamster).unwrap()["accounts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        accounts::remove_received(&receiver, "default", &account).unwrap()["removed"],
        true
    );
    assert!(accounts::receive(&receiver, &envelope).is_err());
    assert!(accounts::credential(&receiver, "default", &account).is_err());
    let step = task(&db, "one", "default");
    accounts::dispatch(
        &db,
        "account_revoke",
        &json!({"project":"default","account":account}),
    )
    .unwrap();
    assert!(
        accounts::choose_for_step(&db, "one", &Settings::default(), "worker", &step)
            .unwrap()
            .is_none()
    );
    accounts::dispatch(
        &db,
        "account_grant",
        &json!({"project":"default","account":account}),
    )
    .unwrap();
    let renewed = accounts::provision(&db, "default", &account, "local", "new-delivery").unwrap();
    accounts::receive(&receiver, &renewed).unwrap();
    assert_eq!(
        accounts::credential(&receiver, "default", &account)
            .unwrap()
            .secret,
        "private-test-key"
    );
}

#[test]
fn remote_attempt_revocation_uses_binding_without_local_reservation() {
    let (_dir, db) = setup();
    let account = create(&db, "remote-profile");
    key(&db, &account);
    let step = task(&db, "received", "default");
    let binding = accounts::choose_for_step(&db, "received", &Settings::default(), "worker", &step)
        .unwrap()
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO attempts(id,step,state,started) VALUES('remote-attempt',?,'running',0)",
            [&step],
        )
        .unwrap();
    horde::project_runtime::record(&db, "received", "remote-attempt", Some(&binding)).unwrap();
    assert!(!horde::project_runtime::revoked(&db, &step).unwrap());
    key(&db, &account);
    assert!(
        !horde::project_runtime::revoked(&db, &step).unwrap(),
        "rotation must not cancel a running invocation"
    );
    assert_eq!(
        accounts::remove_received(&db, "default", &account).unwrap()["removed"],
        false
    );
    assert!(horde::project_runtime::revoked(&db, &step).unwrap());
    db.conn
        .execute(
            "UPDATE attempts SET state='cancelled' WHERE id='remote-attempt'",
            [],
        )
        .unwrap();
    assert_eq!(
        accounts::remove_received(&db, "default", &account).unwrap()["removed"],
        true
    );
}

#[test]
fn unused_controller_credentials_and_project_profiles_are_reconciled() {
    let (_dir, db) = setup();
    let account = create(&db, "cleanable");
    key(&db, &account);
    let profile = accounts::profile_directory(&db.root, "default", &account).unwrap();
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("state"), "private-state").unwrap();
    let refresh = db.root.join("private/account-refresh").join(&account);
    std::fs::create_dir_all(&refresh).unwrap();
    std::fs::write(refresh.join("auth.json"), "private-refresh").unwrap();
    accounts::dispatch(
        &db,
        "account_revoke",
        &json!({"project":"default","account":account}),
    )
    .unwrap();
    accounts::cleanup_ungranted(&db).unwrap();
    assert!(!profile.exists());
    assert!(!refresh.exists());
    assert!(!db.root.join("private/accounts").join(&account).exists());
}

#[test]
fn project_slots_apply_to_simulated_work_and_hold_across_disconnects() {
    let (_dir, db) = setup();
    db.conn
        .execute("UPDATE projects SET concurrency=2 WHERE id='default'", [])
        .unwrap();
    let step = task(&db, "controller", "default");
    db.conn
        .execute(
            "INSERT INTO attempts(id,step,state,started) VALUES('local',?,'running',0)",
            [step],
        )
        .unwrap();
    assert!(
        accounts::reserve_project_remote(&db, "default", "controller", "worker-a", "request-a")
            .unwrap()
    );
    assert!(
        accounts::reserve_project_remote(&db, "default", "controller", "worker-a", "request-a")
            .unwrap()
    );
    assert!(
        !accounts::reserve_project_remote(&db, "default", "controller", "worker-b", "request-b")
            .unwrap()
    );
    assert_eq!(horde::project_runtime::active(&db, "default").unwrap(), 2);
    assert!(
        accounts::reserve_project_remote(&db, "default", "controller", "worker-b", "request-a")
            .is_err()
    );
    accounts::mark_runtime_uncertain(&db, "worker-a").unwrap();
    assert!(
        !accounts::reserve_project_remote(&db, "default", "controller", "worker-b", "request-b")
            .unwrap()
    );
    assert!(
        accounts::release_project_remote(&db, "default", "controller", "worker-b", "request-a")
            .is_err()
    );
    accounts::release_project_remote(&db, "default", "controller", "worker-a", "request-a")
        .unwrap();
    accounts::release_project_remote(&db, "default", "controller", "worker-a", "request-a")
        .unwrap();
    assert!(
        accounts::reserve_project_remote(&db, "default", "controller", "worker-b", "request-b")
            .unwrap()
    );
}

#[test]
fn release_before_acquire_leaves_immutable_project_tombstone() {
    let (_dir, db) = setup();
    task(&db, "cancelled", "default");
    accounts::release_project_remote(&db, "default", "cancelled", "worker", "lost-request")
        .unwrap();
    accounts::release_project_remote(&db, "default", "cancelled", "worker", "lost-request")
        .unwrap();
    assert!(
        accounts::reserve_project_remote(&db, "default", "cancelled", "worker", "lost-request")
            .is_err()
    );
    assert!(
        accounts::release_project_remote(
            &db,
            "default",
            "cancelled",
            "other-worker",
            "lost-request"
        )
        .is_err()
    );
    assert_eq!(horde::project_runtime::active(&db, "default").unwrap(), 0);
    assert!(
        accounts::reserve_project_remote(&db, "default", "cancelled", "worker", "new-request")
            .unwrap()
    );
}

#[test]
fn remote_account_selection_uses_pinned_remote_provider_with_optional_account_pin() {
    let (_dir, db) = setup();
    let tuara = create(&db, "controller-tuara");
    key(&db, &tuara);
    let codex=accounts::dispatch(&db,"account_create",&json!({"name":"remote-codex","provider":"codex","auth_mode":"login","base_url":"https://api.openai.com/v1"})).unwrap().unwrap()["id"].as_str().unwrap().to_owned();
    accounts::set_credential(
        &db,
        "default",
        &codex,
        &accounts::Credential {
            kind: "codex_refresh_token".into(),
            secret: "refresh-key".into(),
            expires_at: None,
            metadata: json!({}),
        },
    )
    .unwrap();
    for pinned in [false, true] {
        let task_id = if pinned { "pinned" } else { "pool" };
        task(&db, task_id, "default");
        let mut selected = json!({"runtime":"worker","capability":"codex-worker","provider":"remote-subscription","kind":"codex","auth_mode":"login","endpoint_hash":horde::store::hash(b"https://api.openai.com/v1"),"model":"gpt-model","configuration_hash":"a".repeat(64)});
        if pinned {
            selected["account"] = json!(codex);
        }
        let policy = json!({"version":1,"allowed":[{"runtime":"worker","capabilities":["codex-worker"]}],"bindings":[selected.clone()],"selected":selected});
        horde::execution_selection::pin(&db, task_id, &policy).unwrap();
        let binding = accounts::reserve_remote(
            &db,
            "default",
            task_id,
            "worker",
            task_id,
            task_id,
            &Settings::default(),
            "worker",
        )
        .unwrap()
        .unwrap();
        assert_eq!(binding.account, Some(codex.clone()));
        accounts::release_remote(&db, "worker", task_id).unwrap();
    }
}
