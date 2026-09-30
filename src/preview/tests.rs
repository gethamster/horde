use super::*;
use serde_json::json;
fn config() -> Value {
    json!({"schema_version":1,"scope":"local","projects":[{"project_id":"4799c098-902d-5c45-b4b2-c92b1e283c8d","project_slug":"hello","enabled":true,"publisher":"/usr/local/bin/system-worker","builder_image":format!("rust@sha256:{}","a".repeat(64)),"runtime_image":format!("debian@sha256:{}","b".repeat(64)),"dockerfile":"Dockerfile","validation":["cargo","test","--locked"],"review_step":"review","admission_url":"http://registry-admission:8092","admission_token_file":"/run/system/admission-token","estimated_publish_bytes":104857600,"timeout_seconds":1800}]})
}
#[test]
fn setup_is_disabled_until_operator_configures_it() {
    let d = tempfile::tempdir().unwrap();
    let db = Store::open(d.path()).unwrap();
    assert!(policy(&db, "default").unwrap().is_none());
}
#[test]
fn config_rejects_mutable_images_and_traversal() {
    for (key, value) in [
        ("builder_image", json!("rust:latest")),
        ("publisher", json!("../worker")),
        ("dockerfile", json!("../Dockerfile")),
        ("admission_url", json!("https://example.com")),
    ] {
        let mut c = config();
        c["projects"][0][key] = value;
        assert!(configure(&c).is_err(), "{key}");
    }
}
#[test]
fn recipe_hash_matches_worker_canonical_recipe() {
    let p = configure(&config()).unwrap().projects.remove(0);
    let canonical = format!(
        "{{\"builder_image\": {}, \"component\": null, \"dockerfile\": \"Dockerfile\", \"runtime_image\": {}}}",
        json!(p.builder_image),
        json!(p.runtime_image)
    );
    assert_eq!(p.recipe_hash(), crate::store::hash(canonical.as_bytes()));
}
#[test]
fn receipt_rejects_changed_provenance() {
    let p = configure(&config()).unwrap().projects.remove(0);
    let mut r = json!({"schema_version":1,"scope":"local","project_id":p.project_id,"project_slug":"hello","run_id":"run","built_commit":"head","tree_sha":"tree","recipe_hash":p.recipe_hash(),"component":p.component,"artifact_digest":format!("sha256:{}","c".repeat(64)),"image":format!("registry:5000/local/hello@sha256:{}","c".repeat(64))});
    assert!(verify_receipt(&p, "local", "run", "head", "tree", &r).is_ok());
    r["built_commit"] = json!("other");
    assert!(verify_receipt(&p, "local", "run", "head", "tree", &r).is_err());
}
#[test]
fn publisher_environment_omits_controller_secrets() {
    let p = configure(&config()).unwrap().projects.remove(0);
    let env = publisher_command(&p)
        .get_envs()
        .filter_map(|(k, v)| {
            v.map(|v| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert!(!env.contains_key("HORDE_RUN_ATTESTATION_KEY"));
    assert!(!env.contains_key("HORDE_SETUP_ADMIN_TOKEN_FILE"));
    assert_eq!(env["DOCKER_HOST"], "tcp://sandbox-docker:2375");
}
fn fixture() -> (tempfile::TempDir, Store, String, Value, Value) {
    let d = tempfile::tempdir().unwrap();
    let repo = d.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| crate::git::run(&repo, args).unwrap();
    git(&["init", "-b", "main"]);
    git(&["config", "user.name", "Fixture"]);
    git(&["config", "user.email", "fixture@example.invalid"]);
    std::fs::write(repo.join("Dockerfile"), "FROM scratch\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-m", "fixture"]);
    let remote = d.path().join("remote.git");
    git(&[
        "clone",
        "--bare",
        repo.to_str().unwrap(),
        remote.to_str().unwrap(),
    ]);
    git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    let db =
        Store::open_with_config_dir(&d.path().join("state"), &d.path().join("config")).unwrap();
    let project = config()["projects"][0]["project_id"]
        .as_str()
        .unwrap()
        .to_owned();
    db.conn
        .execute(
            "INSERT INTO projects VALUES(?,'hello','Hello',1,'native',1)",
            [&project],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO project_tenants VALUES(?,'local',1)",
            [&project],
        )
        .unwrap();
    let plan:crate::template::Plan=serde_json::from_value(json!({"steps":[{"id":"implement","kind":"agent","instructions":"implement","tools":["read_file"]},{"id":"review","kind":"agent","role":"reviewer","needs":["implement"],"instructions":"review current tree","tools":["read_file"]}],"pins":{},"outputs":{}})).unwrap();
    let mut settings = crate::config::Settings::default();
    settings.delivery.base = "main".into();
    let task = db
        .submit_project(&project, "fixture", &repo, &settings, &plan)
        .unwrap();
    crate::run::bind_run_context(&db, &task, Some("thread"), Some("brief")).unwrap();
    crate::git::task_workspace(&db, &task).unwrap();
    let mut c = config();
    c["projects"][0]["validation"] = json!(["true"]);
    setup(&db, &c).unwrap();
    let row = db
        .rows(
            "SELECT * FROM steps WHERE task=? AND name='review'",
            &[&task],
        )
        .unwrap()
        .remove(0);
    prepare_review(&db, &task, &row).unwrap();
    db.conn
        .execute(
            "UPDATE steps SET state='succeeded',result='{\"accepted\":true}' WHERE task=?",
            [&task],
        )
        .unwrap();
    let worker = db
        .register(&task, Some(row["id"].as_str().unwrap()))
        .unwrap();
    db.conn.execute("INSERT INTO attempts(id,step,worker,state,started,finished,result) VALUES('review-attempt',?,?,'succeeded',1,2,'{\"accepted\":true}')",params![row["id"].as_str(),worker["id"].as_str()]).unwrap();
    bind_attempt(&db, row["id"].as_str().unwrap(), "review-attempt").unwrap();
    db.conn
        .execute("UPDATE tasks SET status='succeeded' WHERE id=?", [&task])
        .unwrap();
    fixture_execution(&db, &task, &row);
    db.event(
        &task,
        "workflow.revised",
        json!({"reason":"explicit fixture work after activation"}),
    )
    .unwrap();
    (d, db, task, row, c)
}
#[test]
fn exact_agent_review_creates_one_durable_job_and_survives_reopen() {
    let (_d, db, task, _row, _c) = fixture();
    let first = pipeline::enqueue(&db, &task).unwrap().unwrap();
    assert!(pipeline::enqueue(&db, &task).unwrap().is_none());
    let root = db.root.clone();
    drop(db);
    let db = Store::open(&root).unwrap();
    assert_eq!(status(&db, &task).unwrap()["id"], first);
    assert_eq!(
        status(&db, &task).unwrap()["review_attempt_id"],
        "review-attempt"
    );
}
#[test]
fn feedback_head_schedules_actual_agent_review_in_same_run() {
    let (_d, db, task, _row, _c) = fixture();
    let path = crate::git::task_workspace(&db, &task).unwrap();
    crate::git::run(&path, &["commit", "--allow-empty", "-m", "feedback"]).unwrap();
    assert!(pipeline::enqueue(&db, &task).unwrap().is_none());
    assert_eq!(db.task(&task).unwrap()["status"], "running");
    let steps = db.steps(&task).unwrap();
    assert_eq!(steps.len(), 3);
    let step = Store::step(&steps[2]).unwrap();
    assert_eq!(step.kind, "agent");
    assert!(step.id.starts_with("review.preview-"));
}
#[test]
fn changed_main_invalidates_review_and_requires_new_agent_review() {
    let (_d, db, task, _row, _c) = fixture();
    let taskrow = db.task(&task).unwrap();
    let repo = Path::new(taskrow["repo"].as_str().unwrap());
    crate::git::run(repo, &["commit", "--allow-empty", "-m", "advance main"]).unwrap();
    crate::git::run(repo, &["push", "origin", "main"]).unwrap();
    assert!(pipeline::enqueue(&db, &task).unwrap().is_none());
    assert_eq!(db.steps(&task).unwrap().len(), 3);
    let r = db.steps(&task).unwrap().remove(2);
    prepare_review(&db, &task, &r).unwrap();
    let review = db
        .rows(
            "SELECT * FROM preview_reviews WHERE step=?",
            &[&r["id"].as_str()],
        )
        .unwrap();
    assert_eq!(
        review[0]["head"],
        crate::git::run(
            &crate::git::task_workspace(&db, &task).unwrap(),
            &["rev-parse", "HEAD"]
        )
        .unwrap()
    );
}
#[test]
fn command_review_or_unaccepted_agent_cannot_publish() {
    let (_d, db, task, row, _c) = fixture();
    db.conn
        .execute(
            "UPDATE steps SET result='{\"accepted\":false}' WHERE id=?",
            [row["id"].as_str()],
        )
        .unwrap();
    assert!(pipeline::enqueue(&db, &task).is_err());
    let mut invalid = row;
    invalid["spec"] = json!(
        serde_json::to_string(&json!({"id":"review","kind":"command","command":["true"]})).unwrap()
    );
    assert!(prepare_review(&db, &task, &invalid).is_err());
}
#[test]
fn lost_publication_checkpoint_and_push_responses_reconcile_one_job() {
    if std::env::var_os("HORDE_PREVIEW_FIXTURE_CHILD").is_none() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "preview::tests::lost_publication_checkpoint_and_push_responses_reconcile_one_job",
                "--nocapture",
            ])
            .env("HORDE_PREVIEW_FIXTURE_CHILD", "1")
            .env(
                "HORDE_RUN_ATTESTATION_KEY",
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [42; 32]),
            )
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let (d, db, task, _row, mut c) = fixture();
    let (head, tree) = pipeline::identity(&db, &task).unwrap();
    let p = configure(&c).unwrap().projects.remove(0);
    let receipt = json!({"schema_version":1,"scope":"local","project_id":p.project_id,"project_slug":"hello","run_id":task,"built_commit":head,"tree_sha":tree,"recipe_hash":p.recipe_hash(),"component":p.component,"artifact_digest":format!("sha256:{}","c".repeat(64)),"image":format!("registry:5000/local/hello@sha256:{}","c".repeat(64))});
    let helper = d.path().join("publisher");
    std::fs::write(&helper,format!("#!/bin/sh\n[ \"$(pwd)\" = \"{}\" ] || exit 1\nrequest=$(cat)\ncase \"$request\" in *'\"mode\":\"probe\"'*) printf '%s' '{{\"reused\":true}}';; *) printf '%s' '{}' ;; esac\n",crate::git::task_workspace(&db,&task).unwrap().canonicalize().unwrap().display(),receipt)).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    c["projects"][0]["publisher"] = json!(helper);
    setup(&db, &c).unwrap();
    let review = db
        .rows(
            "SELECT * FROM steps WHERE task=? AND name='review'",
            &[&task],
        )
        .unwrap()
        .remove(0);
    prepare_review(&db, &task, &review).unwrap();
    bind_attempt(&db, review["id"].as_str().unwrap(), "review-attempt").unwrap();
    fixture_execution(&db, &task, &review);
    let job = pipeline::enqueue(&db, &task).unwrap().unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(tokio::task::LocalSet::new().run_until(async {
        let mut queue = Queue::default();
        assert!(queue.tick(&db).await.unwrap());
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                queue.tick(&db).await.unwrap();
                let state = status(&db, &task).unwrap();
                assert_ne!(state["phase"], "held", "{state}");
                if state["phase"] == "succeeded" {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "preview reconciliation timed out: {}",
                status(&db, &task).unwrap()
            )
        });
        queue.shutdown(&db).await.unwrap();
        pipeline::advance(&db, &job).await.unwrap();
    }));

    c["projects"][0]["validation"] = json!(["false"]);
    setup(&db, &c).unwrap();
    prepare_review(&db, &task, &review).unwrap();
    bind_attempt(&db, review["id"].as_str().unwrap(), "review-attempt").unwrap();
    fixture_execution(&db, &task, &review);
    let invalid_job = pipeline::enqueue(&db, &task).unwrap().unwrap();
    std::fs::remove_file(&helper).unwrap();
    rt.block_on(async {
        let error = pipeline::advance(&db, &invalid_job).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("validation failed before publication")
        );
    });
    assert_eq!(status(&db, &task).unwrap()["phase"], "validating");
    db.conn
        .execute("DELETE FROM preview_jobs WHERE id=?", [invalid_job])
        .unwrap();
    let s = status(&db, &task).unwrap();

    assert_eq!(s["phase"], "succeeded");
    assert_eq!(s["checkpoint"]["artifact_digest"], receipt["image"]);
    assert_eq!(s["checkpoint"]["build_id"], job);
    assert_eq!(
        db.rows(
            "SELECT data FROM events WHERE task=? AND kind='run.checkpoint_verified'",
            &[&task]
        )
        .unwrap()
        .len(),
        1
    );
    assert_eq!(
        db.rows(
            "SELECT data FROM events WHERE task=? AND kind='run.branch_published'",
            &[&task]
        )
        .unwrap()
        .len(),
        1
    );
}
#[test]
fn held_retry_replays_same_job_and_rejects_new_identity() {
    let (_d, db, task, _row, _c) = fixture();
    let id = pipeline::enqueue(&db, &task).unwrap().unwrap();
    db.conn
        .execute("UPDATE preview_jobs SET phase='held' WHERE id=?", [&id])
        .unwrap();
    let head = status(&db, &task).unwrap()["commit_sha"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!crate::protocol::worker_allowed("run_preview_retry"));
    let a = retry(&db, &task, &head, "retry-1").unwrap();
    assert_eq!(retry(&db, &task, &head, "retry-1").unwrap(), a);
    assert!(retry(&db, &task, "different", "retry-1").is_err());
    assert_eq!(
        db.rows("SELECT id FROM preview_jobs WHERE task=?", &[&task])
            .unwrap()
            .len(),
        1
    );
    assert_eq!(status(&db, &task).unwrap()["phase"], "queued");
}
#[test]
fn active_publication_prevents_drain_quiescence() {
    let (_d, db, task, _row, _c) = fixture();
    let id = pipeline::enqueue(&db, &task).unwrap().unwrap();
    db.conn
        .execute(
            "UPDATE preview_jobs SET phase='publishing' WHERE id=?",
            [&id],
        )
        .unwrap();
    let status = crate::management::dispatch(&db, "runtime_drain", &json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(status["active"], 1);
    assert_eq!(status["drained"], false);
    assert_eq!(crate::project_runtime::host_active(&db).unwrap(), 1);
    db.conn
        .execute(
            "UPDATE preview_jobs SET phase='succeeded' WHERE id=?",
            [&id],
        )
        .unwrap();
    assert_eq!(crate::management::status(&db).unwrap()["drained"], true);
}

#[tokio::test]
async fn held_reservation_prevents_drain_until_confirmed_release() {
    let (_d, db, task, _row, _c) = fixture();
    let id = pipeline::enqueue(&db, &task).unwrap().unwrap();
    db.conn
        .execute(
            "UPDATE preview_jobs SET phase='held',reservation='lease' WHERE id=?",
            [&id],
        )
        .unwrap();
    let status = crate::management::dispatch(&db, "runtime_drain", &json!({}))
        .unwrap()
        .unwrap();
    assert_eq!(status["active"], 0);
    assert_eq!(status["pending_preview_reservations"], 1);
    assert_eq!(status["drained"], false);
    assert_eq!(crate::project_runtime::host_active(&db).unwrap(), 0);
    struct Release(&'static str);
    impl lease::Client for Release {
        async fn request(&self, _: &Policy, path: &str, _: &Value) -> Result<Value> {
            assert_eq!(path, "/v1/reservations/lease/release");
            Ok(json!({"state":self.0}))
        }
    }
    let p = configure(&config()).unwrap().projects.remove(0);
    assert!(
        lease::release(&db, &id, &p, &Release("admitted"))
            .await
            .is_err()
    );
    assert_eq!(crate::management::status(&db).unwrap()["drained"], false);
    lease::release(&db, &id, &p, &Release("expired"))
        .await
        .unwrap();
    assert_eq!(crate::management::status(&db).unwrap()["drained"], true);
}

#[tokio::test]
async fn reservation_status_reconciliation_preserves_uncertain_effects() {
    struct Status(Value);
    impl lease::StatusClient for Status {
        async fn status(&self, _: &Policy, id: &str) -> Result<Value> {
            assert_eq!(id, "a".repeat(64));
            if self.0.is_null() {
                anyhow::bail!("lost status response");
            }
            Ok(self.0.clone())
        }
    }
    for phase in ["held", "superseded"] {
        let (_d, db, task, _row, _c) = fixture();
        let id = pipeline::enqueue(&db, &task).unwrap().unwrap();
        db.conn.execute("UPDATE preview_jobs SET phase=?,reservation=?,error='retained failure',receipt='retained receipt' WHERE id=?", params![phase,"a".repeat(64),id]).unwrap();
        db.conn
            .execute(
                "INSERT INTO preview_admissions VALUES(?,1,'original-key')",
                [&id],
            )
            .unwrap();
        let job = db
            .rows("SELECT * FROM preview_jobs WHERE id=?", &[&id])
            .unwrap()
            .remove(0);
        let (p, _, scope) = policy(&db, &crate::projects::task_project(&db, &task).unwrap())
            .unwrap()
            .unwrap();
        let response = json!({"schema_version":1,"id":"a".repeat(64),"state":"admitted","identity":{"schema_version":1,"idempotency_key":"original-key","scope":scope,"project_id":p.project_id,"run_id":task,"built_commit":job["head"],"recipe_hash":job["recipe"]}});
        for state in ["admitted", "unknown"] {
            let mut r = response.clone();
            r["state"] = json!(state);
            assert!(!lease::reconcile(&db, &id, &Status(r)).await.unwrap());
            assert_eq!(
                crate::management::dispatch(&db, "runtime_drain", &json!({}))
                    .unwrap()
                    .unwrap()["drained"],
                false
            );
        }
        assert!(
            lease::reconcile(&db, &id, &Status(Value::Null))
                .await
                .is_err()
        );
        for field in ["id", "run_id", "recipe_hash", "idempotency_key", "scope"] {
            let mut r = response.clone();
            r["state"] = json!("expired");
            if field == "id" {
                r[field] = json!("b".repeat(64));
            } else {
                r["identity"][field] = json!("changed");
            }
            assert!(
                lease::reconcile(&db, &id, &Status(r)).await.is_err(),
                "{field}"
            );
            assert_eq!(crate::management::status(&db).unwrap()["drained"], false);
        }
        let mut terminal = response;
        terminal["state"] = json!(if phase == "held" {
            "expired"
        } else {
            "released"
        });
        assert!(lease::reconcile(&db, &id, &Status(terminal)).await.unwrap());
        let after = db
            .rows("SELECT * FROM preview_jobs WHERE id=?", &[&id])
            .unwrap()
            .remove(0);
        assert_eq!(after["phase"], job["phase"]);
        assert_eq!(after["error"], job["error"]);
        assert_eq!(after["receipt"], job["receipt"]);
        assert!(after["reservation"].is_null());
        assert_eq!(crate::management::status(&db).unwrap()["drained"], true);
    }
}

#[tokio::test]
async fn lost_reservation_post_and_legacy_intent_hold_drain_until_exact_status() {
    struct Lost<'a>(&'a Store, std::cell::RefCell<Option<Value>>);
    impl lease::Client for Lost<'_> {
        async fn request(&self, _: &Policy, path: &str, body: &Value) -> Result<Value> {
            assert_eq!(path, "/v1/reservations");
            assert_eq!(
                crate::management::status(self.0).unwrap()["pending_preview_reservations"],
                1
            );
            assert_eq!(
                self.0
                    .rows("SELECT state FROM preview_admission_receipts", &[])
                    .unwrap()[0]["state"],
                "pending"
            );
            self.1.replace(Some(body.clone()));
            anyhow::bail!("successful POST response lost");
        }
    }
    struct Observed(Value);
    impl lease::StatusClient for Observed {
        async fn status(&self, _: &Policy, id: &str) -> Result<Value> {
            assert_eq!(id, self.0["id"]);
            Ok(self.0.clone())
        }
    }
    for legacy in [false, true] {
        let (_d, db, task, _row, _c) = fixture();
        let id = pipeline::enqueue(&db, &task).unwrap().unwrap();
        let job = db
            .rows("SELECT * FROM preview_jobs WHERE id=?", &[&id])
            .unwrap()
            .remove(0);
        let (p, _, scope) = policy(&db, &crate::projects::task_project(&db, &task).unwrap())
            .unwrap()
            .unwrap();
        let request = json!({"schema_version":1,"idempotency_key":format!("{id}-reservation-1"),"scope":scope,"project_id":p.project_id,"run_id":task,"built_commit":job["head"],"recipe_hash":job["recipe"],"estimated_publish_bytes":p.estimated_publish_bytes});
        if legacy {
            db.conn
                .execute(
                    "INSERT INTO preview_admissions VALUES(?,1,?)",
                    params![id, request["idempotency_key"].as_str().unwrap()],
                )
                .unwrap();
        } else {
            let lost = Lost(&db, Default::default());
            assert!(lease::reserve(&db, &id, &p, &request, &lost).await.is_err());
            assert_eq!(lost.1.borrow().as_ref().unwrap(), &request);
        }
        db.conn.execute("UPDATE preview_jobs SET phase='held',reservation=NULL,error='held after lost response' WHERE id=?",[&id]).unwrap();
        assert_eq!(
            crate::management::dispatch(&db, "runtime_drain", &json!({}))
                .unwrap()
                .unwrap()["drained"],
            false
        );
        let rid = crate::store::hash(
            format!("{}:{}", scope, request["idempotency_key"].as_str().unwrap()).as_bytes(),
        );
        let mut response =
            json!({"schema_version":1,"id":rid,"state":"admitted","identity":request});
        assert!(
            !lease::reconcile(&db, &id, &Observed(response.clone()))
                .await
                .unwrap()
        );
        assert_eq!(crate::management::status(&db).unwrap()["drained"], false);
        response["state"] = json!(if legacy { "held" } else { "expired" });
        assert!(
            lease::reconcile(&db, &id, &Observed(response))
                .await
                .unwrap()
        );
        assert_eq!(crate::management::status(&db).unwrap()["drained"], true);
        assert_eq!(
            db.rows("SELECT phase,error FROM preview_jobs WHERE id=?", &[&id])
                .unwrap()[0],
            json!({"phase":"held","error":"held after lost response"})
        );
        assert_eq!(
            db.rows("SELECT * FROM preview_admissions WHERE job=?", &[&id])
                .unwrap()
                .len(),
            1
        );
    }
}

