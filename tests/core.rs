use anyhow::Result;
use horde::{
    config::Settings,
    git, protocol,
    store::{Store, id, overlaps, scope},
    template,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};
use tempfile::TempDir;
struct Fixture {
    dir: TempDir,
    db: Store,
    oid: String,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
            vec!["commit", "--allow-empty", "-m", "initial"],
        ] {
            git::run(&repo, &args).unwrap();
        }
        let db = Store::open(&dir.path().join("data")).unwrap();
        let plan = template::compile(
            "simulated",
            &template::load_templates(Path::new("absent")).unwrap(),
            BTreeMap::from([("task".into(), "test".into())]),
        )
        .unwrap();
        let oid = db
            .submit("test", &repo, &Settings::default(), &plan)
            .unwrap();
        Self { dir, db, oid }
    }
    fn worker(&self) -> Value {
        self.db.register(&self.oid, None).unwrap()
    }
    fn coding_worker(&self) -> String {
        let w = self.worker();
        let wid = w["id"].as_str().unwrap();
        git::allocate(&self.db, &self.oid, wid).unwrap();
        wid.into()
    }
    fn call(&self, name: &str, mut args: Value) -> Result<Value> {
        args["task"] = json!(self.oid);
        protocol::dispatch(&self.db, name, args, None)
    }
}
#[test]
fn messages_are_durable_deduplicated_and_acknowledged_per_recipient() {
    let f = Fixture::new();
    let a = f.worker();
    let b = f.worker();
    let c = f.worker();
    let a = a["id"].as_str().unwrap();
    let b = b["id"].as_str().unwrap();
    let c = c["id"].as_str().unwrap();
    let mid = id();
    f.db.send(
        &f.oid,
        a,
        &mid,
        "task",
        "interface v2",
        &json!({"file":"src/api.rs"}),
        true,
    )
    .unwrap();
    let reopened = Store::open(&f.db.root).unwrap();
    assert_eq!(
        reopened
            .messages(b, 0, 100)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        reopened
            .send(
                &f.oid,
                a,
                &mid,
                "task",
                "interface v2",
                &json!({"file":"src/api.rs"}),
                true
            )
            .unwrap()["duplicate"],
        true
    );
    assert!(
        reopened
            .send(&f.oid, a, &mid, "task", "changed", &json!({}), true)
            .is_err()
    );
    reopened.acknowledge(b, std::slice::from_ref(&mid)).unwrap();
    assert_eq!(reopened.messages(b, 0, 100).unwrap(), json!([]));
    assert_eq!(
        reopened
            .messages(c, 0, 100)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(reopened.acknowledge(a, &[mid]).is_err());
    assert_eq!(reopened.worker(b).unwrap()["status"], "notified");
}
#[test]
fn direct_and_group_messages_do_not_leak() {
    let f = Fixture::new();
    let a = f.worker();
    let b = f.worker();
    let c = f.worker();
    for w in [&a, &b] {
        f.call("join_channel", json!({"worker":w["id"],"channel":"api"}))
            .unwrap();
    }
    f.call(
        "send_message",
        json!({"worker":a["id"],"id":id(),"destination":"group:api","body":"schema"}),
    )
    .unwrap();
    assert_eq!(
        f.db.messages(b["id"].as_str().unwrap(), 0, 100)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        f.db.messages(c["id"].as_str().unwrap(), 0, 100).unwrap(),
        json!([])
    );
    f.call(
        "send_message",
        json!({"worker":b["id"],"id":id(),"destination":c["id"],"body":"direct"}),
    )
    .unwrap();
    assert_eq!(
        f.db.messages(c["id"].as_str().unwrap(), 0, 100)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn out_of_order_ack_does_not_skip_unread() {
    let f = Fixture::new();
    let a = f.worker();
    let b = f.worker();
    let wid = b["id"].as_str().unwrap();
    let m1 = id();
    let m2 = id();
    for m in [&m1, &m2] {
        f.db.send(
            &f.oid,
            a["id"].as_str().unwrap(),
            m,
            wid,
            "hello",
            &json!({}),
            false,
        )
        .unwrap();
    }
    f.db.acknowledge(wid, &[m2]).unwrap();
    let cursor: i64 =
        f.db.conn
            .query_row("SELECT seq FROM cursors WHERE worker=?", [wid], |r| {
                r.get(0)
            })
            .unwrap();
    assert_eq!(f.db.messages(wid, cursor, 100).unwrap()[0]["id"], m1);
}
#[test]
fn independent_connections_cannot_take_overlapping_claims() {
    let f = Fixture::new();
    let a = f.coding_worker();
    let b = f.coding_worker();
    let root = f.db.root.clone();
    let oid = f.oid.clone();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = [a, b]
        .into_iter()
        .map(|wid| {
            let root = root.clone();
            let oid = oid.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let db = Store::open(&root).unwrap();
                barrier.wait();
                db.claim(&oid, &wid, &["src".into()]).is_ok()
            })
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .filter_map(|h| h.join().ok())
            .filter(|x| *x)
            .count(),
        1
    );
}
#[test]
fn claims_are_atomic_and_handoff_preserves_owner_on_failure() {
    let f = Fixture::new();
    let a = f.coding_worker();
    let b = f.coding_worker();
    f.db.claim(&f.oid, &a, &["src".into()]).unwrap();
    assert!(
        f.db.claim(&f.oid, &b, &["docs".into(), "src/api.rs".into()])
            .is_err()
    );
    assert!(f.db.check_write(&b, "docs/x").is_err());
    assert!(f.db.transfer(&f.oid, &b, &a, "src").is_err());
    f.db.check_write(&a, "src/api.rs").unwrap();
    f.db.transfer(&f.oid, &a, &b, "src").unwrap();
    assert!(f.db.check_write(&a, "src/api.rs").is_err());
    f.db.check_write(&b, "src/api.rs").unwrap();
}
#[test]
fn paths_are_component_aware_and_reject_traversal() {
    for p in ["../a", "/etc/passwd", "a/../../b", ".git/config", ""] {
        assert!(scope(p).is_err(), "{p}");
    }
    assert_eq!(scope("./src//file").unwrap(), "src/file");
    assert!(!overlaps("src/a", "src/ab"));
    assert!(overlaps(".", "src"));
    assert!(overlaps("src", "src/a"));
}
#[test]
fn worker_tokens_cannot_cross_tasks_or_invoke_admin_operations() {
    let f = Fixture::new();
    let w = f.worker();
    let token = w["token"].as_str().unwrap();
    assert!(protocol::dispatch(&f.db, "cancel", json!({"task":f.oid}), Some(token)).is_err());
    assert!(
        protocol::dispatch(&f.db, "list_workers", json!({"task":"other"}), Some(token)).is_err()
    );
    assert!(
        protocol::dispatch(
            &f.db,
            "read_messages",
            json!({"worker":"other"}),
            Some(token)
        )
        .is_err()
    );
    assert!(protocol::dispatch(&f.db, "list_workers", json!({}), Some(token)).is_ok());
    let artifact = protocol::dispatch(
        &f.db,
        "put_artifact",
        json!({"name":"x","content":"x","verified":true,"task":f.oid,"worker":w["id"]}),
        Some(token),
    )
    .unwrap();
    assert!(artifact["hash"].is_string());
    let knowledge = protocol::dispatch(&f.db, "add_knowledge", json!({"kind":"fact","content":"x","provenance":{},"verified":true,"task":f.oid,"worker":w["id"]}), Some(token)).unwrap();
    assert!(knowledge["id"].is_string());
    assert_eq!(
        f.db.rows(
            "SELECT verified FROM artifact_links WHERE task=? AND name='x'",
            &[&f.oid]
        )
        .unwrap()[0]["verified"],
        0
    );
    assert_eq!(
        f.db.rows("SELECT verified FROM knowledge WHERE task=?", &[&f.oid])
            .unwrap()[0]["verified"],
        0
    );
    let warnings =
        f.db.rows(
            "SELECT data FROM events WHERE task=? AND kind='tool.argument_dropped'",
            &[&f.oid],
        )
        .unwrap();
    assert_eq!(warnings.len(), 2);
    for row in warnings {
        assert_eq!(
            serde_json::from_str::<Value>(row["data"].as_str().unwrap()).unwrap()["field"],
            "verified"
        );
    }
    assert!(
        protocol::dispatch(
            &f.db,
            "list_workers",
            json!({"_runtime":"spoof"}),
            Some(token)
        )
        .is_err()
    );
}
#[test]
fn knowledge_and_artifact_reuse_require_provenance_and_matching_inputs() {
    let f = Fixture::new();
    assert!(
        f.call("add_knowledge", json!({"kind":"fact","content":"x"}))
            .is_err()
    );
    let k=f.call("add_knowledge",json!({"kind":"fact","content":"schema","provenance":{"file":"src/api.rs"},"verified":true})).unwrap();
    assert!(k["id"].is_string());
    let h = f
        .call(
            "put_artifact",
            json!({"name":"build","content":"data","inputs":{"head":"a"},"verified":true}),
        )
        .unwrap();
    assert_eq!(
        f.call("get_artifact", json!({"hash":h["hash"]})).unwrap()["content"],
        "data"
    );
    assert_eq!(
        f.call(
            "reuse_artifact",
            json!({"name":"build","inputs":{"head":"a"}})
        )
        .unwrap()
        .as_array()
        .unwrap()
        .len(),
        1
    );
    assert_eq!(
        f.call(
            "reuse_artifact",
            json!({"name":"build","inputs":{"head":"b"}})
        )
        .unwrap(),
        json!([])
    );
    std::fs::write(
        f.db.root
            .join("artifacts")
            .join(h["hash"].as_str().unwrap()),
        "corrupt",
    )
    .unwrap();
    assert!(f.call("get_artifact", json!({"hash":h["hash"]})).is_err());
}
#[test]
fn nested_templates_pin_versions_and_rewrite_output_references() {
    let all = template::load_templates(Path::new("absent")).unwrap();
    let plan = template::compile(
        "github-actions",
        &all,
        BTreeMap::from([("task".into(), "fix".into())]),
    )
    .unwrap();
    assert_eq!(plan.steps.len(), 7);
    assert_eq!(plan.pins.len(), 3);
    let implement = plan
        .steps
        .iter()
        .find(|s| s.id == "application.local.implement")
        .unwrap();
    assert!(
        implement
            .instructions
            .contains("${application.local.plan.result}")
    );
    assert_eq!(plan.steps.last().unwrap().needs, vec!["application.build"]);
    assert!(template::compile("nextjs", &all, BTreeMap::new()).is_err());
}
#[test]
fn invalid_compositions_are_rejected() {
    let mut all = template::load_templates(Path::new("absent")).unwrap();
    let mut t = all["simulated"].clone();
    t.name = "bad".into();
    t.steps[0].needs = vec!["verify".into()];
    all.insert("bad".into(), t.clone());
    assert!(template::compile("bad", &all, BTreeMap::from([("task".into(), "x".into())])).is_err());
    t.steps[0].needs = vec![];
    t.steps[0].template = Some("bad".into());
    all.insert("bad".into(), t);
    assert!(template::compile("bad", &all, BTreeMap::from([("task".into(), "x".into())])).is_err());
}
#[test]
fn restart_holds_attempts_and_preserves_claims() {
    let f = Fixture::new();
    let wid = f.coding_worker();
    f.db.claim(&f.oid, &wid, &["src".into()]).unwrap();
    let step = f.db.steps(&f.oid).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    f.db.conn
        .execute("UPDATE steps SET state='running' WHERE id=?", [&step])
        .unwrap();
    f.db.conn
        .execute(
            "INSERT INTO attempts(id,step,worker,state,started) VALUES(?,?,?,'running',0)",
            rusqlite::params![id(), step, wid],
        )
        .unwrap();
    assert_eq!(horde::runtime::recover(&f.db).unwrap(), 1);
    assert_eq!(f.db.task(&f.oid).unwrap()["status"], "blocked");
    f.db.check_write(&wid, "src/x").unwrap();
    assert!(f.call("resume", json!({})).is_err());
    f.call("reconcile_worker", json!({"worker":wid})).unwrap();
    f.call("resume", json!({})).unwrap();
    assert_eq!(f.db.task(&f.oid).unwrap()["status"], "running");
}
fn commit(db: &Store, wid: &str, file: &str, content: &str) {
    let w = db.worker(wid).unwrap();
    let root = Path::new(w["workspace"].as_str().unwrap());
    std::fs::write(root.join(file), content).unwrap();
    git::run(root, &["add", file]).unwrap();
    git::run(root, &["commit", "-m", "worker change"]).unwrap();
}
#[test]
fn real_merge_conflict_is_preserved_and_routed() {
    let f = Fixture::new();
    let a = f.coding_worker();
    let b = f.coding_worker();
    f.db.claim(&f.oid, &a, &["api.txt".into()]).unwrap();
    commit(&f.db, &a, "api.txt", "v1\n");
    git::integrate(&f.db, &f.oid, &a, &[]).unwrap();
    f.db.transfer(&f.oid, &a, &b, "api.txt").unwrap();
    commit(&f.db, &b, "api.txt", "v2\n");
    assert!(git::integrate(&f.db, &f.oid, &b, &[]).is_err());
    let target = git::task_workspace(&f.db, &f.oid).unwrap();
    assert_eq!(
        std::fs::read_to_string(target.join("api.txt")).unwrap(),
        "v1\n"
    );
    assert!(
        git::run(&target, &["status", "--porcelain"])
            .unwrap()
            .is_empty()
    );
    assert!(
        !f.db
            .messages(&b, 0, 100)
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    let w = f.db.worker(&b).unwrap();
    let work = Path::new(w["workspace"].as_str().unwrap());
    let _ = git::run(work, &["merge", &format!("horde/{}", f.oid)]);
    std::fs::write(work.join("api.txt"), "v2 compatible\n").unwrap();
    git::run(work, &["add", "api.txt"]).unwrap();
    git::run(work, &["commit", "-m", "resolve interface conflict"]).unwrap();
    git::integrate(&f.db, &f.oid, &b, &[]).unwrap();
    assert_eq!(
        std::fs::read_to_string(target.join("api.txt")).unwrap(),
        "v2 compatible\n"
    );
}
#[test]
fn independent_edits_merge_but_combined_validation_can_fail() {
    let f = Fixture::new();
    let a = f.coding_worker();
    let b = f.coding_worker();
    f.db.claim(&f.oid, &a, &["a.txt".into()]).unwrap();
    f.db.claim(&f.oid, &b, &["b.txt".into()]).unwrap();
    commit(&f.db, &a, "a.txt", "left");
    commit(&f.db, &b, "b.txt", "right");
    git::integrate(&f.db, &f.oid, &a, &[]).unwrap();
    assert!(
        git::integrate(
            &f.db,
            &f.oid,
            &b,
            &[
                "sh".into(),
                "-c".into(),
                "test ! -f a.txt || test ! -f b.txt".into()
            ]
        )
        .is_err()
    );
    assert_eq!(
        f.db.rows("SELECT state FROM integrations WHERE worker=?", &[&b])
            .unwrap()[0]["state"],
        "validation_failed"
    );
}
#[test]
fn out_of_scope_changes_are_held_before_integration() {
    let f = Fixture::new();
    let a = f.coding_worker();
    f.db.claim(&f.oid, &a, &["allowed".into()]).unwrap();
    commit(&f.db, &a, "outside.txt", "bad");
    assert!(git::integrate(&f.db, &f.oid, &a, &[]).is_err());
    assert!(
        !git::task_workspace(&f.db, &f.oid)
            .unwrap()
            .join("outside.txt")
            .exists()
    );
}
#[test]
fn mcp_and_direct_dispatch_have_equivalent_results() {
    let f = Fixture::new();
    let direct = f.call("inspect", json!({})).unwrap();
    let req = json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"inspect","arguments":{"task":f.oid}}});
    let response =
        protocol::mcp_response(&req, |n, a| protocol::dispatch(&f.db, n, a, None)).unwrap();
    let result: Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(result, direct);
    assert!(
        protocol::mcp_response(
            &json!({"method":"notifications/initialized"}),
            |_, _| panic!()
        )
        .is_none()
    );
}
#[test]
fn workflow_revision_is_validated_before_state_changes() {
    let f = Fixture::new();
    let before = f.db.steps(&f.oid).unwrap().len();
    assert!(
        f.call(
            "add_steps",
            json!({"steps":[{"id":"broken","needs":["absent"]}]})
        )
        .is_err()
    );
    assert_eq!(f.db.steps(&f.oid).unwrap().len(), before);
    f.call(
        "add_steps",
        json!({"steps":[{"id":"extra","kind":"simulated","needs":["verify"]}]}),
    )
    .unwrap();
    assert_eq!(f.db.steps(&f.oid).unwrap().len(), before + 1);
}
#[test]
fn confirmation_mode_waits_until_answered() {
    let f = Fixture::new();
    let all = template::load_templates(Path::new("absent")).unwrap();
    let plan = template::compile(
        "simulated",
        &all,
        BTreeMap::from([("task".into(), "x".into())]),
    )
    .unwrap();
    let settings = Settings {
        autonomy: false,
        ..Default::default()
    };
    let oid =
        f.db.submit("confirm", f.dir.path(), &settings, &plan)
            .unwrap();
    assert_eq!(f.db.task(&oid).unwrap()["status"], "waiting");
    assert!(protocol::dispatch(&f.db, "resume", json!({"task":oid}), None).is_err());
    let q =
        f.db.rows("SELECT id FROM questions WHERE task=?", &[&oid])
            .unwrap()[0]["id"]
            .clone();
    protocol::dispatch(
        &f.db,
        "answer_question",
        json!({"task":oid,"question":q,"answer":"yes"}),
        None,
    )
    .unwrap();
    assert_eq!(f.db.task(&oid).unwrap()["status"], "running");
}

