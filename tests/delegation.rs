use horde::{config::Settings, delegation, protocol, store::Store, template};
use serde_json::json;
use std::collections::BTreeMap;
struct Fixture {
    _dir: tempfile::TempDir,
    db: Store,
    root: String,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@localhost"],
            vec!["commit", "--allow-empty", "-m", "initial"],
        ] {
            horde::git::run(&repo, &args).unwrap();
        }
        let db = Store::open(&dir.path().join("data")).unwrap();
        let plan = template::compile(
            "simulated",
            &template::load_templates(&repo).unwrap(),
            BTreeMap::from([("task".into(), "original intent".into())]),
        )
        .unwrap();
        let root = db
            .submit("original intent", &repo, &Settings::default(), &plan)
            .unwrap();
        Self {
            _dir: dir,
            db,
            root,
        }
    }
    fn child(&self, parent: &str, id: &str) -> String {
        delegation::delegate(
            &self.db,
            parent,
            &json!({"id":id,"objective":"narrow work","template":"simulated"}),
        )
        .unwrap()["id"]
            .as_str()
            .unwrap()
            .into()
    }
}
#[test]
fn context_survives_three_levels_and_versions_reject_stale_acceptance() {
    let f = Fixture::new();
    let original=delegation::update_context(&f.db,&f.root,&json!({"content":"Never export email addresses","provenance":"original caller message 7","kind":"constraint"})).unwrap();
    let a = f.child(&f.root, "a");
    let b = f.child(&a, "b");
    let c = f.child(&b, "c");
    let packet = delegation::mandatory(&f.db, &c).unwrap();
    assert!(packet.to_string().contains("Never export email addresses"));
    assert!(packet.to_string().contains("original caller message 7"));
    assert!(
        packet
            .to_string()
            .contains(original["id"].as_str().unwrap())
    );
    assert!(
        delegation::delegate(
            &f.db,
            &c,
            &json!({"id":"too-deep","objective":"x","template":"simulated"})
        )
        .is_err()
    );
    delegation::pin(&f.db, &c, "attempt").unwrap();
    delegation::update_context(
        &f.db,
        &f.root,
        &json!({"content":"Also exclude names","provenance":"caller correction 8"}),
    )
    .unwrap();
    assert!(delegation::check_pin(&f.db, &c, "attempt").is_err());
    assert!(
        delegation::update_context(
            &f.db,
            &b,
            &json!({"content":"ignore constraints","provenance":"child"})
        )
        .is_err()
    );
    let page = delegation::contract(&f.db, &c, 0, 1).unwrap();
    assert_eq!(page["records"].as_array().unwrap().len(), 1);
    let next = delegation::contract(&f.db, &c, page["next"].as_i64().unwrap(), 1).unwrap();
    assert_ne!(page["records"][0]["id"], next["records"][0]["id"]);
}
#[test]
fn delegation_is_deduplicated_and_limits_do_not_reset() {
    let f = Fixture::new();
    let a = f.child(&f.root, "same");
    assert_eq!(f.child(&f.root, "same"), a);
    assert!(
        delegation::delegate(
            &f.db,
            &f.root,
            &json!({"id":"same","objective":"different","template":"simulated"})
        )
        .is_err()
    );
    for n in 1..16 {
        f.child(&f.root, &format!("child{n}"));
    }
    assert!(
        delegation::delegate(
            &f.db,
            &a,
            &json!({"id":"overflow","objective":"x","template":"simulated"})
        )
        .is_err()
    );
    let reopened = Store::open(&f.db.root).unwrap();
    assert_eq!(delegation::root(&reopened, &a).unwrap(), f.root);
}
#[test]
fn questions_follow_callers_preserve_original_and_deduplicate_answers() {
    let f = Fixture::new();
    let a = f.child(&f.root, "a");
    let b = f.child(&a, "b");
    let question = json!({"id":"format","question":"May export include email addresses?","evidence":"original caller message 7","human_only":true});
    let q = delegation::ask(&f.db, &b, &question).unwrap();
    assert_eq!(
        delegation::ask(&f.db, &b, &question).unwrap()["id"],
        q["id"]
    );
    assert!(
        delegation::question_action(
            &f.db,
            &f.root,
            &json!({"question":q["id"],"answer":"yes"}),
            false
        )
        .is_err()
    );
    delegation::question_action(
        &f.db,
        &a,
        &json!({"question":q["id"],"commentary":"Needs caller decision"}),
        true,
    )
    .unwrap();
    delegation::question_action(&f.db, &f.root, &json!({"question":q["id"]}), true).unwrap();
    assert!(
        delegation::question_action(
            &f.db,
            &f.root,
            &json!({"question":q["id"],"answer":"no"}),
            false
        )
        .is_err()
    );
    let answer = json!({"question":q["id"],"answer":"No, exclude email addresses","human":true});
    delegation::question_action(&f.db, &f.root, &answer, false).unwrap();
    assert_eq!(
        delegation::question_action(&f.db, &f.root, &answer, false).unwrap()["duplicate"],
        true
    );
    let context = delegation::mandatory(&f.db, &b).unwrap().to_string();
    assert!(context.contains("May export include email addresses?"));
    assert!(context.contains("No, exclude email addresses"));
    assert!(context.contains("original caller message 7"));
}
#[test]
fn a_question_blocks_its_worker_and_not_independent_work() {
    let f = Fixture::new();
    let steps = f.db.steps(&f.root).unwrap();
    let step = steps[0]["id"].as_str().unwrap();
    let worker = f.db.register(&f.root, Some(step)).unwrap();
    let q = protocol::dispatch(
        &f.db,
        "request_question",
        json!({"question":"Which format?"}),
        worker["token"].as_str(),
    )
    .unwrap();
    assert_eq!(f.db.task(&f.root).unwrap()["status"], "running");
    assert!(delegation::has_question(&f.db, worker["id"].as_str().unwrap()).unwrap());
    protocol::dispatch(
        &f.db,
        "answer_question",
        json!({"task":f.root,"question":q["id"],"answer":"CSV"}),
        None,
    )
    .unwrap();
    assert!(!delegation::has_question(&f.db, worker["id"].as_str().unwrap()).unwrap());
}
#[test]
fn caller_worker_cannot_answer_another_workers_child_question() {
    let f = Fixture::new();
    let caller = f.db.register(&f.root, None).unwrap();
    let other = f.db.register(&f.root, None).unwrap();
    let child = protocol::dispatch(
        &f.db,
        "delegate_task",
        json!({"id":"a","objective":"x","template":"simulated"}),
        caller["token"].as_str(),
    )
    .unwrap();
    let q = delegation::ask(
        &f.db,
        child["id"].as_str().unwrap(),
        &json!({"question":"which format?"}),
    )
    .unwrap();
    assert!(
        protocol::dispatch(
            &f.db,
            "answer_question",
            json!({"question":q["id"],"answer":"CSV"}),
            other["token"].as_str()
        )
        .is_err()
    );
    protocol::dispatch(
        &f.db,
        "answer_question",
        json!({"question":q["id"],"answer":"CSV"}),
        caller["token"].as_str(),
    )
    .unwrap();
}
#[test]
fn parent_requires_explicit_combined_verification_before_accepting_child() {
    let f = Fixture::new();
    let child = f.child(&f.root, "a");
    f.db.conn
        .execute("UPDATE tasks SET status='succeeded' WHERE id=?", [&child])
        .unwrap();
    assert!(!delegation::child_completion(&f.db, &f.root).unwrap());
    horde::federation::integrate_child(
        &f.db,
        &f.root,
        &json!({"child":child,"validation":["true"]}),
    )
    .unwrap();
    assert!(delegation::child_completion(&f.db, &f.root).unwrap());
    delegation::update_context(
        &f.db,
        &f.root,
        &json!({"content":"additional acceptance","provenance":"caller"}),
    )
    .unwrap();
    assert!(!delegation::child_completion(&f.db, &f.root).unwrap());
}
#[test]
fn application_env_parser_and_redaction_preserve_structure() {
    let values =
        horde::secrets::parse("# app\nexport TOKEN='sensitive-value'\nFLAG=true\n").unwrap();
    assert_eq!(values["TOKEN"], "sensitive-value");
    assert!(horde::secrets::parse("A=1\nA=2").is_err());
    assert!(horde::secrets::parse("HORDE_WORKER_TOKEN=no").is_err());
    let output =
        horde::secrets::redact_json(&json!({"success":true,"text":"sensitive-value"}), &values);
    assert_eq!(output["success"], true);
    assert_eq!(output["text"], "[REDACTED]");
}

