use horde::{
    config::{Decision, DecisionMode, Settings},
    decision::{review, store as decision_store},
    store::Store,
};
use serde_json::json;
use std::{collections::BTreeMap, process::Command};
#[path = "support/decisions.rs"]
#[allow(dead_code)]
mod support;
use support::{CONFIG_LOCK, OperatorConfig, server};

const REVIEW_RESPONSE: &str = r#"{"model":"jev-1.13.0","answers":{"requirements":{"type":"noul","noul":0.1},"correctness":{"type":"noul","noul":0.1},"security":{"type":"noul","noul":0.1},"tests":{"type":"noul","noul":0.1},"integration":{"type":"noul","noul":0.1},"insufficient_evidence":{"type":"noul","noul":0.1},"specialist":{"type":"choice","choice":"none","confidence":0.6,"probabilities":{"none":0.6,"correctness":0.1,"security":0.1,"testing":0.1,"operations":0.05,"abstain":0.05}}},"usage":{"input_tokens":42,"output_tokens":12}}"#;

fn fixture(decision: &Decision) -> (tempfile::TempDir, tempfile::TempDir, Store, String) {
    let data = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let db = Store::open(data.path()).unwrap();
    let settings = Settings {
        decision: decision.clone(),
        ..Settings::default()
    };
    let plan = horde::template::compile(
        "simulated",
        &horde::template::load_templates(repo.path()).unwrap(),
        BTreeMap::from([("task".into(), "review this plan".into())]),
    )
    .unwrap();
    let task = db
        .submit("review this plan", repo.path(), &settings, &plan)
        .unwrap();
    (data, repo, db, task)
}