#[tokio::test(flavor = "current_thread")]
async fn native_file_and_patch_tools_enforce_claims_and_reject_symlinks() {
    let f = Fixture::new();
    let wid = f.coding_worker();
    f.db.claim(&f.oid, &wid, &["allowed.txt".into(), "link.txt".into()])
        .unwrap();
    let w = f.db.worker(&wid).unwrap();
    let root = Path::new(w["workspace"].as_str().unwrap());
    let settings = Settings::default();
    let allowed = vec![
        "write_file".into(),
        "apply_patch".into(),
        "read_file".into(),
    ];
    assert!(
        horde::native::call(
            &f.db,
            &wid,
            "write_file",
            &json!({"path":"outside.txt","content":"bad"}),
            &settings,
            &allowed
        )
        .await
        .is_err()
    );
    std::fs::write(root.join("outside.txt"), "protected").unwrap();
    std::os::unix::fs::symlink("outside.txt", root.join("link.txt")).unwrap();
    assert!(
        horde::native::call(
            &f.db,
            &wid,
            "write_file",
            &json!({"path":"link.txt","content":"bad"}),
            &settings,
            &allowed
        )
        .await
        .is_err()
    );
    let patch = "diff --git a/allowed.txt b/allowed.txt\nnew file mode 100644\n--- /dev/null\n+++ b/allowed.txt\n@@ -0,0 +1 @@\n+hello\n";
    horde::native::call(
        &f.db,
        &wid,
        "apply_patch",
        &json!({"patch":patch}),
        &settings,
        &allowed,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("allowed.txt")).unwrap(),
        "hello\n"
    );
    let bad = patch.replace("allowed.txt", "unclaimed.txt");
    assert!(
        horde::native::call(
            &f.db,
            &wid,
            "apply_patch",
            &json!({"patch":bad}),
            &settings,
            &allowed
        )
        .await
        .is_err()
    );
    assert!(!root.join("unclaimed.txt").exists());
}
#[test]
fn output_references_are_checked_and_inserted_as_data() {
    let mut all = template::load_templates(Path::new("absent")).unwrap();
    all.get_mut("simulated").unwrap().steps[1].instructions = "${missing.result}".into();
    assert!(
        template::compile(
            "simulated",
            &all,
            BTreeMap::from([("task".into(), "x".into())])
        )
        .is_err()
    );
    assert_eq!(
        template::resolve_refs(
            "plan: ${plan.result}",
            &BTreeMap::from([("plan.result".into(), json!("code uses ${literal}"))])
        )
        .unwrap(),
        "plan: code uses ${literal}"
    );
}
#[test]
fn project_settings_merge_and_unknown_settings_fail() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".horde.toml"),
        "concurrency=2\n[executors.worker]\nprovider=\"claude\"\n",
    )
    .unwrap();
    let s = Settings::load(dir.path()).unwrap();
    assert_eq!(s.concurrency, 2);
    assert_eq!(s.executor("worker").unwrap().kind, "claude");
    assert!(s.executors.contains_key("planner"));
    std::fs::write(dir.path().join(".horde.toml"), "concurency=2\n").unwrap();
    assert!(Settings::load(dir.path()).is_err());
}
#[test]
fn one_named_provider_serves_every_role_that_does_not_override_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".horde.toml"),
        r#"