#[tokio::test]
async fn stale_admission_denial_cannot_clear_retried_intent_and_requests_are_frozen() {
    let (_d, db, task, _row, _c) = fixture();
    let id = pipeline::enqueue(&db, &task).unwrap().unwrap();
    let job = db
        .rows("SELECT * FROM preview_jobs WHERE id=?", &[&id])
        .unwrap()
        .remove(0);
    let (p, _, scope) = policy(&db, &crate::projects::task_project(&db, &task).unwrap())
        .unwrap()
        .unwrap();
    let request = json!({"schema_version":1,"idempotency_key":format!("{id}-reservation-1"),"scope":scope,"project_id":p.project_id,"run_id":task,"built_commit":job["head"],"recipe_hash":job["recipe"],"estimated_publish_bytes":p.estimated_publish_bytes});
    let rid = lease_receipts::intent(&db, &id, 1, &request).unwrap();
    let mut changed = request.clone();
    changed["estimated_publish_bytes"] = json!(2);
    assert!(lease_receipts::intent(&db, &id, 1, &changed).is_err());
    changed = request.clone();
    changed["idempotency_key"] = json!("another-key");
    assert!(lease_receipts::intent(&db, &id, 1, &changed).is_err());
    struct Status<'a>(&'a Store, String, Value, bool);
    impl lease::StatusClient for Status<'_> {
        async fn status(&self, _: &Policy, _: &str) -> Result<Value> {
            if self.3 {
                lease_receipts::intent(self.0, &self.1, 1, &self.2).unwrap();
            }
            Ok(
                json!({"schema_version":1,"id":crate::store::hash(format!("{}:{}",self.2["scope"].as_str().unwrap(),self.2["idempotency_key"].as_str().unwrap()).as_bytes()),"state":"held","identity":self.2}),
            )
        }
    }
    db.conn
        .execute("UPDATE preview_jobs SET phase='held' WHERE id=?", [&id])
        .unwrap();
    crate::management::dispatch(&db, "runtime_drain", &json!({})).unwrap();
    assert!(
        !lease::reconcile(&db, &id, &Status(&db, id.clone(), request.clone(), true))
            .await
            .unwrap()
    );
    assert_eq!(
        db.rows("SELECT reservation FROM preview_jobs WHERE id=?", &[&id])
            .unwrap()[0]["reservation"],
        rid
    );
    assert_eq!(crate::management::status(&db).unwrap()["drained"], false);
    struct Missing;
    impl lease::StatusClient for Missing {
        async fn status(&self, _: &Policy, _: &str) -> Result<Value> {
            anyhow::bail!("HTTP404 not found");
        }
    }
    assert!(lease::reconcile(&db, &id, &Missing).await.is_err());
    assert_eq!(crate::management::status(&db).unwrap()["drained"], false);
    assert!(
        lease::reconcile(&db, &id, &Status(&db, id.clone(), request, false))
            .await
            .unwrap()
    );
    assert_eq!(crate::management::status(&db).unwrap()["drained"], true);
    assert_eq!(
        db.rows(
            "SELECT state,generation FROM preview_admission_receipts WHERE job=?",
            &[&id]
        )
        .unwrap()[0],
        json!({"state":"held","generation":2})
    );
}