async fn drain(queue: &mut review::Queue, db: &Store, task: &str) {
    for _ in 0..100 {
        queue.tick(db).await.unwrap();
        if review::list(db, task, 0, 50)
            .unwrap()
            .iter()
            .all(|row| row["state"] != "queued" && row["state"] != "running")
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("review queue did not finish");
}

#[tokio::test(flavor = "current_thread")]
async fn plan_review_uses_one_batched_request_and_becomes_stale_on_revision() {
    let _lock = CONFIG_LOCK.lock().await;
    tokio::task::LocalSet::new().run_until(async {
        let (base_url, mut bodies) = server(vec![("200 OK", REVIEW_RESPONSE, 0)]).await;
        let key = format!("HORDE_REVIEW_TEST_KEY_{}", std::process::id());
        unsafe { std::env::set_var(&key, "review-test-secret") };
        let decision = Decision { mode: DecisionMode::Shadow, review_enabled: true, base_url, api_key_env: key.clone(), ..Decision::default() };
        let _operator = OperatorConfig::install(&decision);
        let (_data, _repo, db, task) = fixture(&decision);
        let mut queue = review::Queue::default();
        queue.scan(&db).unwrap();
        queue.scan(&db).unwrap();
        assert_eq!(review::list(&db,&task,0,50).unwrap().len(),1);
        drain(&mut queue,&db,&task).await;
        let wire: serde_json::Value = serde_json::from_slice(&bodies.recv().await.unwrap()).unwrap();
        assert_eq!(wire["questions"].as_object().unwrap().len(),7);
        assert_eq!(wire["state"]["catalog"],"review-v1");
        assert_eq!(wire["state"]["coverage"]["complete"],true);
        assert!(!wire.to_string().contains("review-test-secret"));
        let first=review::list(&db,&task,0,50).unwrap().remove(0);
        assert_eq!(first["kind"],"plan");
        assert_eq!(first["state"],"succeeded");
        assert_eq!(first["proposed"],"none");
        assert_eq!(first["fresh"],true);
        assert!(first.get("independent_generative_review").is_none());
        assert_eq!(first["generative_review_attempts"],json!([]));
        db.conn.execute("INSERT INTO revisions(task,revision,plan,created) SELECT task,2,plan,created FROM revisions WHERE task=? AND revision=1",[&task]).unwrap();
        assert_eq!(review::list(&db,&task,0,50).unwrap()[0]["fresh"],false);
        assert_eq!(decision_store::list(&db,&task,0,50).unwrap().len(),1);
        unsafe { std::env::remove_var(key) };
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn missing_workspace_abstains_and_restart_does_not_duplicate_checkpoint() {
    let _lock = CONFIG_LOCK.lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut bodies) = server(vec![("200 OK", REVIEW_RESPONSE, 0)]).await;
            let key = format!("HORDE_REVIEW_ABSTAIN_KEY_{}", std::process::id());
            unsafe { std::env::set_var(&key, "review-test-secret") };
            let decision = Decision {
                mode: DecisionMode::Shadow,
                review_enabled: true,
                base_url,
                api_key_env: key.clone(),
                ..Decision::default()
            };
            let _operator = OperatorConfig::install(&decision);
            let (data, _repo, db, task) = fixture(&decision);
            db.event(
                &task,
                "integration.conflict",
                json!({"before":"missing","error":"conflict","revision":1}),
            )
            .unwrap();
            let mut queue = review::Queue::default();
            queue.scan(&db).unwrap();
            let before = review::list(&db, &task, 0, 50).unwrap();
            assert_eq!(before.len(), 2);
            let pending = before
                .iter()
                .find(|row| row["kind"] == "integration_failure")
                .unwrap();
            assert_eq!(pending["state"], "queued");
            drain(&mut queue, &db, &task).await;
            let failure = review::list(&db, &task, 0, 50)
                .unwrap()
                .into_iter()
                .find(|row| row["kind"] == "integration_failure")
                .unwrap();
            assert_eq!(failure["state"], "succeeded");
            assert_eq!(failure["abstention"], 1);
            assert_eq!(failure["coverage"]["reason"], "workspace_unavailable");
            assert_eq!(failure["provider_attempts"], 0);
            assert!(bodies.recv().await.is_some());
            let reopened = Store::open(data.path()).unwrap();
            let mut after_restart = review::Queue::default();
            after_restart.scan(&reopened).unwrap();
            assert_eq!(review::list(&reopened, &task, 0, 50).unwrap().len(), 2);
            let failed = review::list(&reopened, &task, 0, 50)
                .unwrap()
                .into_iter()
                .find(|row| row["kind"] == "integration_failure")
                .unwrap();
            assert_eq!(failed["state"], "succeeded");
            assert!(!matches!(
                tokio::time::timeout(std::time::Duration::from_millis(100), bodies.recv()).await,
                Ok(Some(_))
            ));
            unsafe { std::env::remove_var(key) };
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn patch_manifest_is_complete_and_worktree_edits_stale_the_review() {
    let _lock = CONFIG_LOCK.lock().await;
    tokio::task::LocalSet::new().run_until(async {
        let (base_url, mut bodies) = server(vec![("200 OK", REVIEW_RESPONSE, 0)]).await;
        let key = format!("HORDE_REVIEW_PATCH_KEY_{}", std::process::id());
        unsafe { std::env::set_var(&key, "review-test-secret") };
        let decision = Decision { mode: DecisionMode::Shadow, review_enabled: true, base_url, api_key_env: key.clone(), ..Decision::default() };
        let _operator = OperatorConfig::install(&decision);
        let (data, _repo, db, task) = fixture(&decision);
        let path=data.path().join("workspaces").join(&task).join("integrated");
        std::fs::create_dir_all(&path).unwrap();
        let git=|args: &[&str]| { assert!(Command::new("git").current_dir(&path).args(args).status().unwrap().success()); };
        let head=|| String::from_utf8(Command::new("git").current_dir(&path).args(["rev-parse","HEAD"]).output().unwrap().stdout).unwrap().trim().to_owned();
        git(&["init","-q"]); git(&["config","user.name","Reviewer"]); git(&["config","user.email","reviewer@example.test"]);
        std::fs::write(path.join("src.txt"),"base\n").unwrap(); git(&["add","."]); git(&["commit","-qm","base"]);
        let base=head();
        std::fs::write(path.join("src.txt"),"updated\n").unwrap(); git(&["add","."]); git(&["commit","-qm","patch"]);
        let integrated_head=head();
        db.conn.execute("INSERT INTO workspace_bases(task,start,source,fetched,created) VALUES(?,?,?,0,0)",rusqlite::params![task,base,"test"]).unwrap();
        let step=horde::store::id();
        db.conn.execute("INSERT INTO steps(id,task,name,spec,state) VALUES(?,?,?,?,?)",rusqlite::params![step,task,"implementation",r#"{"id":"implementation","kind":"agent"}"#,"succeeded"]).unwrap();
        db.event(&task,"step.finished",json!({"step":step,"state":"succeeded","result":{"integration":{"integrated_head":integrated_head}},"revision":1})).unwrap();
        let mut queue=review::Queue::default(); queue.scan(&db).unwrap(); drain(&mut queue,&db,&task).await;
        let patch_request:serde_json::Value=serde_json::from_slice(&bodies.recv().await.unwrap()).unwrap();
        assert_eq!(patch_request["state"]["checkpoint"]["kind"],"patch");
        assert_eq!(patch_request["state"]["manifest"]["path_count"],1);
        assert_eq!(patch_request["state"]["manifest"]["paths"][0]["path"],"src.txt");
        let patch=review::list(&db,&task,0,50).unwrap().into_iter().find(|row|row["kind"]=="patch").unwrap();
        assert_eq!(patch["coverage"]["complete"],true);
        assert_eq!(patch["fresh"],true);
        db.event(&task,"step.finished",json!({"step":step,"state":"succeeded","result":{"integration":{"integrated_head":"different-head"}},"revision":1})).unwrap();
        queue.scan(&db).unwrap(); drain(&mut queue,&db,&task).await;
        let changed=review::list(&db,&task,0,50).unwrap().into_iter().filter(|row|row["kind"]=="patch").max_by_key(|row|row["event_seq"].as_i64().unwrap()).unwrap();
        assert_eq!(changed["coverage"]["reason"],"checkpoint_head_changed");
        assert_eq!(changed["abstention"],1);
        assert_eq!(changed["provider_attempts"],0);
        let verify_step=horde::store::id();
        db.conn.execute("INSERT INTO steps(id,task,name,spec,state) VALUES(?,?,?,?,?)",rusqlite::params![verify_step,task,"verification",r#"{"id":"verification","kind":"command"}"#,"succeeded"]).unwrap();
        db.event(&task,"step.finished",json!({"step":verify_step,"state":"succeeded","integrated_head":base,"result":{"tests":"passed"},"revision":1})).unwrap();
        queue.scan(&db).unwrap(); drain(&mut queue,&db,&task).await;
        let verify=review::list(&db,&task,0,50).unwrap().into_iter().find(|row|row["kind"]=="verification").unwrap();
        assert_eq!(verify["coverage"]["reason"],"checkpoint_head_changed");
        assert_eq!(verify["provider_attempts"],0);
        std::fs::write(path.join("src.txt"),"uncommitted edit\n").unwrap();
        let stale=review::list(&db,&task,0,50).unwrap().into_iter().find(|row|row["kind"]=="patch").unwrap();
        assert_eq!(stale["fresh"],false);
        std::fs::write(path.join("src.txt"),"updated\n").unwrap();
        std::fs::write(path.join("binary.bin"),b"changed\0binary").unwrap();
        git(&["add","."]); git(&["commit","-qm","binary"]);
        db.event(&task,"integration.succeeded",json!({"integrated_head":head(),"commit":head(),"revision":1})).unwrap();
        queue.scan(&db).unwrap(); drain(&mut queue,&db,&task).await;
        let binary=review::list(&db,&task,0,50).unwrap().into_iter().find(|row|row["kind"]=="integration").unwrap();
        assert_eq!(binary["coverage"]["reason"],"diff_content_incomplete");
        assert!(binary["coverage"]["uncovered_paths"].as_array().unwrap().iter().any(|row|row["path"]=="binary.bin" && row["reason"]=="binary_path"));
        assert_eq!(binary["provider_attempts"],0);
        unsafe { std::env::remove_var(key) };
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_evidence_abstains_and_task_limit_is_inspectable() {
    let _lock = CONFIG_LOCK.lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let key = format!("HORDE_REVIEW_LIMIT_KEY_{}", std::process::id());
            unsafe { std::env::set_var(&key, "review-test-secret") };
            let decision = Decision {
                mode: DecisionMode::Shadow,
                review_enabled: true,
                backend: "typesafe".into(),
                base_url: "https://api.typesafe.ai".into(),
                api_key_env: key.clone(),
                max_decisions_per_task: 1,
                ..Decision::default()
            };
            let _operator = OperatorConfig::install(&decision);
            let (_data, _repo, db, task) = fixture(&decision);
            db.conn
                .execute(
                    "UPDATE tasks SET objective=? WHERE id=?",
                    rusqlite::params!["long evidence ".repeat(4000), task],
                )
                .unwrap();
            let mut queue = review::Queue::default();
            queue.scan(&db).unwrap();
            drain(&mut queue, &db, &task).await;
            let first = review::list(&db, &task, 0, 50).unwrap().remove(0);
            assert_eq!(first["coverage"]["reason"], "request_limit");
            assert_eq!(first["abstention"], 1);
            assert_eq!(first["provider_attempts"], 0);
            db.event(
                &task,
                "integration.conflict",
                json!({"before":"missing","revision":1}),
            )
            .unwrap();
            queue.scan(&db).unwrap();
            let rows = review::list(&db, &task, 0, 50).unwrap();
            let limited = rows
                .iter()
                .find(|row| row["kind"] == "integration_failure")
                .unwrap();
            assert_eq!(limited["state"], "skipped");
            assert_eq!(limited["error"], "task_decision_limit");
            unsafe { std::env::remove_var(key) };
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_task_does_not_call_provider() {
    let _lock = CONFIG_LOCK.lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let key = format!("HORDE_REVIEW_CANCEL_KEY_{}", std::process::id());
            unsafe { std::env::set_var(&key, "review-test-secret") };
            let decision = Decision {
                mode: DecisionMode::Shadow,
                review_enabled: true,
                api_key_env: key.clone(),
                ..Decision::default()
            };
            let _operator = OperatorConfig::install(&decision);
            let (_data, _repo, db, task) = fixture(&decision);
            let mut queue = review::Queue::default();
            queue.scan(&db).unwrap();
            db.conn
                .execute("UPDATE tasks SET status='cancelled' WHERE id=?", [&task])
                .unwrap();
            queue.tick(&db).await.unwrap();
            let result = review::list(&db, &task, 0, 50).unwrap().remove(0);
            assert_eq!(result["state"], "cancelled");
            assert_eq!(result["provider_attempts"], 0);
            unsafe { std::env::remove_var(key) };
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn obsolete_plan_and_oversized_final_evidence_abstain() {
    let _lock = CONFIG_LOCK.lock().await;
    tokio::task::LocalSet::new().run_until(async {
        let key = format!("HORDE_REVIEW_FINAL_KEY_{}", std::process::id());
        unsafe { std::env::set_var(&key, "review-test-secret") };
        let decision = Decision { mode: DecisionMode::Shadow, review_enabled: true, backend: "typesafe".into(), base_url: "https://api.typesafe.ai".into(), api_key_env: key.clone(), ..Decision::default() };
        let _operator = OperatorConfig::install(&decision);
        let (_data, _repo, db, task) = fixture(&decision);
        db.conn.execute("UPDATE review_scan_cursor SET seq=(SELECT MAX(seq) FROM events) WHERE id=1",[]).unwrap();
        db.event(&task,"integration.conflict",json!({"before":"missing","revision":1})).unwrap();
        let mut queue=review::Queue::default(); queue.scan(&db).unwrap();
        db.conn.execute("INSERT INTO revisions(task,revision,plan,created) SELECT task,2,plan,created FROM revisions WHERE task=? AND revision=1",[&task]).unwrap();
        db.event(&task,"workflow.revised",json!({"revision":1})).unwrap();
        db.event(&task,"integration.validation_failed",json!({"before":"missing","revision":2})).unwrap();
        queue.scan(&db).unwrap();
        db.conn.execute("UPDATE task_tree SET version=2 WHERE task=?",[&task]).unwrap();
        db.conn.execute("UPDATE steps SET result=? WHERE task=?",rusqlite::params![serde_json::to_string(&"large result ".repeat(2000)).unwrap(),task]).unwrap();
        db.event(&task,"task.finished",json!({"status":"succeeded","revision":2})).unwrap();
        queue.scan(&db).unwrap(); drain(&mut queue,&db,&task).await;
        let rows=review::list(&db,&task,0,50).unwrap();
        assert_eq!(rows.len(),4);
        let integration=rows.iter().find(|row|row["kind"]=="integration_failure" && row["coverage"]["reason"]=="checkpoint_revision_changed").unwrap();
        assert_eq!(integration["coverage"]["reason"],"checkpoint_revision_changed");
        let changed_context=rows.iter().find(|row|row["kind"]=="integration_failure" && row["coverage"]["reason"]=="checkpoint_context_changed").unwrap();
        assert_eq!(changed_context["provider_attempts"],0);
        let plan=rows.iter().find(|row|row["kind"]=="plan").unwrap();
        assert_eq!(plan["coverage"]["reason"],"checkpoint_revision_changed");
        let final_row=rows.iter().find(|row|row["kind"]=="final_evidence").unwrap();
        assert_eq!(final_row["coverage"]["reason"],"step_evidence_oversize");
        assert_eq!(final_row["provider_attempts"],0);
        unsafe { std::env::remove_var(key) };
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn escapable_secrets_in_nested_json_never_reach_provider_or_evidence_artifacts() {
    let _lock = CONFIG_LOCK.lock().await;
    tokio::task::LocalSet::new().run_until(async {
        let (base_url, mut bodies) = server(vec![("200 OK", REVIEW_RESPONSE, 0)]).await;
        let key = format!("HORDE_REVIEW_SECRET_KEY_{}", std::process::id());
        let secret = "credential\"with\\escapes";
        unsafe { std::env::set_var(&key, secret) };
        let decision = Decision { mode: DecisionMode::Shadow, review_enabled: true, base_url, api_key_env: key.clone(), ..Decision::default() };
        let _operator = OperatorConfig::install(&decision);
        let (data, _repo, db, task) = fixture(&decision);
        db.conn.execute("UPDATE tasks SET plan=? WHERE id=?",rusqlite::params![json!({"nested_secret":secret}).to_string(),task]).unwrap();
        db.conn.execute("UPDATE events SET data=? WHERE task=? AND kind='task.submitted'",rusqlite::params![json!({"objective":"review this plan","integrated_head":null,"revision":1,"context_version":1,"nested_json":json!({"secret":secret}).to_string()}).to_string(),task]).unwrap();
        db.conn.execute("INSERT INTO external_ops(task,name,state,data) VALUES(?,?,?,?)",rusqlite::params![task,"test","complete",json!({"secret":secret}).to_string()]).unwrap();
        db.conn.execute("UPDATE steps SET result=? WHERE task=?",rusqlite::params![json!({"secret":secret}).to_string(),task]).unwrap();
        db.event(&task,"task.finished",json!({"status":"succeeded","integrated_head":null,"revision":1})).unwrap();
        let mut queue=review::Queue::default(); queue.scan(&db).unwrap(); drain(&mut queue,&db,&task).await;
        let sent=String::from_utf8(tokio::time::timeout(std::time::Duration::from_secs(2),bodies.recv()).await.unwrap().unwrap()).unwrap();
        let escaped=serde_json::to_string(secret).unwrap();
        let escaped=&escaped[1..escaped.len()-1];
        assert!(!sent.contains(secret) && !sent.contains(escaped));
        let links=db.rows("SELECT hash FROM artifact_links WHERE task=? AND name LIKE 'review-evidence-%'", &[&task]).unwrap();
        assert_eq!(links.len(),2);
        for link in links {
            let raw=std::fs::read_to_string(data.path().join("artifacts").join(link["hash"].as_str().unwrap())).unwrap();
            assert!(!raw.contains(secret) && !raw.contains(escaped));
        }
        unsafe { std::env::remove_var(key) };
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn restart_replays_only_reviews_with_no_provider_attempt() {
    let _lock = CONFIG_LOCK.lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut bodies) = server(vec![("200 OK", REVIEW_RESPONSE, 0)]).await;
            let key = format!("HORDE_REVIEW_REPLAY_KEY_{}", std::process::id());
            unsafe { std::env::set_var(&key, "review-test-secret") };
            let decision = Decision {
                mode: DecisionMode::Shadow,
                review_enabled: true,
                base_url,
                api_key_env: key.clone(),
                ..Decision::default()
            };
            let _operator = OperatorConfig::install(&decision);
            let (data, _repo, db, task) = fixture(&decision);
            db.event(
                &task,
                "integration.conflict",
                json!({"before":"missing","integrated_head":null,"revision":1}),
            )
            .unwrap();
            let mut old_queue = review::Queue::default();
            old_queue.scan(&db).unwrap();
            let failure = review::list(&db, &task, 0, 50)
                .unwrap()
                .into_iter()
                .find(|row| row["kind"] == "integration_failure")
                .unwrap();
            db.conn
                .execute(
                    "UPDATE decisions SET state='running',started=1,provider_attempts=0 WHERE id=?",
                    [failure["id"].as_str().unwrap()],
                )
                .unwrap();
            decision_store::interrupt_running(&db.conn).unwrap();
            let reopened = Store::open(data.path()).unwrap();
            assert_eq!(review::resume_never_sent(&reopened).unwrap(), 1);
            let mut queue = review::Queue::default();
            drain(&mut queue, &reopened, &task).await;
            let rows = review::list(&reopened, &task, 0, 50).unwrap();
            assert_eq!(
                rows.iter().find(|row| row["kind"] == "plan").unwrap()["state"],
                "succeeded"
            );
            assert_eq!(
                rows.iter()
                    .find(|row| row["kind"] == "integration_failure")
                    .unwrap()["state"],
                "interrupted"
            );
            assert!(bodies.recv().await.is_some());
            unsafe { std::env::remove_var(key) };
        })
        .await;
}
