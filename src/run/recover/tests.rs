use super::*;
use crate::{config::Settings, store::Store, template};
use std::{collections::BTreeMap, path::PathBuf};

struct Fixture {
    _dir: tempfile::TempDir,
    db: Store,
    task: String,
    worker: String,
    worker_token: String,
    workspace: PathBuf,
    base: String,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.com"],
            vec!["commit", "--allow-empty", "-m", "initial"],
        ] {
            git::run(&repo, &args).unwrap();
        }
        let db = Store::open(&dir.path().join("data")).unwrap();
        let mut plan = template::compile(
            "simulated",
            &template::load_templates(Path::new("absent")).unwrap(),
            BTreeMap::from([("task".into(), "test".into())]),
        )
        .unwrap();
        plan.steps
            .iter_mut()
            .find(|step| step.id == "left")
            .unwrap()
            .kind = "agent".into();
        let task = db
            .submit("test", &repo, &Settings::default(), &plan)
            .unwrap();
        let step = db
            .steps(&task)
            .unwrap()
            .into_iter()
            .find(|row| row["name"] == "left")
            .unwrap();
        let sid = step["id"].as_str().unwrap();
        let registered = db.register(&task, Some(sid)).unwrap();
        let worker = registered["id"].as_str().unwrap().to_owned();
        let worker_token = registered["token"].as_str().unwrap().to_owned();
        let workspace = git::allocate(&db, &task, &worker).unwrap();
        let base = head(&workspace).unwrap();
        db.claim(&task, &worker, &["hello.txt".into()]).unwrap();
        std::fs::write(workspace.join("hello.txt"), "recovered\n").unwrap();
        db.conn
            .execute(
                "UPDATE steps SET state='succeeded' WHERE task=? AND name IN ('plan','right')",
                [&task],
            )
            .unwrap();
        db.conn
            .execute("UPDATE steps SET state='failed' WHERE id=?", [sid])
            .unwrap();
        db.conn
            .execute(
                "UPDATE steps SET state='skipped' WHERE task=? AND name='verify'",
                [&task],
            )
            .unwrap();
        db.conn
            .execute("UPDATE tasks SET status='failed' WHERE id=?", [&task])
            .unwrap();
        db.conn
            .execute("UPDATE workers SET status='failed' WHERE id=?", [&worker])
            .unwrap();
        db.conn.execute("INSERT INTO attempts(id,step,worker,state,started,finished,result) VALUES('failed-agent',?,?,'failed',1,2,'{}')",params![sid,worker]).unwrap();
        Self {
            _dir: dir,
            db,
            task,
            worker,
            worker_token,
            workspace,
            base,
        }
    }

    fn recover(&self, key: &str, validation: &[String]) -> Result<Value> {
        recover_with_policy(
            &self.db, &self.task, "left", &self.base, &self.base, validation, key,
        )
    }
}

fn checks() -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        "grep -qx recovered hello.txt".into(),
    ]
}

#[test]
fn recovers_agent_edits_on_same_run_and_resets_only_skipped_dependents() {
    let f = Fixture::new();
    let recovered = f.recover("stable-request", &checks()).unwrap();
    assert_eq!(recovered["recovered"], true);
    assert_ne!(recovered["worker_commit_sha"], recovered["run_head_sha"]);
    assert_eq!(
        recovered["run_head_sha"],
        head(&git::task_workspace(&f.db, &f.task).unwrap()).unwrap()
    );
    assert_eq!(
        recovered["tree_sha"],
        git::run(&f.workspace, &["rev-parse", "HEAD^{tree}"]).unwrap()
    );
    assert_eq!(
        recovered["branch_ref"],
        format!("refs/heads/horde/{}", f.task)
    );
    assert_eq!(f.recover("stable-request", &checks()).unwrap(), recovered);
    let steps = f.db.steps(&f.task).unwrap();
    assert_eq!(
        steps.iter().find(|row| row["name"] == "left").unwrap()["state"],
        "succeeded"
    );
    assert_eq!(
        steps.iter().find(|row| row["name"] == "verify").unwrap()["state"],
        "pending"
    );
    assert_eq!(
        steps.iter().find(|row| row["name"] == "right").unwrap()["state"],
        "succeeded"
    );
    assert_eq!(f.db.task(&f.task).unwrap()["status"], "running");
    assert_eq!(f.db.worker(&f.worker).unwrap()["status"], "idle");
    assert!(
        f.db.rows("SELECT path FROM claims WHERE worker=?", &[&f.worker])
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        f.db.rows("SELECT state FROM attempts WHERE id='failed-agent'", &[])
            .unwrap()[0]["state"],
        "failed"
    );
    assert!(f.recover("stable-request", &["true".into()]).is_err());
}

