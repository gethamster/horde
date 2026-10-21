//! Synthetic repositories and loopback services only; never operational qualification.
use horde::{
    config::{Decision, DecisionMode, Settings},
    decision::review,
    store::Store,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
#[allow(dead_code)]
#[path = "support/maintenance.rs"]
mod maintenance;
#[allow(dead_code)]
#[path = "support/decisions.rs"]
mod support;
const REVIEW: &str = r#"{"model":"jev-1.13.0","answers":{"requirements":{"type":"noul","noul":0.0,"confidence":0.99},"correctness":{"type":"noul","noul":0.0,"confidence":0.99},"security":{"type":"noul","noul":0.0,"confidence":0.99},"tests":{"type":"noul","noul":0.0,"confidence":0.99},"integration":{"type":"noul","noul":0.0,"confidence":0.99},"insufficient_evidence":{"type":"noul","noul":0.0,"confidence":0.99},"specialist":{"type":"choice","choice":"none","confidence":0.99,"probabilities":{"none":0.99,"correctness":0.002,"security":0.002,"testing":0.002,"operations":0.002,"abstain":0.002}}}}"#;
const VETO: &str = r#"{"model":"jev-1.13.0","answers":{"delivery":{"type":"choice","choice":"routine","confidence":0.99,"probabilities":{"routine":0.99,"escalate":0.01}}}}"#;
fn hash(value: &impl serde::Serialize) -> String {
    horde::store::hash(&serde_json::to_vec(value).unwrap())
}
struct Fixture {
    _dir: tempfile::TempDir,
    _operator: support::OperatorConfig,
    db: Store,
    task: String,
    args: Value,
    workspace: std::path::PathBuf,
    decision: Decision,
    authority_active: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl Fixture {
    async fn new(url: String, calls: u64) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        for argv in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@localhost"],
        ] {
            horde::git::run(&repo, &argv).unwrap();
        }
        std::fs::write(repo.join("src/fixture.rs"), "fn value() -> usize { 1 }\n").unwrap();
        horde::git::run(&repo, &["add", "."]).unwrap();
        horde::git::run(&repo, &["commit", "-m", "base"]).unwrap();
        let base = horde::git::run(&repo, &["rev-parse", "HEAD"]).unwrap();
        horde::git::run(&repo, &["remote", "add", "origin", repo.to_str().unwrap()]).unwrap();
        let decision = Decision {
            mode: DecisionMode::Shadow,
            review_enabled: true,
            base_url: url,
            api_key_env: "HORDE_QUALIFICATION_LOOPBACK_KEY".into(),
            deadline_ms: 1000,
            ..Default::default()
        };
        unsafe {
            std::env::set_var(&decision.api_key_env, "qualification-loopback-key");
        }
        let operator = support::OperatorConfig::install(&decision);
        let db = Store::open(&dir.path().join("state")).unwrap();
        let mut settings = Settings {
            decision: decision.clone(),
            ..Default::default()
        };
        settings.delivery.repository = "gethamster/adam".into();
        settings.delivery.base = "main".into();
        let objective = "Observe committed fixture";
        let plan = horde::template::compile(
            "simulated",
            &horde::template::load_templates(&repo).unwrap(),
            BTreeMap::from([("task".into(), objective.into())]),
        )
        .unwrap();
        let task = db.submit(objective, &repo, &settings, &plan).unwrap();
        // Model a real preallocated candidate whose first attempt follows freeze.
        db.conn
            .execute(
                "UPDATE tasks SET created=? WHERE id=?",
                rusqlite::params![horde::store::now() - 120, task],
            )
            .unwrap();
        let workspace = horde::git::task_workspace(&db, &task).unwrap();
        std::fs::write(
            workspace.join("src/fixture.rs"),
            "fn value() -> usize { 2 }\n",
        )
        .unwrap();
        horde::git::run(&workspace, &["add", "."]).unwrap();
        horde::git::run(&workspace, &["commit", "-m", "patch"]).unwrap();
        let head = horde::git::run(&workspace, &["rev-parse", "HEAD"]).unwrap();
        let authority_active =
            maintenance::bind_controlled(&db, &task, &repo, calls, 1000000, 1000000)
                .await
                .unwrap();
        db.event(&task,"run.checkpoint_verified",json!({"commit_sha":head,"expected_main_head":base,"validation":["true"],"validation_id":"fixture-checkpoint"})).unwrap();
        let spec = json!({"id":"reviewer","kind":"agent","role":"reviewer","output_types":{"reviewed_head":"string","reviewed_revision":"integer","verdict":"string","risk":"string","coverage_complete":"boolean","findings":"array"}});
        db.conn.execute("INSERT INTO steps(id,task,name,spec,state) VALUES('review-step',?,'independent-review',?,'succeeded')",rusqlite::params![task,spec.to_string()]).unwrap();
        db.conn.execute("INSERT INTO workers(id,task,step,status,token_hash,updated) VALUES('review-worker',?,'review-step','idle','fixture-token',?)",rusqlite::params![task,horde::store::now()]).unwrap();
        let result = json!({"accepted":true,"verdict":"pass","risk":"routine","coverage_complete":true,"findings":[],"reviewed_head":head,"reviewed_revision":1});
        db.conn.execute("INSERT INTO attempts(id,step,worker,state,started,finished,result) VALUES('independent-attempt','review-step','review-worker','succeeded',?,?,?)",rusqlite::params![horde::store::now(),horde::store::now(),result.to_string()]).unwrap();
        let github = json!({"repository":"gethamster/adam","base_oid":base,"observed_at":horde::store::now(),"pull_request":{"number":1,"state":"OPEN","headRefOid":head,"baseRefName":"main","author":{"login":"author"}},"check_runs":{"total_count":1,"check_runs":[{"id":1,"name":"test","head_sha":head,"status":"completed","conclusion":"success","completed_at":"2026-10-21T00:00:00Z"}]},"reviews":[{"id":2,"user":{"login":"reviewer"},"state":"APPROVED","commit_id":head,"submitted_at":"2026-10-21T00:00:00Z"}]});
        db.artifact(
            &task,
            None,
            "qualification-github-receipts",
            &serde_json::to_vec(&github).unwrap(),
            &json!({}),
            true,
        )
        .unwrap();
        db.conn
            .execute(
                "UPDATE review_scan_cursor SET seq=(SELECT MAX(seq) FROM events)",
                [],
            )
            .unwrap();
        db.event(&task,"step.finished",json!({"step":"review-step","state":"succeeded","result":{"integration":{"integrated_head":head}},"revision":1,"context_version":1})).unwrap();
        let mut queue = review::Queue::default();
        queue.scan(&db).unwrap();
        for _ in 0..200 {
            queue.tick(&db).await.unwrap();
            if review::list(&db, &task, 0, 200)
                .unwrap()
                .iter()
                .any(|row| row["state"] == "succeeded")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let rows = review::list(&db, &task, 0, 200).unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["state"], "succeeded", "{rows:?}");
        let policy = json!({"enabled":true,"gh_program":"/usr/bin/gh","repositories":["gethamster/adam"],"bases":["main"],"allowed_paths":["src"],"required_checks":["test"],"minimum_approvals":1,"require_independent_review":true,"qualification_id":"mock-corpus","minimum_routine_probability":0.8,"minimum_review_confidence":0.8,"package_release":{"repository":"gethamster/adam","workflow":"release.yml"}});
        let policy = serde_json::to_value(
            serde_json::from_value::<horde::config::AutomaticDelivery>(policy).unwrap(),
        )
        .unwrap();
        let cases:Vec<_>=(0..50).map(|i|json!({"id":format!("case-{i}"),"repository":"gethamster/adam","base_commit":base,"objective":if i==1 {objective.to_owned()}else{format!("Fixture {i}")},"inputs_sha256":format!("{i:064x}"),"verification_argv_sha256":hash(&json!(["true"])),"baseline_acceptance_question_sha256":horde::store::hash(b"Accept or escalate."),"critical":i==0,"baseline_task":format!("baseline-{i}"),"candidate_task":if i==1 {task.clone()}else{format!("candidate-{i}")}})).collect();
        let manifest = json!({"schema_version":1,"qualification_id":"mock-corpus","spend_reference":"shared-decision-budget","oracle_id":"independent-mock-oracle","oracle_spec_sha256":"a".repeat(64),"decision":decision,"delivery_policy":policy,"cases":cases});
        let frozen_at = horde::store::now() - 60;
        let frozen = json!({"manifest_hash":hash(&json!({"manifest":manifest,"frozen_at":frozen_at})),"manifest":manifest,"frozen_at":frozen_at});
        let args = json!({"frozen":frozen,"case_id":"case-1","task":task,"expected_head":head,"request_id":"observe-1","project":"default"});
        Self {
            _dir: dir,
            _operator: operator,
            db,
            task,
            args,
            workspace,
            decision,
            authority_active,
        }
    }
    async fn call(&self, args: Value) -> anyhow::Result<Value> {
        let root = self.db.root.clone();
        tokio::task::spawn_blocking(move || {
            horde::protocol::dispatch(
                &Store::open(&root)?,
                "delivery_qualification_observe",
                args,
                None,
            )
        })
        .await?
    }
}

