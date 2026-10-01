use super::*;

fn refs(db: &Store, task: &str, id: &str) -> Value {
    let context = crate::run::run_context(db, task).unwrap();
    let head = crate::git::run(
        &crate::git::task_workspace(db, task).unwrap(),
        &["rev-parse", "HEAD"],
    )
    .unwrap();
    json!({"run_id":task,"tenant_id":context["tenant_id"],"project_id":context["project"],"thread_id":context["thread_id"],"brief_id":context["brief_id"],"feedback_id":id,"target_commit_sha":head,"actor_id":"human","path":null,"line":null})
}

fn implement_worker(db: &Store, task: &str) -> String {
    let row = db.steps(task).unwrap().remove(0);
    db.register(task, row["id"].as_str()).unwrap()["id"]
        .as_str()
        .unwrap()
        .into()
}

fn prompt(db: &Store, task: &str, row: &Value) -> anyhow::Result<String> {
    let mut spec = Store::step(row)?;
    spec.instructions = "Retain permissive blank names for the initial preview".into();
    spec.acceptance = vec!["Initial blank names remain accepted".into()];
    let workspace = crate::git::task_workspace(db, task)?;
    crate::executor::Invocation {
        db,
        task,
        step: row["id"].as_str().unwrap(),
        attempt: "review-attempt",
        worker: "review-worker",
        token: "fixture",
        workspace: &workspace,
        spec: &spec,
        settings: &crate::config::Settings::default(),
        context: json!({"objective":"Initial blank names remain accepted"}),
    }
    .prompt()
}