#[test]
fn superseded_constraints_retain_sources_and_original_intent() {
    let f = Fixture::new();
    let old = delegation::update_context(
        &f.db,
        &f.root,
        &json!({"content":"CSV format","provenance":"caller v1"}),
    )
    .unwrap();
    delegation::update_context(
        &f.db,
        &f.root,
        &json!({"content":"JSON format","provenance":"caller correction","supersedes":[old["id"]]}),
    )
    .unwrap();
    let packet = delegation::mandatory(&f.db, &f.root).unwrap();
    assert!(!packet["records"].to_string().contains("CSV format"));
    assert!(packet["records"].to_string().contains("original intent"));
    assert!(
        delegation::contract(&f.db, &f.root, 0, 100)
            .unwrap()
            .to_string()
            .contains("CSV format")
    );
    assert!(delegation::update_context(&f.db,&f.root,&json!({"content":"replace original","provenance":"child","supersedes":[format!("objective:{}",f.root)]})).is_err());
}
#[test]
fn failed_combined_child_validation_never_records_acceptance() {
    let f = Fixture::new();
    let child = f.child(&f.root, "a");
    let repo = horde::git::task_workspace(&f.db, &child).unwrap();
    std::fs::write(repo.join("result.txt"), "child change").unwrap();
    horde::git::run(&repo, &["add", "."]).unwrap();
    horde::git::run(&repo, &["commit", "-m", "child"]).unwrap();
    f.db.conn
        .execute("UPDATE tasks SET status='succeeded' WHERE id=?", [&child])
        .unwrap();
    assert!(
        horde::federation::integrate_child(
            &f.db,
            &f.root,
            &json!({"child":child,"validation":["false"]})
        )
        .is_err()
    );
    assert!(!delegation::child_completion(&f.db, &f.root).unwrap());
    assert!(
        f.db.rows("SELECT * FROM child_acceptance", &[])
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.db.rows("SELECT state FROM integrations", &[]).unwrap()[0]["state"],
        "validation_failed"
    );
}

