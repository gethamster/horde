use horde::{
    config::Settings,
    run,
    store::{Store, hash},
};
use rusqlite::params;
use serde_json::{Value, json};

struct Fixture {
    _dir: tempfile::TempDir,
    db: Store,
    task: String,
    before: String,
    configured: String,
}
impl Fixture {
    fn call(&self, key: &str) -> anyhow::Result<Value> {
        run::rebind_model(
            &self.db,
            &self.task,
            "codex",
            &hash(self.before.as_bytes()),
            &hash(self.configured.as_bytes()),
            key,
        )
    }
}
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    db.conn
        .execute(
            "INSERT INTO projects VALUES('recovery','recovery','Recovery',1,'native',1)",
            [],
        )
        .unwrap();
    let task = "model-recovery".to_owned();
    let mut settings = Settings::default();
    settings.providers.get_mut("codex").unwrap().model = Some("old-model".into());
    settings.providers.get_mut("codex").unwrap().account = Some("retained-account".into());
    settings.executors.get_mut("worker").unwrap().provider = Some("codex".into());
    let before = serde_json::to_string(&settings).unwrap();
    db.conn
        .execute(
            "INSERT INTO tasks VALUES(?,?,?,'failed',?,'{}',1)",
            params![task, "original objective", dir.path().to_str(), before],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO task_projects VALUES(?,'recovery',NULL)",
            [&task],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO steps VALUES('plan-step',?,'plan','{}','failed',NULL)",
            [&task],
        )
        .unwrap();
    db.conn.execute("INSERT INTO attempts(id,step,state,started,finished,result) VALUES('original-attempt','plan-step','failed',1,2,'original failure')",[]).unwrap();
    let provider = settings.executor("worker").unwrap();
    db.conn.execute("INSERT INTO accounts(id,owner_project,name,provider,auth_mode,base_url,concurrency,authenticated) VALUES('retained-account','recovery','retained',?,?,?,1,1)",params![provider.kind,provider.auth_mode,provider.base_url]).unwrap();
    db.conn.execute("INSERT INTO auth_profiles(id,account,credential_version) VALUES('profile','retained-account',1)",[]).unwrap();
    db.conn
        .execute(
            "INSERT INTO account_grants VALUES('recovery','retained-account',1)",
            [],
        )
        .unwrap();
    let mut configured = settings.clone();
    configured.providers.get_mut("codex").unwrap().model = Some("current-catalog-default".into());
    configured.providers.get_mut("codex").unwrap().account = None;
    let configured = toml::to_string(&configured).unwrap();
    let cfg_dir = dir.path().join("projects/recovery");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(cfg_dir.join("config.toml"), &configured).unwrap();
    Fixture {
        _dir: dir,
        db,
        task,
        before,
        configured,
    }
}
#[test]
fn model_rebind_preserves_original_run_account_and_replays_without_resuming() {
    let f = fixture();
    let response = f.call("same-original-operation").unwrap();
    assert_eq!(response["model_before"], "old-model");
    assert_eq!(response["model_after"], "current-catalog-default");
    let row = f.db.task(&f.task).unwrap();
    assert_eq!(row["status"], "failed");
    let mut after: Value = serde_json::from_str(row["settings"].as_str().unwrap()).unwrap();
    after["providers"]["codex"]["model"] = json!("old-model");
    assert_eq!(after, serde_json::from_str::<Value>(&f.before).unwrap());
    assert_eq!(
        f.db.rows(
            "SELECT state,result FROM attempts WHERE id='original-attempt'",
            &[]
        )
        .unwrap()[0]["result"],
        "original failure"
    );
    assert_eq!(f.call("same-original-operation").unwrap(), response);
    assert!(f.call("another-operation-with-old-settings").is_err());
    assert!(
        run::rebind_model(
            &f.db,
            &f.task,
            "claude",
            &hash(f.before.as_bytes()),
            &hash(f.configured.as_bytes()),
            "same-original-operation"
        )
        .is_err()
    );
    let receipts =
        f.db.rows("SELECT data FROM external_ops WHERE task=?", &[&f.task])
            .unwrap();
    assert_eq!(receipts.len(), 1);
    let receipt: Value = serde_json::from_str(receipts[0]["data"].as_str().unwrap()).unwrap();
    assert_eq!(receipt["settings_before_raw"], f.before);
    assert_eq!(
        f.db.rows(
            "SELECT kind FROM events WHERE task=? AND kind='run.model_rebound'",
            &[&f.task]
        )
        .unwrap()
        .len(),
        1
    );
}
#[test]
fn model_rebind_refuses_mutable_execution_and_accepted_work() {
    for sql in [
        "UPDATE tasks SET status='running'",
        "UPDATE attempts SET state='uncertain'",
        "UPDATE attempts SET state='waiting'",
        "UPDATE steps SET state='waiting'",
        "INSERT INTO questions VALUES('pending','model-recovery','question',NULL)",
        "INSERT INTO workers(id,task,status,token_hash,updated) VALUES('w','model-recovery','working','hash',1)",
        "INSERT INTO workers(id,task,status,token_hash,updated) VALUES('w','model-recovery','unresponsive','hash',1)",
        "INSERT INTO account_reservations VALUES('plan-step','recovery','retained-account','profile',1,'uncertain',1)",
        "INSERT INTO task_execution_policy VALUES('model-recovery','{}')",
        "UPDATE steps SET state='succeeded',result='{\"accepted\":true}'",
        "INSERT INTO events(task,kind,data,created) VALUES('model-recovery','run.checkpoint_verified','{}',1)",
        "DELETE FROM account_grants",
        "UPDATE accounts SET authenticated=0",
    ] {
        let f = fixture();
        f.db.conn.execute(sql, []).unwrap();
        assert!(f.call("guard").is_err(), "guard accepted: {sql}");
        assert_eq!(f.db.task(&f.task).unwrap()["settings"], f.before);
        assert!(
            f.db.rows("SELECT * FROM external_ops", &[])
                .unwrap()
                .is_empty()
        );
    }
}
#[test]
fn model_rebind_requires_exact_config_and_admin_transport() {
    let f = fixture();
    assert!(
        run::rebind_model(
            &f.db,
            &f.task,
            "codex",
            &"0".repeat(64),
            &hash(f.configured.as_bytes()),
            "stale-settings"
        )
        .is_err()
    );
    assert!(
        run::rebind_model(
            &f.db,
            &f.task,
            "codex",
            &hash(f.before.as_bytes()),
            &"0".repeat(64),
            "stale-config"
        )
        .is_err()
    );
    let changed = f
        .configured
        .replace("https://api.openai.com/v1", "https://unapproved.invalid/v1");
    std::fs::write(
        f._dir.path().join("projects/recovery/config.toml"),
        &changed,
    )
    .unwrap();
    assert!(
        run::rebind_model(
            &f.db,
            &f.task,
            "codex",
            &hash(f.before.as_bytes()),
            &hash(changed.as_bytes()),
            "endpoint-change"
        )
        .is_err()
    );
    assert!(!horde::protocol::worker_allowed("run_rebind_model"));
    assert!(!horde::protocol::project_allowed("run_rebind_model"));
}

