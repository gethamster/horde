//! The terminal summary is the one place scripts read a task's delivery outcome,
//! so it must name a PR when one exists and explain why when none was attempted.
use horde::{
    config::{Delivery, Settings},
    git, protocol,
    store::Store,
    template,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf};
use tempfile::TempDir;

const DELIVERY_TEMPLATE: &str = r#"
name = "delivered"
version = "1"
[[steps]]
id = "plan"
kind = "simulated"
[[steps]]
id = "deliver"
kind = "delivery"
needs = ["plan"]
instructions = "Open a PR"
"#;

struct Fixture {
    _dir: TempDir,
    db: Store,
    repo: PathBuf,
    templates: PathBuf,
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
        let templates = dir.path().join("templates");
        std::fs::create_dir(&templates).unwrap();
        std::fs::write(templates.join("delivered.toml"), DELIVERY_TEMPLATE).unwrap();
        let db = Store::open(&dir.path().join("data")).unwrap();
        Self {
            _dir: dir,
            db,
            repo,
            templates,
        }
    }
    fn submit(&self, template: &str, settings: &Settings) -> String {
        let plan = template::compile(
            template,
            &template::load_templates(&self.templates).unwrap(),
            BTreeMap::from([("task".into(), "test".into())]),
        )
        .unwrap();
        self.db.submit("test", &self.repo, settings, &plan).unwrap()
    }
    fn finish_step(&self, oid: &str, name: &str, state: &str, result: &Value) {
        self.db
            .conn
            .execute(
                "UPDATE steps SET state=?,result=? WHERE task=? AND name=?",
                rusqlite::params![state, result.to_string(), oid, name],
            )
            .unwrap();
    }
    fn set_status(&self, oid: &str, status: &str) {
        self.db
            .conn
            .execute(
                "UPDATE tasks SET status=? WHERE id=?",
                rusqlite::params![status, oid],
            )
            .unwrap();
    }
    fn summary(&self, oid: &str) -> Value {
        protocol::dispatch(&self.db, "summary", json!({"task":oid}), None).unwrap()
    }
}