[providers.anthropic]
kind = "claude"
auth_mode = "api"
base_url = "https://api.anthropic.com/v1"
api_key_env = "ANTHROPIC_API_KEY"
model = "shared-model"
[executors.worker]
provider = "anthropic"
[executors.reviewer]
provider = "anthropic"
model = "reviewer-model"
max_tokens = 2048
"#,
    )
    .unwrap();
    let s = Settings::load(dir.path()).unwrap();
    let worker = s.executor("worker").unwrap();
    let reviewer = s.executor("reviewer").unwrap();
    // The key and endpoint are stated once and inherited by both roles.
    for role in [&worker, &reviewer] {
        assert_eq!(role.api_key_env, "ANTHROPIC_API_KEY");
        assert_eq!(role.base_url, "https://api.anthropic.com/v1");
        assert_eq!(role.auth_mode, "api");
        assert_eq!(role.kind, "claude");
    }
    // Model and limits are the per-role dimension; both are picked independently.
    assert_eq!(worker.model.as_deref(), Some("shared-model"));
    assert_eq!(reviewer.model.as_deref(), Some("reviewer-model"));
    assert_eq!(worker.max_tokens, 8192);
    assert_eq!(reviewer.max_tokens, 2048);
    // A role that names no provider still resolves against the API-keyed default.
    let planner = s.executor("planner").unwrap();
    assert_eq!(planner.kind, "tuara");
    assert_eq!(planner.auth_mode, "api");
    assert_eq!(planner.api_key_env, "TUARA_API_KEY");
}
#[test]
fn an_executor_naming_an_unconfigured_provider_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".horde.toml"),
        "[executors.worker]\nprovider=\"absent\"\n",
    )
    .unwrap();
    let error = Settings::load(dir.path()).unwrap_err().to_string();
    assert!(error.contains("absent"), "{error}");
}
#[test]
fn a_role_cannot_restate_the_connection_and_is_told_where_it_moved() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".horde.toml"),
        "[executors.worker]\nkind=\"codex\"\n",
    )
    .unwrap();
    let error = Settings::load(dir.path()).unwrap_err().to_string();
    assert!(error.contains("belongs to a provider"), "{error}");
    assert!(error.contains("providers."), "{error}");
}
#[test]
fn a_new_provider_never_inherits_another_providers_endpoint_or_key() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".horde.toml"),
        "[providers.openai]\nkind=\"codex\"\nauth_mode=\"api\"\n",
    )
    .unwrap();
    let error = Settings::load(dir.path()).unwrap_err().to_string();
    assert!(error.contains("base_url"), "{error}");
}
#[test]
fn install_writes_a_private_starter_config_and_never_overwrites_it() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("horde");
    let created = horde::config::initialize(&home).unwrap();
    assert_eq!(
        created,
        vec![home.join("config.toml"), home.join("credentials.env")]
    );
    use std::os::unix::fs::PermissionsExt;
    for file in &created {
        let mode = std::fs::metadata(file).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{} is not private", file.display());
    }
    // A second install leaves whatever the user has since put in these files alone.
    std::fs::write(home.join("config.toml"), "concurrency = 9\n").unwrap();
    assert!(horde::config::initialize(&home).unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(home.join("config.toml")).unwrap(),
        "concurrency = 9\n"
    );
}
#[test]
fn the_starter_configuration_states_exactly_the_built_in_defaults() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".horde.toml"), horde::config::STARTER).unwrap();
    let starter = Settings::load(dir.path()).unwrap();
    assert_eq!(
        toml::Value::try_from(&starter).unwrap(),
        toml::Value::try_from(Settings::default()).unwrap()
    );
}
#[test]
fn newer_database_schema_is_not_silently_downgraded() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    db.conn.execute_batch("PRAGMA user_version=999").unwrap();
    drop(db);
    assert!(Store::open(dir.path()).is_err());
}