#[test]
fn failed_checks_do_not_accept_work_or_advance_run() {
    let f = Fixture::new();
    assert!(f.recover("bad-check", &["false".into()]).is_err());
    assert_eq!(
        head(&git::task_workspace(&f.db, &f.task).unwrap()).unwrap(),
        f.base
    );
    assert_eq!(head(&f.workspace).unwrap(), f.base);
    assert_eq!(
        f.db.steps(&f.task)
            .unwrap()
            .iter()
            .find(|row| row["name"] == "left")
            .unwrap()["state"],
        "failed"
    );
    assert!(f.recover("good-check", &checks()).is_ok());
}

#[test]
fn stale_run_head_and_out_of_scope_changes_are_rejected() {
    let f = Fixture::new();
    let integrated = git::task_workspace(&f.db, &f.task).unwrap();
    git::run(&integrated, &["commit", "--allow-empty", "-m", "external"]).unwrap();
    assert!(f.recover("stale-head", &checks()).is_err());

    let f = Fixture::new();
    std::fs::write(f.workspace.join("outside.txt"), "unclaimed\n").unwrap();
    assert!(f.recover("out-of-scope", &checks()).is_err());
    assert_eq!(head(&f.workspace).unwrap(), f.base);
}

#[test]
fn renewed_head_revalidates_a_prior_recovery_commit() {
    let f = Fixture::new();
    let integrated = git::task_workspace(&f.db, &f.task).unwrap();
    let move_head = vec![
        "sh".into(),
        "-c".into(),
        format!(
            "grep -qx recovered hello.txt && git -C '{}' commit --allow-empty -m concurrent",
            integrated.display()
        ),
    ];
    assert!(f.recover("first-key", &move_head).is_err());
    let committed = head(&f.workspace).unwrap();
    assert_ne!(committed, f.base);
    let new_run_head = head(&integrated).unwrap();
    assert_ne!(new_run_head, f.base);
    let result = recover_with_policy(
        &f.db,
        &f.task,
        "left",
        &committed,
        &new_run_head,
        &checks(),
        "renewed-key",
    )
    .unwrap();
    assert_eq!(result["worker_commit_sha"], committed);
    assert_eq!(f.db.task(&f.task).unwrap()["status"], "running");
    assert_eq!(
        recover_with_policy(
            &f.db,
            &f.task,
            "left",
            &committed,
            &new_run_head,
            &checks(),
            "renewed-key"
        )
        .unwrap(),
        result
    );
}

#[test]
fn retry_after_commit_before_receipt_update_keeps_one_worker_commit() {
    let f = Fixture::new();
    let step =
        f.db.steps(&f.task)
            .unwrap()
            .into_iter()
            .find(|row| row["name"] == "left")
            .unwrap();
    let sid = step["id"].as_str().unwrap();
    git::run(&f.workspace, &["add", "--all"]).unwrap();
    let staged = tree(&f.workspace).unwrap();
    let request = json!({"step":"left","expected_worker_head":f.base,
            "expected_run_head":f.base,"validation":checks()});
    f.db.conn.execute("INSERT INTO run_step_recoveries(task,idempotency_key,request,step,attempt,worker,validated_tree,commit_key,commit_parent,phase,created) VALUES(?,?,?,?,?,?,?,?,?,'validated',?)",
            params![f.task,"lost-reply",request.to_string(),sid,"failed-agent",f.worker,staged,"lost-reply",f.base,now()]).unwrap();
    git::run(
        &f.workspace,
        &[
            "-c",
            "user.name=Horde Recovery",
            "-c",
            "user.email=recovery@horde.sh",
            "commit",
            "-m",
            "Recovered\n\nHorde-Recovery-Id: lost-reply",
        ],
    )
    .unwrap();
    let committed = head(&f.workspace).unwrap();
    let result = f.recover("lost-reply", &checks()).unwrap();
    assert_eq!(result["worker_commit_sha"], committed);
    assert_eq!(f.recover("lost-reply", &checks()).unwrap(), result);
}