fn enabled_delivery() -> Settings {
    Settings {
        delivery: Delivery {
            enabled: true,
            repository: "test/repo".into(),
            base: "main".into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn template_without_delivery_step_reports_explicit_skip() {
    let f = Fixture::new();
    let oid = f.submit("simulated", &Settings::default());
    let s = f.summary(&oid);
    assert_eq!(s["task"], oid);
    assert_eq!(s["branch"], format!("horde/{oid}"));
    assert_eq!(s["terminal"], false);
    assert_eq!(s["delivery"]["outcome"], "skipped");
    assert_eq!(s["delivery"]["pr_url"], Value::Null);
    let reason = s["delivery_skipped"].as_str().unwrap();
    assert_eq!(reason, s["delivery"]["reason"].as_str().unwrap());
    assert!(reason.contains("template has no delivery step"), "{reason}");
    assert!(reason.contains("delivery disabled in settings"), "{reason}");
    assert_eq!(s["steps"].as_array().unwrap().len(), 4);
    assert!(
        s["steps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|step| step["kind"] == "simulated" && step["state"] == "pending")
    );
}

#[test]
fn delivery_step_with_disabled_delivery_reports_disabled_reason() {
    let f = Fixture::new();
    let oid = f.submit("delivered", &Settings::default());
    let s = f.summary(&oid);
    assert_eq!(s["delivery"]["outcome"], "skipped");
    assert_eq!(
        s["delivery_skipped"],
        "delivery disabled in settings ([delivery] enabled = false)"
    );
    assert_eq!(s["delivery"]["reason"], s["delivery_skipped"]);
    assert!(
        s["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step["name"] == "deliver" && step["kind"] == "delivery")
    );
}

#[test]
fn successful_delivery_reports_pr_url() {
    let f = Fixture::new();
    let oid = f.submit("delivered", &enabled_delivery());
    f.finish_step(
        &oid,
        "plan",
        "succeeded",
        &json!({"result":"planned","accepted":true}),
    );
    f.finish_step(
        &oid,
        "deliver",
        "succeeded",
        &json!({"result":"https://example.invalid/pr/1","accepted":true,"delivery":"pr_ready"}),
    );
    f.db.conn
        .execute(
            "INSERT INTO external_ops VALUES(?,'pr','succeeded',?)",
            rusqlite::params![
                oid,
                json!({"number":1,"url":"https://example.invalid/pr/1","state":"OPEN"}).to_string()
            ],
        )
        .unwrap();
    f.set_status(&oid, "succeeded");
    let s = f.summary(&oid);
    assert_eq!(s["status"], "succeeded");
    assert_eq!(s["terminal"], true);
    assert_eq!(s["delivery"]["outcome"], "pr_ready");
    assert_eq!(s["delivery"]["pr_url"], "https://example.invalid/pr/1");
    assert_eq!(s["delivery"]["reason"], Value::Null);
    assert!(s.get("delivery_skipped").is_none());
    let deliver = s["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["name"] == "deliver")
        .unwrap();
    assert_eq!(deliver["accepted"], true);
    assert_eq!(deliver["result"], "https://example.invalid/pr/1");
    assert_eq!(deliver["error"], Value::Null);
}

#[test]
fn merged_delivery_reports_merged_outcome() {
    let f = Fixture::new();
    let oid = f.submit("delivered", &enabled_delivery());
    f.finish_step(&oid, "deliver", "succeeded", &json!({"accepted":true}));
    for (name, url) in [
        ("pr", "https://example.invalid/pr/2"),
        ("merge", "https://example.invalid/pr/2"),
    ] {
        f.db.conn
            .execute(
                "INSERT INTO external_ops VALUES(?,?,'succeeded',?)",
                rusqlite::params![oid, name, json!({"url":url}).to_string()],
            )
            .unwrap();
    }
    let s = f.summary(&oid);
    assert_eq!(s["delivery"]["outcome"], "merged");
    assert_eq!(s["delivery"]["pr_url"], "https://example.invalid/pr/2");
}

#[test]
fn failed_task_summary_is_terminal_with_step_errors() {
    let f = Fixture::new();
    let oid = f.submit("delivered", &enabled_delivery());
    f.finish_step(
        &oid,
        "plan",
        "failed",
        &json!({"error":"simulated failure","accepted":false}),
    );
    f.finish_step(&oid, "deliver", "skipped", &Value::Null);
    f.set_status(&oid, "failed");
    let s = f.summary(&oid);
    assert_eq!(s["status"], "failed");
    assert_eq!(s["terminal"], true);
    assert_eq!(s["integrated_head"], Value::Null);
    assert_eq!(s["questions_pending"], 0);
    let plan = &s["steps"][0];
    assert_eq!(plan["name"], "plan");
    assert_eq!(plan["state"], "failed");
    assert_eq!(plan["accepted"], false);
    assert_eq!(plan["error"], "simulated failure");
    assert_eq!(s["delivery"]["outcome"], "skipped");
    assert_eq!(s["delivery_skipped"], "upstream step failed: plan");
}

#[test]
fn failed_delivery_step_reports_its_error() {
    let f = Fixture::new();
    let oid = f.submit("delivered", &enabled_delivery());
    f.finish_step(
        &oid,
        "deliver",
        "failed",
        &json!({"error":"push failed: rejected"}),
    );
    let s = f.summary(&oid);
    assert_eq!(s["delivery"]["outcome"], "failed");
    assert_eq!(s["delivery"]["reason"], "push failed: rejected");
    assert!(s.get("delivery_skipped").is_none());
}

#[test]
fn long_step_results_are_truncated() {
    let f = Fixture::new();
    let oid = f.submit("simulated", &Settings::default());
    f.finish_step(
        &oid,
        "plan",
        "succeeded",
        &json!({"result":"x".repeat(1000),"accepted":true}),
    );
    let s = f.summary(&oid);
    assert_eq!(
        s["steps"][0]["result"].as_str().unwrap().chars().count(),
        400
    );
}

#[test]
fn integrated_head_reads_the_integrated_worktree() {
    let f = Fixture::new();
    let oid = f.submit("simulated", &Settings::default());
    let workspace = git::task_workspace(&f.db, &oid).unwrap();
    let head = git::run(&workspace, &["rev-parse", "HEAD"]).unwrap();
    assert_eq!(f.summary(&oid)["integrated_head"], head);
}

#[test]
fn summary_is_admin_only_and_published() {
    let f = Fixture::new();
    let oid = f.submit("simulated", &Settings::default());
    assert!(
        protocol::OPERATIONS
            .iter()
            .any(|(name, _)| *name == "summary")
    );
    assert!(!protocol::worker_allowed("summary"));
    assert_eq!(
        protocol::admin_schema("summary")["properties"]["task"]["type"],
        "string"
    );
    let worker = f.db.register(&oid, None).unwrap();
    let denied = protocol::dispatch(
        &f.db,
        "summary",
        json!({}),
        Some(worker["token"].as_str().unwrap()),
    );
    assert!(denied.is_err());
    assert!(protocol::dispatch(&f.db, "summary", json!({"task":"missing"}), None).is_err());
}