#[test]
fn parent_claim_handoff_moves_descendants_atomically() {
    let f = Fixture::new();
    let a = f.coding_worker();
    let b = f.coding_worker();
    f.db.claim(&f.oid, &a, &["src".into(), "src/api".into()])
        .unwrap();
    assert!(f.db.transfer(&f.oid, &a, &b, "src/api").is_err());
    f.db.transfer(&f.oid, &a, &b, "src").unwrap();
    assert!(f.db.check_write(&a, "src/api/x").is_err());
    f.db.check_write(&b, "src/api/x").unwrap();
}
#[test]
fn named_outputs_have_a_runtime_type_contract() {
    let step: template::Step =
        serde_json::from_value(json!({"id":"step","output_types":{"count":"integer"}})).unwrap();
    assert!(template::validate_result(&step, &json!({"count":"wrong"})).is_err());
    assert!(template::validate_result(&step, &json!({})).is_err());
    template::validate_result(&step, &json!({"count":3})).unwrap();
}
#[test]
fn required_information_question_accepts_an_ordinary_answer() {
    let f = Fixture::new();
    let tid = f.db.steps(&f.oid).unwrap()[0]["id"].clone();
    let w = f.db.register(&f.oid, tid.as_str()).unwrap();
    let token = w["token"].as_str().unwrap();
    let q = protocol::dispatch(
        &f.db,
        "request_question",
        json!({"question":"Which format is required?"}),
        Some(token),
    )
    .unwrap();
    assert_eq!(f.db.task(&f.oid).unwrap()["status"], "running");
    f.db.conn
        .execute("UPDATE steps SET state='failed' WHERE id=?", [tid.as_str()])
        .unwrap();
    f.call(
        "answer_question",
        json!({"question":q["id"],"answer":"CSV"}),
    )
    .unwrap();
    assert_eq!(f.db.task(&f.oid).unwrap()["status"], "running");
    assert_eq!(f.db.steps(&f.oid).unwrap()[0]["state"], "pending");
}
#[test]
fn allocation_recovers_a_worktree_created_before_registration_was_persisted() {
    let f = Fixture::new();
    let w = f.worker();
    let wid = w["id"].as_str().unwrap();
    let first = git::allocate(&f.db, &f.oid, wid).unwrap();
    f.db.conn
        .execute(
            "UPDATE workers SET workspace=NULL,branch=NULL,base=NULL WHERE id=?",
            [wid],
        )
        .unwrap();
    assert_eq!(git::allocate(&f.db, &f.oid, wid).unwrap(), first);
}
#[test]
fn metrics_do_not_present_unreported_cost_as_known_zero() {
    let f = Fixture::new();
    let tid = f.db.steps(&f.oid).unwrap()[0]["id"].clone();
    f.db.conn.execute("INSERT INTO attempts(id,step,state,started,finished,usage) VALUES(?,?,'succeeded',1,2,?)",rusqlite::params![id(),tid.as_str(),json!({"provider":{"input_tokens":100,"output_tokens":10,"cached_input_tokens":50}}).to_string()]).unwrap();
    let m = horde::metrics::report(&f.db, &f.oid).unwrap();
    assert_eq!(m["attempts_without_cost"], 1);
    assert_eq!(m["reported_input_tokens"], 100);
    assert_eq!(m["reported_cached_tokens"], 50);
    assert!(m["subscription_capacity"].is_null());
}