#[tokio::test]
async fn draining_queue_observes_expiry_without_dispatching_publication() {
    tokio::task::LocalSet::new().run_until(async {
        let (_d, db, task, _row, _c)=fixture();
        let id=pipeline::enqueue(&db,&task).unwrap().unwrap();
        let rid="a".repeat(64);
        db.conn.execute("UPDATE preview_jobs SET phase='superseded',reservation=?,error='retained' WHERE id=?",params![rid,id]).unwrap();
        db.conn.execute("INSERT INTO preview_admissions VALUES(?,1,'original-key')",[&id]).unwrap();
        let job=db.rows("SELECT * FROM preview_jobs WHERE id=?",&[&id]).unwrap().remove(0);
        let effects_before=db.rows("SELECT * FROM events WHERE kind IN ('run.preview_phase','run.checkpoint_created','run.promoted')",&[]).unwrap();
        let project=crate::projects::task_project(&db,&task).unwrap();
        let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let token=db.root.join("fixture-token");
        std::fs::write(&token,"fixture-admission-token-0123456789abcdef").unwrap();
        let (mut p,_,scope)=policy(&db,&project).unwrap().unwrap();
        p.admission_url=format!("http://{}",listener.local_addr().unwrap()); p.admission_token_file=token;
        db.conn.execute("UPDATE preview_policies SET policy=? WHERE project=?",params![serde_json::to_string(&p).unwrap(),project]).unwrap();
        let body=json!({"schema_version":1,"id":rid,"state":"expired","identity":{"schema_version":1,"idempotency_key":"original-key","scope":scope,"project_id":project,"run_id":task,"built_commit":job["head"],"recipe_hash":job["recipe"]}}).to_string();
        let (send,receive)=std::sync::mpsc::channel();
        let server=std::thread::spawn(move||{
            use std::io::{Read,Write};
            let (mut stream,_)=listener.accept().unwrap();
            stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
            let mut bytes=[0;4096]; let n=stream.read(&mut bytes).unwrap();
            let request=String::from_utf8_lossy(&bytes[..n]);
            assert!(request.starts_with(&format!("GET /v1/reservations/{} HTTP/1.1", "a".repeat(64))));
            assert!(request.to_ascii_lowercase().contains("authorization: bearer fixture-admission-token-0123456789abcdef"));
            receive.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        });
        crate::management::dispatch(&db,"runtime_drain",&json!({})).unwrap();
        let mut queue=Queue::default();
        assert!(!queue.tick(&db).await.unwrap());
        assert!(!queue.tick(&db).await.unwrap());
        assert_eq!(crate::management::status(&db).unwrap()["drained"],false);
        send.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5),async {
            while crate::management::status(&db).unwrap()["drained"]!=true {
                assert!(!queue.tick(&db).await.unwrap());
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        assert!(!queue.tick(&db).await.unwrap());
        queue.shutdown(&db).await.unwrap(); server.join().unwrap();
        let after=db.rows("SELECT * FROM preview_jobs WHERE id=?",&[&id]).unwrap().remove(0);
        assert_eq!(after["phase"],"superseded"); assert_eq!(after["error"],"retained");
        assert_eq!(db.rows("SELECT * FROM attempts",&[]).unwrap().len(),1);
        assert_eq!(db.rows("SELECT * FROM events WHERE kind IN ('run.preview_phase','run.checkpoint_created','run.promoted')",&[]).unwrap(),effects_before);
    }).await;
}
struct AdmissionMock {
    states: std::cell::RefCell<Vec<String>>,
    keys: std::cell::RefCell<Vec<String>>,
}
impl lease::Client for AdmissionMock {
    async fn request(&self, _: &Policy, path: &str, body: &Value) -> Result<Value> {
        if path.ends_with("/release") {
            return Ok(json!({"state":"released"}));
        }
        self.keys
            .borrow_mut()
            .push(body["idempotency_key"].as_str().unwrap().into());
        let state = self.states.borrow_mut().remove(0);
        Ok(
            json!({"schema_version":1,"state":state,"id":crate::store::hash(format!("{}:{}",body["scope"].as_str().unwrap(),body["idempotency_key"].as_str().unwrap()).as_bytes()),"identity":body}),
        )
    }
}
#[test]
fn reservation_restart_reuses_key_and_expiry_persists_new_attempt() {
    let (_d, db, task, _row, c) = fixture();
    let id = pipeline::enqueue(&db, &task).unwrap().unwrap();
    let p = configure(&c).unwrap().projects.remove(0);
    let body = json!({"schema_version":1,"scope":"local","project_id":p.project_id,"run_id":task,"built_commit":"head","recipe_hash":p.recipe_hash(),"estimated_publish_bytes":1});
    let m = AdmissionMock {
        states: std::cell::RefCell::new(vec![
            "admitted".into(),
            "admitted".into(),
            "expired".into(),
            "admitted".into(),
        ]),
        keys: Default::default(),
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            assert_eq!(
                lease::reserve(&db, &id, &p, &body, &m).await.unwrap(),
                crate::store::hash(format!("local:{id}-reservation-1").as_bytes())
            );
            lease::reserve(&db, &id, &p, &body, &m).await.unwrap();
            lease::reserve(&db, &id, &p, &body, &m).await.unwrap();
            lease::release(&db, &id, &p, &m).await.unwrap();
        });
    let keys = m.keys.borrow();
    assert_eq!(keys[0], keys[1]);
    assert_eq!(keys[1], keys[2]);
    assert_ne!(keys[2], keys[3]);
    assert!(
        db.rows("SELECT reservation FROM preview_jobs WHERE id=?", &[&id])
            .unwrap()[0]["reservation"]
            .is_null()
    );
    assert_eq!(
        db.rows(
            "SELECT ordinal,state FROM preview_admission_receipts WHERE job=? ORDER BY ordinal",
            &[&id]
        )
        .unwrap(),
        vec![
            json!({"ordinal":1,"state":"expired"}),
            json!({"ordinal":2,"state":"released"})
        ]
    );
}
#[test]
fn held_admission_cannot_invoke_publisher_and_retries_same_reservation_key() {
    let (_d, db, task, _row, c) = fixture();
    let id = pipeline::enqueue(&db, &task).unwrap().unwrap();
    let p = configure(&c).unwrap().projects.remove(0);
    let body = json!({"schema_version":1,"scope":"local","project_id":p.project_id,"run_id":task,"built_commit":"head","recipe_hash":p.recipe_hash(),"estimated_publish_bytes":1});
    let m = AdmissionMock {
        states: std::cell::RefCell::new(vec!["held".into(), "admitted".into()]),
        keys: Default::default(),
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            assert!(lease::reserve(&db, &id, &p, &body, &m).await.is_err());
            assert!(lease::reserve(&db, &id, &p, &body, &m).await.is_ok());
        });
    assert_eq!(m.keys.borrow()[0], m.keys.borrow()[1]);
}
#[test]
fn draining_controller_does_not_dispatch_queued_preview() {
    let (_d, db, task, _row, _c) = fixture();
    pipeline::enqueue(&db, &task).unwrap().unwrap();
    crate::management::dispatch(&db, "runtime_drain", &json!({})).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut q = Queue::default();
            assert!(!q.tick(&db).await.unwrap());
            q.shutdown(&db).await.unwrap();
        });
    assert_eq!(status(&db, &task).unwrap()["phase"], "queued");
}
#[test]
fn private_admission_http_uses_scoped_credential_and_rejects_bad_receipts() {
    let d = tempfile::tempdir().unwrap();
    let token = d.path().join("admission-token");
    std::fs::write(&token, "scoped-purpose-token-0123456789abcdef").unwrap();
    let mut p = configure(&config()).unwrap().projects.remove(0);
    p.admission_token_file = token;
    for (body, success) in [
        (r#"{"schema_version":1,"state":"admitted"}"#, true),
        ("bad-json", false),
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        p.admission_url = format!("http://{}", listener.local_addr().unwrap());
        let body = body.to_owned();
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut bytes = [0; 4096];
            let n = stream.read(&mut bytes).unwrap();
            let req = String::from_utf8_lossy(&bytes[..n]);
            assert!(
                req.to_ascii_lowercase()
                    .contains("authorization: bearer scoped-purpose-token-0123456789abcdef")
            );
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        });
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(process::admission(
                &p,
                reqwest::Method::GET,
                "/v1/reservations/fixture",
                None,
            ));
        assert_eq!(result.is_ok(), success);
        server.join().unwrap();
    }
    use std::os::unix::fs::symlink;
    let link = d.path().join("token-link");
    symlink(&p.admission_token_file, &link).unwrap();
    p.admission_token_file = link;
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(process::admission(
            &p,
            reqwest::Method::POST,
            "/v1/reservations",
            Some(&json!({})),
        ));
    assert!(result.is_err());
}
#[test]
fn policy_reconfiguration_invalidates_inflight_job() {
    let (_d, db, task, _row, mut c) = fixture();
    let id = pipeline::enqueue(&db, &task).unwrap().unwrap();
    let j = db
        .rows("SELECT * FROM preview_jobs WHERE id=?", &[&id])
        .unwrap()
        .remove(0);
    let (p, generation, _) = policy(&db, &p_project(&c)).unwrap().unwrap();
    assert!(pipeline::fresh(&db, &j, &p, &generation).is_ok());
    c["projects"][0]["enabled"] = json!(false);
    setup(&db, &c).unwrap();
    assert!(pipeline::fresh(&db, &j, &p, &generation).is_err());
    assert_eq!(status(&db, &task).unwrap()["enabled"], false);
}
fn p_project(c: &Value) -> String {
    c["projects"][0]["project_id"].as_str().unwrap().into()
}
#[test]
fn previous_successful_attempt_cannot_satisfy_a_new_review_binding() {
    let (_d, db, task, row, c) = fixture();
    let (_, generation, _) = policy(&db, &p_project(&c)).unwrap().unwrap();
    let (head, tree) = pipeline::identity(&db, &task).unwrap();
    assert!(
        review::reviewed(&db, &task, &head, &tree, &generation)
            .unwrap()
            .is_some()
    );
    prepare_review(&db, &task, &row).unwrap();
    assert!(
        review::reviewed(&db, &task, &head, &tree, &generation)
            .unwrap()
            .is_none()
    );
    assert!(pipeline::enqueue(&db, &task).unwrap().is_none());
    assert_eq!(db.steps(&task).unwrap().len(), 3);
}
fn fixture_execution(db: &Store, task: &str, row: &Value) {
    let w = db
        .rows("SELECT worker FROM attempts WHERE id='review-attempt'", &[])
        .unwrap()
        .remove(0)["worker"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = crate::git::allocate(db, task, &w).unwrap();
    let settings =
        serde_json::from_str(db.task(task).unwrap()["settings"].as_str().unwrap()).unwrap();
    verify_execution(db, row, "review-attempt", &w, &settings, "reviewer", &path).unwrap();
}
#[test]
fn simulated_executor_and_checkout_cannot_attest_integrated_review() {
    let (_d, db, task, row, _c) = fixture();
    let mut settings: crate::config::Settings =
        serde_json::from_str(db.task(&task).unwrap()["settings"].as_str().unwrap()).unwrap();
    for p in settings.providers.values_mut() {
        p.kind = "simulated".into();
    }
    db.conn
        .execute(
            "UPDATE tasks SET settings=? WHERE id=?",
            params![serde_json::to_string(&settings).unwrap(), task],
        )
        .unwrap();
    assert!(prepare_review(&db, &task, &row).is_err());
    let mut checkout = row;
    let mut spec = Store::step(&checkout).unwrap();
    spec.workspace = Some(crate::template::CommandWorkspace::Checkout);
    checkout["spec"] = json!(serde_json::to_string(&spec).unwrap());
    assert!(prepare_review(&db, &task, &checkout).is_err());
}
#[test]
fn retry_reviewer_fast_forwards_only_clean_registered_workspace() {
    let (_d, db, task, row, _c) = fixture();
    let integrated = crate::git::task_workspace(&db, &task).unwrap();
    crate::git::run(&integrated, &["commit", "--allow-empty", "-m", "feedback"]).unwrap();
    prepare_review(&db, &task, &row).unwrap();
    bind_attempt(&db, row["id"].as_str().unwrap(), "review-attempt").unwrap();
    fixture_execution(&db, &task, &row);
    let x = db
        .rows(
            "SELECT * FROM preview_review_execution WHERE attempt='review-attempt'",
            &[],
        )
        .unwrap();
    assert_eq!(
        x[0]["head"],
        crate::git::run(&integrated, &["rev-parse", "HEAD"]).unwrap()
    );
    let path = Path::new(x[0]["workspace"].as_str().unwrap());
    std::fs::write(path.join("unreviewed"), "retain me").unwrap();
    let w = db
        .rows("SELECT worker FROM attempts WHERE id='review-attempt'", &[])
        .unwrap();
    let settings =
        serde_json::from_str(db.task(&task).unwrap()["settings"].as_str().unwrap()).unwrap();
    assert!(
        verify_execution(
            &db,
            &row,
            "review-attempt",
            w[0]["worker"].as_str().unwrap(),
            &settings,
            "reviewer",
            path
        )
        .is_err()
    );
    assert!(path.join("unreviewed").exists());
}
#[path = "tests/hardening.rs"]
mod hardening;