#[test]
fn model_rebind_protocol_scope_and_atomic_receipt() {
    let f = fixture();
    let input = json!({"task":f.task,"project":"recovery","provider":"codex","expected_settings_hash":hash(f.before.as_bytes()),"expected_project_config_hash":hash(f.configured.as_bytes()),"idempotency_key":"protocol"});
    assert!(
        horde::protocol::dispatch_scoped(
            &f.db,
            "run_rebind_model",
            input.clone(),
            Some("not-a-worker-token"),
            None
        )
        .is_err()
    );
    assert!(
        horde::protocol::dispatch_scoped(
            &f.db,
            "run_rebind_model",
            input.clone(),
            None,
            Some("recovery")
        )
        .is_err()
    );
    let mut foreign = input.clone();
    foreign["project"] = json!("default");
    assert!(horde::protocol::dispatch(&f.db, "run_rebind_model", foreign, None).is_err());
    let mut injected = input.clone();
    injected["model"] = json!("caller-model");
    assert!(horde::protocol::dispatch(&f.db, "run_rebind_model", injected, None).is_err());
    f.db.conn.execute_batch("CREATE TRIGGER refuse_audit BEFORE INSERT ON events WHEN NEW.kind='run.model_rebound' BEGIN SELECT RAISE(ABORT,'fixture audit failure'); END;").unwrap();
    assert!(horde::protocol::dispatch(&f.db, "run_rebind_model", input.clone(), None).is_err());
    assert_eq!(f.db.task(&f.task).unwrap()["settings"], f.before);
    assert!(
        f.db.rows("SELECT * FROM external_ops", &[])
            .unwrap()
            .is_empty()
    );
    f.db.conn
        .execute_batch("DROP TRIGGER refuse_audit")
        .unwrap();
    let result = horde::protocol::dispatch(&f.db, "run_rebind_model", input.clone(), None).unwrap();
    assert_eq!(result["resumed"], false);
    f.db.conn
        .execute("UPDATE tasks SET status='running'", [])
        .unwrap();
    std::fs::remove_file(f._dir.path().join("projects/recovery/config.toml")).unwrap();
    assert_eq!(
        horde::protocol::dispatch(&f.db, "run_rebind_model", input, None).unwrap(),
        result
    );
}