#[test]
fn planner_proposals_evolve_the_workflow_and_gate_existing_successors() {
    let f = Fixture::new();
    let step = f.db.steps(&f.oid).unwrap()[0].clone();
    let tid = step["id"].as_str().unwrap();
    let mut spec = Store::step(&step).unwrap();
    spec.role = "planner".into();
    f.db.conn
        .execute(
            "UPDATE steps SET state='running',spec=? WHERE id=?",
            rusqlite::params![serde_json::to_string(&spec).unwrap(), tid],
        )
        .unwrap();
    let mut plan: template::Plan =
        serde_json::from_str(f.db.task(&f.oid).unwrap()["plan"].as_str().unwrap()).unwrap();
    plan.steps[0].role = "planner".into();
    f.db.conn
        .execute(
            "UPDATE tasks SET plan=? WHERE id=?",
            rusqlite::params![serde_json::to_string(&plan).unwrap(), f.oid],
        )
        .unwrap();
    let w = f.db.register(&f.oid, Some(tid)).unwrap();
    let token = w["token"].as_str().unwrap();
    protocol::dispatch(&f.db,"propose_steps",json!({"task":f.oid,"worker":w["id"],"steps":[{"id":"api","kind":"simulated","scope":["src/api"]},{"id":"ui","kind":"simulated","scope":["src/ui"]}]}),Some(token)).unwrap();
    let steps = f.db.steps(&f.oid).unwrap();
    assert_eq!(steps.len(), 6);
    let left = steps.iter().find(|t| t["name"] == "left").unwrap();
    let left = Store::step(left).unwrap();
    assert!(left.needs.contains(&"api".into()));
    assert!(left.needs.contains(&"ui".into()));
    assert!(horde::runtime::ready(&f.db, &f.oid).unwrap().is_empty());
}
#[test]
fn adding_a_provider_keeps_the_key_out_of_settings_and_the_comments_in() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("horde");
    horde::config::initialize(&home).unwrap();
    let spec = horde::provisioning::Spec {
        name: "anthropic".into(),
        roles: vec!["reviewer".into()],
        model: Some("a-reviewing-model".into()),
        ..Default::default()
    }
    .with_preset(horde::provisioning::preset("anthropic").unwrap());
    horde::provisioning::apply(&home, &spec, Some("sk-secret".into())).unwrap();

    let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
    // The key lives in credentials.env alone; settings carry only the variable name.
    assert!(!config.contains("sk-secret"), "{config}");
    assert!(config.contains("ANTHROPIC_API_KEY"));
    // The starter's commentary survives a surgical edit.
    assert!(config.contains("# Providers are declared once"), "{config}");
    let credentials = std::fs::read_to_string(home.join("credentials.env")).unwrap();
    assert!(
        credentials.contains("ANTHROPIC_API_KEY='sk-secret'"),
        "{credentials}"
    );
    assert!(credentials.contains("# Daemon-only provider credentials"));

    let settings = Settings::load_dir(&home).unwrap();
    let reviewer = settings.executor("reviewer").unwrap();
    assert_eq!(reviewer.kind, "claude");
    assert_eq!(reviewer.auth_mode, "api");
    assert_eq!(reviewer.api_key_env, "ANTHROPIC_API_KEY");
    assert_eq!(reviewer.model.as_deref(), Some("a-reviewing-model"));
    // Roles that were not named keep the provider they had.
    assert_eq!(settings.executor("worker").unwrap().kind, "tuara");
}
#[test]
fn re_adding_a_provider_rotates_its_key_without_duplicating_anything() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("horde");
    horde::config::initialize(&home).unwrap();
    let spec = || {
        horde::provisioning::Spec {
            name: "tuara".into(),
            ..Default::default()
        }
        .with_preset(horde::provisioning::preset("tuara").unwrap())
    };
    horde::provisioning::apply(&home, &spec(), Some("first".into())).unwrap();
    horde::provisioning::apply(&home, &spec(), Some("second".into())).unwrap();
    let credentials = std::fs::read_to_string(home.join("credentials.env")).unwrap();
    // Exactly one live entry; the commented example in the starter is left alone.
    let live: Vec<_> = credentials
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && l.contains("TUARA_API_KEY="))
        .collect();
    assert_eq!(live, vec!["TUARA_API_KEY='second'"], "{credentials}");
    assert!(!credentials.contains("first"));
    // A second write must not leave a duplicate table behind for TOML to reject.
    Settings::load_dir(&home).unwrap();
    use std::os::unix::fs::PermissionsExt;
    for name in ["config.toml", "credentials.env"] {
        let mode = std::fs::metadata(home.join(name))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{name} is not private");
    }
}
#[test]
fn adding_a_preset_configures_the_matching_provider_instead_of_a_twin() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("horde");
    horde::config::initialize(&home).unwrap();
    let tuara = horde::provisioning::preset("tuara").unwrap();
    let spec = horde::provisioning::Spec {
        name: "tuara".into(),
        ..Default::default()
    }
    .with_preset(tuara);
    horde::provisioning::apply(&home, &spec, Some("sk-key".into())).unwrap();

    // providers.default is already this endpoint, so the key lands there and the
    // roles that rely on it keep working, rather than a second Tuara appearing.
    let settings = Settings::load_dir(&home).unwrap();
    assert!(
        !settings.providers.contains_key("tuara"),
        "a duplicate provider was created"
    );
    assert_eq!(settings.providers["default"].kind, "tuara");

    // An explicitly different name is still honoured as a separate provider.
    let second = horde::provisioning::Spec {
        name: "tuara-fast".into(),
        model: Some("qwen/qwen3.8-flash".into()),
        ..Default::default()
    }
    .with_preset(tuara);
    horde::provisioning::apply(&home, &second, None).unwrap();
    let settings = Settings::load_dir(&home).unwrap();
    assert_eq!(
        settings.providers["tuara-fast"].model.as_deref(),
        Some("qwen/qwen3.8-flash")
    );
}
#[test]
fn a_provider_whose_settings_would_not_load_is_reported_not_silently_written() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("horde");
    horde::config::initialize(&home).unwrap();
    let spec = horde::provisioning::Spec {
        name: "broken".into(),
        kind: Some("tuara".into()),
        auth_mode: Some("api".into()),
        ..Default::default()
    };
    let error = horde::provisioning::apply(&home, &spec, None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("no longer load"), "{error}");
}

