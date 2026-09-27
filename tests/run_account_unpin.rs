use horde::{config::Settings, run, store::Store};
use rusqlite::params;
use serde_json::json;

#[test]
fn account_unpin_is_operator_only_and_project_scoped() {
    assert!(!horde::protocol::worker_allowed("run_unpin_account"));
    assert!(horde::protocol::project_allowed("run_unpin_account"));
    let (_dir, db, task) = fixture();
    assert!(
        horde::protocol::dispatch_scoped(
            &db,
            "run_unpin_account",
            json!({"task":task,"account":"old-account","idempotency_key":"bound"}),
            None,
            Some("unknown-project"),
        )
        .is_err()
    );
    assert_eq!(
        horde::protocol::dispatch_scoped(
            &db,
            "run_unpin_account",
            json!({"task":task,"account":"old-account","idempotency_key":"bound"}),
            None,
            Some("default"),
        )
        .unwrap()["unpin_account"],
        "old-account"
    );
}

fn fixture() -> (tempfile::TempDir, Store, String) {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let task = "task-account-unpin".to_owned();
    let mut settings = Settings::default();
    settings.executors.get_mut("worker").unwrap().account = Some("old-account".into());
    settings.executors.get_mut("reviewer").unwrap().account = Some("old-account".into());
    db.conn.execute("INSERT INTO tasks(id,objective,repo,status,settings,plan,created) VALUES(?,?,?,'running',?,'{}',1)",
        params![task,"existing Run",dir.path().to_str(),serde_json::to_string(&settings).unwrap()]).unwrap();
    db.conn
        .execute(
            "INSERT INTO task_projects(task,project) VALUES(?,'default')",
            [&task],
        )
        .unwrap();
    let resolved = settings.executor("worker").unwrap();
    for (id, name) in [("old-account", "old"), ("new-account", "new")] {
        db.conn.execute("INSERT INTO accounts(id,owner_project,name,provider,auth_mode,base_url,concurrency,authenticated) VALUES(?,'default',?,?,?,?,1,1)",
            params![id,name,resolved.kind,resolved.auth_mode,resolved.base_url]).unwrap();
        db.conn
            .execute(
                "INSERT INTO auth_profiles(id,account,credential_version) VALUES(?,?,1)",
                params![format!("profile-{id}"), id],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO account_grants(project,account,created) VALUES('default',?,1)",
                [id],
            )
            .unwrap();
    }
    (dir, db, task)
}

#[test]
fn unpins_idle_run_to_project_account_pool_and_replays_lost_response() {
    let (_dir, db, task) = fixture();
    let before = db.task(&task).unwrap();
    let response = run::unpin_account(&db, &task, "old-account", "same-request").unwrap();
    assert_eq!(response["unpin_account"], "old-account");
    assert_eq!(response["task"], task);
    assert_eq!(response["roles"], json!(["reviewer", "worker"]));
    assert_eq!(
        run::unpin_account(&db, &task, "old-account", "same-request").unwrap(),
        response
    );
    assert!(run::unpin_account(&db, &task, "new-account", "same-request").is_err());
    let after = db.task(&task).unwrap();
    let settings: Settings = serde_json::from_str(after["settings"].as_str().unwrap()).unwrap();
    assert!(settings.executor("worker").unwrap().account.is_none());
    assert!(settings.executor("reviewer").unwrap().account.is_none());
    db.conn.execute("INSERT INTO account_capacity(account,window,provider,used,reset,observed,source) VALUES('old-account','primary','test',90,9999999999,9999999999,'provider')", []).unwrap();
    assert_eq!(
        horde::accounts::select_account(&db, "default", &settings.executor("reviewer").unwrap())
            .unwrap(),
        Some("new-account".into())
    );
    assert_eq!(before["id"], after["id"]);
    assert_eq!(before["status"], after["status"]);
    assert_eq!(before["plan"], after["plan"]);
    assert_eq!(
        db.rows(
            "SELECT kind FROM events WHERE task=? AND kind='run.account_unpinned'",
            &[&task]
        )
        .unwrap()
        .len(),
        1
    );
}

#[test]
fn unpin_requires_alternate_grant_and_no_live_attempt() {
    let (_dir, db, task) = fixture();
    db.conn
        .execute("DELETE FROM account_grants WHERE account='new-account'", [])
        .unwrap();
    assert!(run::unpin_account(&db, &task, "old-account", "missing-grant").is_err());
    db.conn
        .execute(
            "INSERT INTO account_grants(project,account,created) VALUES('default','new-account',1)",
            [],
        )
        .unwrap();
    db.conn.execute("INSERT INTO steps(id,task,name,spec,state) VALUES('active-step',?,'active','{}','running')",[&task]).unwrap();
    db.conn.execute("INSERT INTO attempts(id,step,state,started) VALUES('active-attempt','active-step','running',1)",[]).unwrap();
    assert!(run::unpin_account(&db, &task, "old-account", "live-attempt").is_err());
}

#[test]
fn unpin_holds_uncertain_account_reservations() {
    let (_dir, db, task) = fixture();
    db.conn
        .execute(
            "INSERT INTO steps(id,task,name,spec,state) VALUES('held-step',?,'held','{}','failed')",
            [&task],
        )
        .unwrap();
    db.conn.execute("INSERT INTO account_reservations(step,project,account,profile,credential_version,state,created) VALUES('held-step','default','old-account','profile-old-account',1,'uncertain',1)", []).unwrap();
    assert!(run::unpin_account(&db, &task, "old-account", "uncertain-reservation").is_err());
    assert!(
        db.rows("SELECT * FROM external_ops WHERE task=?", &[&task])
            .unwrap()
            .is_empty()
    );
}