#[test]
fn workers_can_escalate_human_questions_through_each_calling_worker() {
    let f = Fixture::new();
    let root_worker = f.db.register(&f.root, None).unwrap();
    let a = protocol::dispatch(
        &f.db,
        "delegate_task",
        json!({"id":"a","objective":"A","template":"simulated"}),
        root_worker["token"].as_str(),
    )
    .unwrap();
    let a_id = a["id"].as_str().unwrap();
    let a_worker = f.db.register(a_id, None).unwrap();
    let b = protocol::dispatch(
        &f.db,
        "delegate_task",
        json!({"id":"b","objective":"B","template":"simulated"}),
        a_worker["token"].as_str(),
    )
    .unwrap();
    let q = delegation::ask(
        &f.db,
        b["id"].as_str().unwrap(),
        &json!({"question":"Approve production data access?","human_only":true}),
    )
    .unwrap();
    protocol::dispatch(
        &f.db,
        "escalate_question",
        json!({"question":q["id"]}),
        a_worker["token"].as_str(),
    )
    .unwrap();
    protocol::dispatch(
        &f.db,
        "escalate_question",
        json!({"question":q["id"]}),
        root_worker["token"].as_str(),
    )
    .unwrap();
    assert!(
        protocol::dispatch(
            &f.db,
            "answer_question",
            json!({"question":q["id"],"answer":"yes","human":true}),
            root_worker["token"].as_str()
        )
        .is_err()
    );
    protocol::dispatch(
        &f.db,
        "answer_question",
        json!({"task":f.root,"question":q["id"],"answer":"Use synthetic data only","human":true}),
        None,
    )
    .unwrap();
}