#[tokio::test(flavor = "current_thread")]
async fn routed_observation_retains_real_wire_and_replays_without_another_call() {
    let _lock = support::CONFIG_LOCK.lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (url, mut bodies) =
                support::server(vec![("200 OK", REVIEW, 0), ("200 OK", VETO, 0)]).await;
            let f = Fixture::new(url, 3).await;
            bodies.recv().await.unwrap();
            let before = review::list(&f.db, &f.task, 0, 200).unwrap();
            let first = f.call(f.args.clone()).await.unwrap();
            assert_eq!(first["state"], "succeeded", "{first}");
            let body: Value = serde_json::from_slice(&bodies.recv().await.unwrap()).unwrap();
            assert_eq!(
                body["questions"]["delivery"]["criteria"],
                json!({"routine":null, "escalate":null})
            );
            assert_eq!(body["state"]["head"], f.args["expected_head"]);
            assert_eq!(f.call(f.args.clone()).await.unwrap(), first);
            assert!(bodies.try_recv().is_err());
            assert_eq!(
                review::list(&f.db, &f.task, 0, 200).unwrap()[0]["fresh"],
                before[0]["fresh"]
            );
            let usage = horde::upstream_maintenance::inspection(&f.db, &f.task).unwrap();
            assert_eq!(usage["usage"]["model_calls"], 2);
            assert_eq!(usage["usage"]["output_bytes"], REVIEW.len() + VETO.len());
            let links =
                f.db.rows(
                    "SELECT name,hash FROM artifact_links WHERE task=?",
                    &[&f.task],
                )
                .unwrap();
            let link = links
                .iter()
                .find(|r| r["name"] == "qualification-decision")
                .unwrap();
            let observed: Value = serde_json::from_slice(
                &std::fs::read(
                    f.db.root
                        .join("artifacts")
                        .join(link["hash"].as_str().unwrap()),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(
                observed["response"],
                serde_json::from_str::<Value>(VETO).unwrap()
            );
            assert_eq!(
                f.db.rows("SELECT * FROM delivery_qualifications", &[])
                    .unwrap()
                    .len(),
                0
            );
            let mut changed = f.args.clone();
            changed["request_id"] = json!("observe-2");
            assert!(f.call(changed).await.is_err());
            let mut changed = f.args.clone();
            changed["expected_head"] = json!("f".repeat(40));
            assert!(f.call(changed).await.is_err());
            assert!(!horde::protocol::worker_allowed(
                "delivery_qualification_observe"
            ));
            assert!(!horde::protocol::project_allowed(
                "delivery_qualification_observe"
            ));
            assert!(
                horde::protocol::dispatch_scoped(
                    &f.db,
                    "delivery_qualification_observe",
                    f.args.clone(),
                    None,
                    Some("default")
                )
                .is_err()
            );
            f.db.conn
                .execute("UPDATE tasks SET status='succeeded' WHERE id=?", [&f.task])
                .unwrap();
            assert_eq!(f.call(f.args.clone()).await.unwrap(), first);
            assert!(bodies.try_recv().is_err());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_send_remains_held_without_resend_or_new_identity() {
    let _lock = support::CONFIG_LOCK.lock().await;
    tokio::task::LocalSet::new().run_until(async {
        let (url,mut bodies)=support::server(vec![("200 OK",REVIEW,0),("200 OK",VETO,1500),("200 OK",VETO,0)]).await;
        let f=Fixture::new(url,4).await; bodies.recv().await.unwrap();
        assert_eq!(f.call(f.args.clone()).await.unwrap()["state"],"held"); bodies.recv().await.unwrap();
        assert_eq!(f.call(f.args.clone()).await.unwrap()["state"],"held"); assert!(bodies.try_recv().is_err());
        let mut changed=f.args.clone();changed["request_id"]=json!("replacement"); assert!(f.call(changed).await.is_err());
        assert_eq!(horde::upstream_maintenance::inspection(&f.db,&f.task).unwrap()["usage"]["model_calls"],2);
        assert!(f.db.rows("SELECT hash FROM artifact_links WHERE task=? AND name='qualification-decision'",&[&f.task]).unwrap().is_empty());
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_revocation_budget_and_changed_config_prevent_dispatch() {
    let _lock = support::CONFIG_LOCK.lock().await;
    tokio::task::LocalSet::new().run_until(async {
        for veto in ["cancel","revoke","budget","config","head","context","succeeded","failed"] {
            let (url,mut bodies)=support::server(vec![("200 OK",REVIEW,0),("200 OK",VETO,0)]).await;
            let f=Fixture::new(url,if veto=="budget" {1}else{4}).await; bodies.recv().await.unwrap();
            match veto {
                "cancel"=>{ f.db.conn.execute("UPDATE tasks SET status='cancelled' WHERE id=?",[&f.task]).unwrap(); },
                "revoke"=>{horde::upstream_maintenance::dispatch(&f.db,"maintenance_policy_revoke",&json!({"project":"default","policy_id":"decision-maintenance","policy_revision":1})).unwrap();},
                "config"=>{let mut changed=f.decision.clone();changed.mode=DecisionMode::Disabled;f._operator.rewrite(&changed);},
                "head"=>{horde::git::run(&f.workspace,&["commit","--allow-empty","-m","changed"]).unwrap();},
                "context"=>{f.db.conn.execute("UPDATE task_tree SET version=version+1 WHERE task=?",[&f.task]).unwrap();},
                "succeeded"|"failed"=>{f.db.conn.execute("UPDATE tasks SET status=? WHERE id=?",rusqlite::params![veto,f.task]).unwrap();},
                _=>{},
            }
            let result=f.call(f.args.clone()).await;
            if veto=="budget" {assert_eq!(result.unwrap()["state"],"held");}else{assert!(result.is_err(),"{veto}: {result:?}");}
            assert!(bodies.try_recv().is_err(),"{veto} sent HTTP");
            assert_eq!(horde::upstream_maintenance::inspection(&f.db,&f.task).unwrap()["usage"]["model_calls"],1);
        }
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn every_retry_rechecks_revocation_cancellation_head_config_and_the_same_budget() {
    let _lock = support::CONFIG_LOCK.lock().await;
    tokio::task::LocalSet::new().run_until(async {
        for change in ["cancel", "revoke", "head", "config", "budget"] {
            let (url, mut bodies) = support::server(vec![("200 OK", REVIEW, 0), ("429 Too Many Requests", "{}", 250), ("200 OK", VETO, 0)]).await;
            let f = Fixture::new(url, if change == "budget" {2} else {4}).await;
            bodies.recv().await.unwrap();
            let root = f.db.root.clone();
            let args = f.args.clone();
            let call = tokio::task::spawn_blocking(move || horde::protocol::dispatch(&Store::open(&root)?, "delivery_qualification_observe", args, None));
            bodies.recv().await.unwrap();
            match change {
                "cancel" => { f.db.conn.execute("UPDATE tasks SET status='cancelled' WHERE id=?", [&f.task]).unwrap(); },
                "revoke" => { horde::upstream_maintenance::dispatch(&f.db, "maintenance_policy_revoke", &json!({"project":"default","policy_id":"decision-maintenance","policy_revision":1})).unwrap(); },
                "head" => { horde::git::run(&f.workspace, &["commit", "--allow-empty", "-m", "changed during send"]).unwrap(); },
                "config" => { let mut changed = f.decision.clone(); changed.mode = DecisionMode::Disabled; f._operator.rewrite(&changed); },
                _ => {},
            }
            assert_eq!(call.await.unwrap().unwrap()["state"], "held", "{change}");
            assert!(bodies.try_recv().is_err(), "{change} allowed another HTTP request");
            assert_eq!(horde::upstream_maintenance::inspection(&f.db, &f.task).unwrap()["usage"]["model_calls"], 2);
            assert!(f.db.rows("SELECT hash FROM artifact_links WHERE task=? AND name='qualification-decision'", &[&f.task]).unwrap().is_empty());
        }
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn remote_revocation_during_response_holds_the_observation_and_keeps_usage() {
    let _lock = support::CONFIG_LOCK.lock().await;
    tokio::task::LocalSet::new().run_until(async {
        let (url, mut bodies) = support::server(vec![("200 OK", REVIEW, 0), ("200 OK", VETO, 250)]).await;
        let f = Fixture::new(url, 4).await;
        bodies.recv().await.unwrap();
        let root = f.db.root.clone();
        let args = f.args.clone();
        let call = tokio::task::spawn_blocking(move || horde::protocol::dispatch(&Store::open(&root)?, "delivery_qualification_observe", args, None));
        bodies.recv().await.unwrap();
        f.authority_active.store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(call.await.unwrap().unwrap()["state"], "held");
        assert_eq!(horde::upstream_maintenance::inspection(&f.db,&f.task).unwrap()["usage"]["model_calls"], 2);
        assert!(f.db.rows("SELECT hash FROM artifact_links WHERE task=? AND name='qualification-decision'", &[&f.task]).unwrap().is_empty());
        assert_eq!(f.call(f.args.clone()).await.unwrap()["state"], "held");
        assert!(bodies.try_recv().is_err());
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn unusable_or_lost_wire_responses_never_become_verified_observations() {
    let _lock = support::CONFIG_LOCK.lock().await;
    tokio::task::LocalSet::new().run_until(async {
        const ECHO: &str = r#"{"model":"jev-1.13.0","answers":{"delivery":{"type":"choice","choice":"routine","confidence":0.99,"probabilities":{"routine":0.99,"escalate":0.01}}},"debug":"qualification-loopback-key"}"#;
        for (status, body) in [("201 Created", VETO), ("200 OK", ECHO), ("DROP", "")] {
            let (url, mut bodies) = support::server(vec![("200 OK", REVIEW, 0), (status, body, 0), ("200 OK", VETO, 0)]).await;
            let f = Fixture::new(url, 4).await;
            bodies.recv().await.unwrap();
            let denied = horde::protocol::dispatch(&f.db, "delivery_qualification_observe", f.args.clone(), Some("fixture-worker-token")).unwrap_err();
            assert!(denied.to_string().contains("unavailable to worker"));
            assert!(bodies.try_recv().is_err());
            assert_eq!(f.call(f.args.clone()).await.unwrap()["state"], "held", "{status}");
            bodies.recv().await.unwrap();
            assert_eq!(f.call(f.args.clone()).await.unwrap()["state"], "held");
            assert!(bodies.try_recv().is_err(), "{status} caused a resend");
            assert_eq!(horde::upstream_maintenance::inspection(&f.db, &f.task).unwrap()["usage"]["model_calls"], 2);
            assert!(f.db.rows("SELECT hash FROM artifact_links WHERE task=? AND name='qualification-decision'", &[&f.task]).unwrap().is_empty());
        }
    }).await;
}