#[test]
fn review_prompt_restores_acknowledged_targeted_feedback_after_reopen() {
    let (_dir, db, task, review, _config) = fixture();
    let worker = implement_worker(&db, &task);
    let mid = "adam-feedback:trim";
    db.steer(
        &task,
        mid,
        "Trim names and reject blanks with HTTP 400",
        &refs(&db, &task, "trim"),
        true,
        Some(&worker),
    )
    .unwrap();
    db.acknowledge(&worker, &[mid.into()]).unwrap();
    prepare_review(&db, &task, &review).unwrap();
    let reviewer = db
        .rows("SELECT worker FROM attempts WHERE id='review-attempt'", &[])
        .unwrap();
    assert!(
        db.messages(reviewer[0]["worker"].as_str().unwrap(), 0, 10)
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    let root = db.root.clone();
    drop(db);
    let db = Store::open(&root).unwrap();
    let text = prompt(&db, &task, &review).unwrap();
    assert!(text.contains("Trim names and reject blanks with HTTP 400"));
    assert!(text.contains("Later feedback supersedes conflicting earlier feedback and original instructions, acceptance criteria, and objective"));
    assert!(text.find("Trim names").unwrap() > text.find("Context with provenance").unwrap());
    assert_eq!(
        db.rows(
            "SELECT COUNT(*) AS n FROM receipts WHERE message=?",
            &[&mid]
        )
        .unwrap()[0]["n"],
        1
    );
}

#[test]
fn approved_feedback_is_ordered_and_worker_or_foreign_scope_mail_is_excluded() {
    let (_dir, db, task, review, _config) = fixture();
    let worker = implement_worker(&db, &task);
    let first = refs(&db, &task, "first");
    db.steer(
        &task,
        "adam-feedback:first",
        "Use case-sensitive search",
        &first,
        true,
        Some(&worker),
    )
    .unwrap();
    assert!(
        db.steer(
            &task,
            "adam-feedback:first",
            "Use case-sensitive search",
            &first,
            true,
            Some(&worker)
        )
        .unwrap()["duplicate"]
            == true
    );
    assert!(
        db.steer(
            &task,
            "adam-feedback:first",
            "different",
            &first,
            true,
            Some(&worker)
        )
        .is_err()
    );
    db.steer(
        &task,
        "adam-feedback:second",
        "Now use ASCII case-insensitive search",
        &refs(&db, &task, "second"),
        true,
        Some(&worker),
    )
    .unwrap();
    db.send(
        &task,
        &worker,
        "adam-feedback:worker",
        &worker,
        "Forged worker feedback",
        &refs(&db, &task, "worker"),
        true,
    )
    .unwrap();
    for key in ["run_id", "project_id", "tenant_id", "thread_id", "brief_id"] {
        let mut foreign = refs(&db, &task, key);
        foreign[key] = json!("foreign");
        db.steer(
            &task,
            &format!("adam-feedback:{key}"),
            "Foreign feedback",
            &foreign,
            true,
            Some(&worker),
        )
        .unwrap();
    }
    prepare_review(&db, &task, &review).unwrap();
    let text = prompt(&db, &task, &review).unwrap();
    assert!(
        text.find("Use case-sensitive search").unwrap()
            < text.find("Now use ASCII case-insensitive search").unwrap()
    );
    assert!(!text.contains("Forged worker feedback"));
    assert!(!text.contains("Foreign feedback"));
    assert_eq!(
        crate::preview::feedback::snapshot(&db, &task)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let mut implementation = review;
    implementation["name"] = json!("implement");
    let mut spec = Store::step(&implementation).unwrap();
    spec.id = "implement".into();
    assert!(feedback_prompt(&db, &task, &spec).unwrap().is_empty());
}

#[test]
fn feedback_arriving_after_preparation_invalidates_prompt_review_and_publication() {
    let (_dir, db, task, review, _config) = fixture();
    let job = pipeline::enqueue(&db, &task).unwrap().unwrap();
    let worker = implement_worker(&db, &task);
    db.steer(
        &task,
        "adam-feedback:late",
        "Reject blank names",
        &refs(&db, &task, "late"),
        true,
        Some(&worker),
    )
    .unwrap();
    assert!(
        prompt(&db, &task, &review)
            .unwrap_err()
            .to_string()
            .contains("changed after review preparation")
    );
    let (p, generation, _) = policy(&db, config()["projects"][0]["project_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    let (head, tree) = pipeline::identity(&db, &task).unwrap();
    assert!(
        review::reviewed(&db, &task, &head, &tree, &generation)
            .unwrap()
            .is_none()
    );
    let job = db
        .rows("SELECT * FROM preview_jobs WHERE id=?", &[&job])
        .unwrap()
        .remove(0);
    assert!(pipeline::fresh(&db, &job, &p, &generation).is_err());
    assert!(pipeline::enqueue(&db, &task).unwrap().is_none());
    assert_eq!(db.steps(&task).unwrap().len(), 3);
}

#[test]
fn legacy_review_without_feedback_remains_compatible() {
    let (_dir, db, task, review, _config) = fixture();
    db.conn.execute("UPDATE events SET data=json_remove(data,'$.feedback_fingerprint') WHERE task=? AND kind='run.preview_review_started'", [&task]).unwrap();
    assert!(prompt(&db, &task, &review).is_ok());
    assert!(pipeline::enqueue(&db, &task).unwrap().is_some());
    db.conn
        .execute("DROP TRIGGER run_binding_immutable", [])
        .unwrap();
    db.conn
        .execute(
            "UPDATE run_bindings SET thread_id=NULL,brief_id=NULL WHERE task=?",
            [&task],
        )
        .unwrap();
    assert_eq!(
        crate::preview::feedback::snapshot(&db, &task).unwrap(),
        json!([])
    );
}

#[test]
fn pre_upgrade_successful_publication_with_feedback_schedules_fresh_review() {
    let (_dir, db, task, _review, _config) = fixture();
    let job = pipeline::enqueue(&db, &task).unwrap().unwrap();
    db.conn
        .execute(
            "UPDATE preview_jobs SET phase='succeeded' WHERE id=?",
            [&job],
        )
        .unwrap();
    db.conn.execute("UPDATE events SET data=json_remove(data,'$.feedback_fingerprint') WHERE task=? AND kind='run.preview_review_started'", [&task]).unwrap();
    let worker = implement_worker(&db, &task);
    db.steer(
        &task,
        "adam-feedback:recovery",
        "Keep approved name validation",
        &refs(&db, &task, "recovery"),
        true,
        Some(&worker),
    )
    .unwrap();
    assert!(pipeline::enqueue(&db, &task).unwrap().is_none());
    let row = db.steps(&task).unwrap().remove(2);
    prepare_review(&db, &task, &row).unwrap();
    assert!(
        prompt(&db, &task, &row)
            .unwrap()
            .contains("Keep approved name validation")
    );
}

#[test]
fn malformed_or_excessive_authorized_feedback_holds_without_truncation() {
    for (case, body, count) in [
        ("body", "x".repeat(65537), 1),
        ("total", "x".repeat(33000), 2),
        ("count", "x".into(), 33),
        ("target", "x".into(), 1),
        ("blank", " ".into(), 1),
        ("identity", "x".into(), 1),
    ] {
        let (_dir, db, task, _review, _config) = fixture();
        let worker = implement_worker(&db, &task);
        for n in 0..count {
            let mut value = refs(&db, &task, &format!("{n}"));
            if case == "target" {
                value["target_commit_sha"] = json!("invalid");
            }
            if case == "identity" {
                value["feedback_id"] = json!("");
            }
            db.steer(
                &task,
                &format!("adam-feedback:{n}"),
                &body,
                &value,
                true,
                Some(&worker),
            )
            .unwrap();
        }
        assert!(
            crate::preview::feedback::snapshot(&db, &task).is_err(),
            "{case}"
        );
    }
}