#[test]
fn workflow_input_errors_name_the_field_without_mutating_the_plan() {
    let invalid = [
        (json!([{"id":"bad","needs":"init"}]), "steps[0].needs"),
        (
            json!([{"id":"bad","environment":{"timeout_seconds":"slow"}}]),
            "steps[0].environment.timeout_seconds",
        ),
        (json!([{"name":"bad"}]), "unknown field `name`"),
        (json!([{"instructions":"missing id"}]), "missing field `id`"),
        (json!([{"id":"bad","attempts":0}]), ".attempts"),
        (json!([{"id":"bad","needs":["missing"]}]), ".needs"),
    ];
    for operation in ["add_steps", "propose_steps"] {
        let f = Fixture::new();
        let row = f.db.steps(&f.oid).unwrap()[0].clone();
        let mut step = Store::step(&row).unwrap();
        step.role = "planner".into();
        let tid = row["id"].as_str().unwrap();
        f.db.conn
            .execute(
                "UPDATE steps SET state='running',spec=? WHERE id=?",
                rusqlite::params![serde_json::to_string(&step).unwrap(), tid],
            )
            .unwrap();
        let worker = f.db.register(&f.oid, Some(tid)).unwrap();
        let before_steps = f.db.steps(&f.oid).unwrap();
        let before_plan = f.db.task(&f.oid).unwrap()["plan"].clone();
        let before_revisions =
            f.db.rows("SELECT * FROM revisions WHERE task=?", &[&f.oid])
                .unwrap();
        for (steps, expected) in &invalid {
            let error = protocol::dispatch(
                &f.db,
                operation,
                json!({"task":f.oid,"worker":worker["id"],"steps":steps}),
                None,
            )
            .unwrap_err()
            .to_string();
            assert!(
                error.contains(expected),
                "{operation}: {error}; expected {expected}"
            );
            assert_eq!(f.db.steps(&f.oid).unwrap(), before_steps);
            assert_eq!(f.db.task(&f.oid).unwrap()["plan"], before_plan);
            assert_eq!(
                f.db.rows("SELECT * FROM revisions WHERE task=?", &[&f.oid])
                    .unwrap(),
                before_revisions
            );
        }
    }
}