#[test]
fn retry_after_integration_before_final_receipt_keeps_exact_merge() {
    let f = Fixture::new();
    let step =
        f.db.steps(&f.task)
            .unwrap()
            .into_iter()
            .find(|row| row["name"] == "left")
            .unwrap();
    let sid = step["id"].as_str().unwrap();
    git::run(&f.workspace, &["add", "--all"]).unwrap();
    let staged = tree(&f.workspace).unwrap();
    let request = json!({"step":"left","expected_worker_head":f.base,
            "expected_run_head":f.base,"validation":checks()});
    git::run(
        &f.workspace,
        &[
            "-c",
            "user.name=Horde Recovery",
            "-c",
            "user.email=recovery@horde.sh",
            "commit",
            "-m",
            "Recovered\n\nHorde-Recovery-Id: lost-final",
        ],
    )
    .unwrap();
    let committed = head(&f.workspace).unwrap();
    f.db.conn.execute("INSERT INTO run_step_recoveries(task,idempotency_key,request,step,attempt,worker,validated_tree,commit_key,commit_parent,phase,worker_commit,created) VALUES(?,?,?,?,?,?,?,?,?,'committed',?,?)",
            params![f.task,"lost-final",request.to_string(),sid,"failed-agent",f.worker,staged,"lost-final",f.base,committed,now()]).unwrap();
    let integrated =
        git::integrate_expected(&f.db, &f.task, &f.worker, &checks(), Some(&f.base)).unwrap();
    let merged = integrated["integrated_head"].as_str().unwrap().to_owned();
    assert_ne!(merged, f.base);
    // Simulate a crash after Git made the exact merge and before the
    // integration state or recovery response became durable.
    f.db.conn
        .execute(
            "UPDATE integrations SET state='running' WHERE task=? AND worker=? AND commit_id=?",
            params![f.task, f.worker, committed],
        )
        .unwrap();
    let result = f.recover("lost-final", &checks()).unwrap();
    assert_eq!(result["run_head_sha"], merged);
    assert_eq!(
        head(&git::task_workspace(&f.db, &f.task).unwrap()).unwrap(),
        merged
    );
    assert_eq!(
        f.db.rows(
            "SELECT state FROM integrations WHERE task=? AND worker=? AND commit_id=?",
            &[&f.task, &f.worker, &committed]
        )
        .unwrap()[0]["state"],
        "succeeded"
    );
    assert_eq!(f.recover("lost-final", &checks()).unwrap(), result);
}

#[test]
fn retry_holds_when_run_moves_beyond_the_previous_merge() {
    let f = Fixture::new();
    let integrated = git::task_workspace(&f.db, &f.task).unwrap();
    let sid =
        f.db.steps(&f.task)
            .unwrap()
            .into_iter()
            .find(|row| row["name"] == "left")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
    git::run(&f.workspace, &["add", "--all"]).unwrap();
    let staged = tree(&f.workspace).unwrap();
    let request = json!({"step":"left","expected_worker_head":f.base,
            "expected_run_head":f.base,"validation":checks()});
    git::run(
        &f.workspace,
        &[
            "-c",
            "user.name=Horde Recovery",
            "-c",
            "user.email=recovery@horde.sh",
            "commit",
            "-m",
            "Recovered\n\nHorde-Recovery-Id: racing-key",
        ],
    )
    .unwrap();
    let committed = head(&f.workspace).unwrap();
    f.db.conn.execute("INSERT INTO run_step_recoveries(task,idempotency_key,request,step,attempt,worker,validated_tree,commit_key,commit_parent,phase,worker_commit,created) VALUES(?,?,?,?,?,?,?,?,?,'committed',?,?)",
            params![f.task,"racing-key",request.to_string(),sid,"failed-agent",f.worker,staged,"racing-key",f.base,committed,now()]).unwrap();
    git::integrate_expected(&f.db, &f.task, &f.worker, &checks(), Some(&f.base)).unwrap();
    git::run(
        &integrated,
        &["commit", "--allow-empty", "-m", "authorized next push"],
    )
    .unwrap();
    assert!(f.recover("racing-key", &checks()).is_err());
    assert_eq!(
        f.db.steps(&f.task)
            .unwrap()
            .into_iter()
            .find(|row| row["name"] == "left")
            .unwrap()["state"],
        "failed"
    );
}

#[test]
fn validation_timeout_kills_the_check_without_committing_agent_work() {
    let f = Fixture::new();
    assert!(
        checked_command(
            &f.workspace,
            &["sh".into(), "-c".into(), "sleep 5".into()],
            1
        )
        .is_err()
    );
    assert_eq!(head(&f.workspace).unwrap(), f.base);
}

#[test]
fn worker_credentials_cannot_invoke_operator_recovery() {
    let f = Fixture::new();
    assert!(!crate::protocol::worker_allowed("run_recover_step"));
    assert!(
        crate::protocol::dispatch(
            &f.db,
            "run_recover_step",
            json!({"task":f.task,"step":"left","expected_worker_head":f.base,
                "expected_run_head":f.base,"validation":checks(),"idempotency_key":"worker"}),
            Some(&f.worker_token)
        )
        .is_err()
    );
    assert_eq!(head(&f.workspace).unwrap(), f.base);
}