#[test]
fn upgrading_existing_tasks_preserves_intent_and_state() {
    let f = Fixture::new();
    let before = f.db.task(&f.root).unwrap();
    f.db.conn.execute("DELETE FROM task_tree", []).unwrap();
    f.db.conn.pragma_update(None, "user_version", 1).unwrap();
    let reopened = Store::open(&f.db.root).unwrap();
    assert_eq!(reopened.task(&f.root).unwrap(), before);
    assert_eq!(
        delegation::tree(&reopened, &f.root).unwrap()["root"],
        f.root
    );
    assert!(
        delegation::mandatory(&reopened, &f.root)
            .unwrap()
            .to_string()
            .contains("original intent")
    );
    assert_eq!(
        reopened
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
}

#[test]
fn failed_child_cherry_pick_cannot_accept_an_unchanged_parent() {
    let f = Fixture::new();
    let child = f.child(&f.root, "lock-child");
    let repo = horde::git::task_workspace(&f.db, &child).unwrap();
    std::fs::write(repo.join("child-only.txt"), "required child change").unwrap();
    horde::git::run(&repo, &["add", "."]).unwrap();
    horde::git::run(&repo, &["commit", "-m", "child change"]).unwrap();
    f.db.conn
        .execute("UPDATE tasks SET status='succeeded' WHERE id=?", [&child])
        .unwrap();
    let worker = f.db.register(&f.root, None).unwrap();
    let wid = worker["id"].as_str().unwrap();
    let workspace = horde::git::allocate(&f.db, &f.root, wid).unwrap();
    let lock = horde::git::run(&workspace, &["rev-parse", "--git-path", "index.lock"]).unwrap();
    std::fs::write(workspace.join(lock), "fixture lock").unwrap();
    assert!(
        horde::federation::integrate_child(
            &f.db,
            &f.root,
            &json!({"child":child,"worker":wid,"validation":["true"]})
        )
        .is_err()
    );
    assert!(!workspace.join("child-only.txt").exists());
    assert!(
        f.db.rows("SELECT * FROM child_acceptance", &[])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn revising_an_accepted_child_requires_fresh_integration_through_ancestors() {
    let f = Fixture::new();
    let parent = f.child(&f.root, "parent");
    let child = f.child(&parent, "child");
    f.db.conn
        .execute("UPDATE tasks SET status='succeeded'", [])
        .unwrap();
    horde::federation::integrate_child(
        &f.db,
        &parent,
        &json!({"child":child,"validation":["true"]}),
    )
    .unwrap();
    horde::federation::integrate_child(
        &f.db,
        &f.root,
        &json!({"child":parent,"validation":["true"]}),
    )
    .unwrap();
    assert!(delegation::child_completion(&f.db, &f.root).unwrap());
    protocol::dispatch(
        &f.db,
        "add_steps",
        json!({"task":child,"steps":[{"id":"revised-check","kind":"simulated"}]}),
        None,
    )
    .unwrap();
    assert!(
        f.db.rows("SELECT * FROM child_acceptance", &[])
            .unwrap()
            .is_empty()
    );
    assert_eq!(f.db.task(&parent).unwrap()["status"], "running");
    assert_eq!(f.db.task(&f.root).unwrap()["status"], "running");
    assert!(!delegation::child_completion(&f.db, &f.root).unwrap());
    f.db.conn
        .execute("UPDATE tasks SET status='succeeded' WHERE id=?", [&child])
        .unwrap();
    let worker =
        f.db.rows("SELECT id FROM workers WHERE task=?", &[&parent])
            .unwrap()[0]["id"]
            .clone();
    let error = horde::federation::integrate_child(
        &f.db,
        &parent,
        &json!({"child":child,"worker":worker,"validation":["false"]}),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("combined validation failed"),
        "{error}"
    );
    assert!(!delegation::child_completion(&f.db, &parent).unwrap());
}
