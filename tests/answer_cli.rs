//! `horde answer` is the documented way to answer a worker's question, so it must
//! be able to answer a `human_only` question once the operator attests that a
//! person gave the answer, and never without that attestation.
use horde::{config::Settings, delegation, store::Store, template};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};
const BIN: &str = env!("CARGO_BIN_EXE_horde");

struct Fixture {
    daemon: Child,
    dir: tempfile::TempDir,
    root: PathBuf,
    task: String,
    worker_token: String,
}
impl Fixture {
    fn new() -> Self {
        // macOS Unix socket paths are limited to 104 bytes.
        let dir = tempfile::Builder::new()
            .prefix("answer-cli-")
            .tempdir_in("/tmp")
            .unwrap();
        let root = dir.path().join("data");
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
        let db = Store::open(&root).unwrap();
        let plan = template::compile(
            "simulated",
            &template::load_templates(&repo).unwrap(),
            BTreeMap::from([("task".into(), "export the customer report".into())]),
        )
        .unwrap();
        let task = db
            .submit(
                "export the customer report",
                &repo,
                &Settings::default(),
                &plan,
            )
            .unwrap();
        // The scheduler only runs running tasks, so pausing this one keeps every
        // executor idle while the question waits. Nothing leaves the temporary directory.
        db.conn
            .execute("UPDATE tasks SET status='paused' WHERE id=?", [&task])
            .unwrap();
        let worker = db.register(&task, None).unwrap();
        let worker_token = worker["token"].as_str().unwrap().to_owned();
        drop(db);
        let daemon = command(dir.path(), &root)
            .arg("daemon")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let f = Self {
            daemon,
            dir,
            root,
            task,
            worker_token,
        };
        let start = Instant::now();
        while !f.horde(&["call", "list_tasks"], None).status.success() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "daemon startup timeout"
            );
            std::thread::sleep(Duration::from_millis(30));
        }
        f
    }
    /// Runs the CLI as the operator, or as the worker when a token is given.
    fn horde(&self, args: &[&str], token: Option<&str>) -> Output {
        let mut command = command(self.dir.path(), &self.root);
        command.args(args);
        if let Some(token) = token {
            command.env("HORDE_WORKER_TOKEN", token);
        }
        command.output().unwrap()
    }
    fn call(&self, method: &str, args: Value) -> Value {
        parse(&self.horde(&["call", method, &args.to_string()], None))
    }
    fn pending(&self) -> Vec<Value> {
        let pending = self.call("pending_questions", json!({"task":self.task}));
        pending
            .as_array()
            .unwrap()
            .iter()
            .map(|route| route["question"].clone())
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn command(dir: &Path, root: &Path) -> Command {
    let mut command = Command::new(BIN);
    command
        .arg("--data-dir")
        .arg(root)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env_remove("HORDE_WORKER_TOKEN")
        .env_remove("HORDE_BOOTSTRAP_JSON")
        .env_remove("HORDE_ENROLLMENT_FILE")
        .env_remove("HORDE_ENROLLMENT_JSON")
        .stdin(Stdio::null());
    command
}

fn parse(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn answer_requires_the_human_flag_for_a_human_only_question() {
    let f = Fixture::new();
    let request = json!({
        "id": "email-export",
        "question": "May the export include customer email addresses?",
        "evidence": "The request names customers but not their contact details",
        "human_only": true,
    });
    let asked = f.horde(
        &["call", "request_question", &request.to_string()],
        Some(&f.worker_token),
    );
    let question = parse(&asked)["id"].as_str().unwrap().to_owned();
    assert_eq!(f.pending(), vec![json!(question)]);
    let answer = "No, leave email addresses out";

    let refused = f.horde(&["answer", &f.task, &question, answer], None);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "question {question} is marked human_only; if a person gave this answer, rerun with --human"
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains(delegation::HUMAN_ATTESTATION_REQUIRED),
        "{stderr}"
    );
    assert_eq!(f.pending(), vec![json!(question)]);

    // The flag is an operator attestation; a worker credential still cannot answer.
    let worker = f.horde(
        &["answer", &f.task, &question, answer, "--human"],
        Some(&f.worker_token),
    );
    let stderr = String::from_utf8_lossy(&worker.stderr);
    assert!(!worker.status.success(), "{stderr}");
    assert!(
        stderr.contains("external caller must answer this question"),
        "{stderr}"
    );
    assert!(!stderr.contains("rerun with --human"), "{stderr}");
    assert_eq!(f.pending(), vec![json!(question)]);

    let accepted = f.horde(&["answer", &f.task, &question, answer, "--human"], None);
    assert_eq!(parse(&accepted)["answered"], true);
    assert!(f.pending().is_empty());
    let inspected = f.call("inspect", json!({"task":f.task}));
    let recorded = inspected["questions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == json!(question))
        .unwrap();
    assert_eq!(recorded["answer"], answer);
    let context = f.call("read_context", json!({"task":f.task}));
    let records = context["records"].as_array().unwrap();
    let record = records
        .iter()
        .find(|record| record["kind"] == "answer")
        .unwrap();
    assert!(record["content"].as_str().unwrap().contains(answer));
    let provenance: Value = serde_json::from_str(record["provenance"].as_str().unwrap()).unwrap();
    assert_eq!(provenance["author"], "external caller");
}