#[test]
fn mcp_returns_nested_step_validation_as_a_tool_result() {
    let f = Fixture::new();
    let reply = protocol::mcp_response(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
        "name":"add_steps","arguments":{"task":f.oid,"steps":[{"id":"check","when":{"step":"verify","status":12}}]}
    }}), |name, args| protocol::dispatch(&f.db, name, args, None)).unwrap();
    assert!(reply.get("error").is_none());
    assert_eq!(reply["result"]["isError"], true);
    assert!(
        reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("steps[0].when.status")
    );
}

#[test]
fn native_extra_body_cannot_replace_workflow_fields() {
    let dir = tempfile::tempdir().unwrap();
    for key in [
        "model",
        "messages",
        "tools",
        "stream",
        "max_tokens",
        "max_price",
        "n",
    ] {
        std::fs::write(
            dir.path().join(".horde.toml"),
            format!("[providers.default.extra_body]\n{key}=\"override\"\n"),
        )
        .unwrap();
        let error = Settings::load(dir.path()).unwrap_err().to_string();
        assert!(
            error.contains(&format!("extra_body.{key} is reserved")),
            "{error}"
        );
    }
}

#[test]
fn native_tool_events_bound_and_redact_arguments_and_results() {
    use horde::executor::{Invocation, record_tool_completed};
    let f = Fixture::new();
    let worker = f.worker();
    let row = f.db.steps(&f.oid).unwrap()[0].clone();
    let step = Store::step(&row).unwrap();
    let settings = Settings {
        tool_event_bytes: 80,
        ..Default::default()
    };
    let secret = "synthetic-\n\"provider-secret";
    f.db.conn
        .execute(
            "INSERT INTO task_bundles VALUES(?,?,?)",
            rusqlite::params![f.oid, "event-app", "v1"],
        )
        .unwrap();
    let bundles = f.db.root.join("remote-secrets").join(&f.oid);
    std::fs::create_dir_all(&bundles).unwrap();
    std::fs::write(
        bundles.join(horde::store::hash(b"event-app")),
        json!({"version":"v1","values":{"APP_SECRET":"synthetic-app-secret"}}).to_string(),
    )
    .unwrap();
    let payload = json!({"a_key":secret,"b_key":"synthetic-app-secret","z_text":"界".repeat(100)});
    let i = Invocation {
        db: &f.db,
        task: &f.oid,
        step: row["id"].as_str().unwrap(),
        attempt: "test",
        worker: worker["id"].as_str().unwrap(),
        token: worker["token"].as_str().unwrap(),
        workspace: f.dir.path(),
        spec: &step,
        settings: &settings,
        context: json!({}),
    };
    record_tool_completed(&i, "read_file", &payload, &Ok(payload.clone()), 7, secret).unwrap();
    let rows =
        f.db.rows(
            "SELECT data FROM events WHERE kind='tool.completed' AND task=?",
            &[&f.oid],
        )
        .unwrap();
    let event: Value = serde_json::from_str(rows[0]["data"].as_str().unwrap()).unwrap();
    for (field, flag) in [
        ("arguments", "arguments_truncated"),
        ("result", "result_truncated"),
    ] {
        let text = event[field].as_str().unwrap();
        assert!(text.len() <= 80);
        assert!(text.contains("[REDACTED]"));
        assert!(!text.contains("synthetic"));
        assert_eq!(event[flag], true);
    }
    assert_eq!(event["duration_ms"], 7);
    assert_eq!(event["success"], true);
    let disabled = Settings {
        tool_event_bytes: 0,
        ..settings.clone()
    };
    let i = Invocation {
        settings: &disabled,
        ..i
    };
    record_tool_completed(&i, "read_file", &payload, &Ok(payload.clone()), 8, secret).unwrap();
    let rows =
        f.db.rows(
            "SELECT data FROM events WHERE kind='tool.completed' AND task=? ORDER BY seq DESC",
            &[&f.oid],
        )
        .unwrap();
    let event: Value = serde_json::from_str(rows[0]["data"].as_str().unwrap()).unwrap();
    assert!(
        event["arguments"].is_null()
            && event["result"].is_null()
            && event["result_summary"].is_null()
    );
}
