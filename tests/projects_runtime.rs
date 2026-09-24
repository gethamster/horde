//! Native daemon acceptance without credentials, provider requests, or virtualization.
use horde::{daemon_client, store::Store};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Daemon {
    child: Child,
    root: PathBuf,
    _directory: tempfile::TempDir,
}
impl Drop for Daemon {
    fn drop(&mut self) {
        support::stop_daemon(&mut self.child, &self.root);
    }
}
mod support;
impl Daemon {
    fn new() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("horde-project-")
            .tempdir_in("/tmp")
            .unwrap();
        let root = directory.path().join("data");
        let config = directory.path().join("config/horde");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("config.toml"), "concurrency = 2\n").unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_horde"))
            .arg("--data-dir")
            .arg(&root)
            .arg("daemon")
            .env("XDG_CONFIG_HOME", directory.path().join("config"))
            .env_remove("HORDE_WORKER_TOKEN")
            .env_remove("CODEHORDE_WORKER_TOKEN")
            .env_remove("HORDE_BOOTSTRAP_JSON")
            .env_remove("HORDE_ENROLLMENT_JSON")
            .env_remove("HORDE_ENROLLMENT_FILE")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let daemon = Self {
            child,
            root,
            _directory: directory,
        };
        let started = Instant::now();
        while !daemon_client::running(&daemon.root) {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "daemon startup timeout"
            );
            std::thread::sleep(Duration::from_millis(30));
        }
        daemon
    }
    fn call(&self, method: &str, args: Value) -> Value {
        daemon_client::request(&self.root, method, args).unwrap()
    }
    fn submit(&self, slug: &str) -> (String, String) {
        self.submit_with_delay(slug, 1)
    }
    fn submit_with_delay(&self, slug: &str, seconds: u64) -> (String, String) {
        let project = self.call("project_create", json!({"slug":slug,"concurrency":1}))["id"]
            .as_str()
            .unwrap()
            .to_owned();
        self.call(
            "project_runtime_grant",
            json!({"project":project,"runtime":"local"}),
        );
        let repo = self._directory.path().join(slug);
        std::fs::create_dir_all(repo.join(".horde/templates")).unwrap();
        std::fs::write(
            repo.join(".horde/templates/project-test.toml"),
            format!(
                r#"name = "project-test"
version = "1.0.0"
inputs = ["task"]
[[steps]]
id = "plan"
kind = "simulated"
[[steps]]
id = "write"
kind = "command"
needs = ["plan"]
command = ["/bin/sh", "-c", "sleep {seconds}; printf '{slug}' > result.txt; pwd"]
"#
            ),
        )
        .unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@localhost"],
            vec!["add", "."],
            vec!["commit", "-m", "initial"],
        ] {
            horde::git::run(&repo, &args).unwrap();
        }
        self.call("project_repo_add", json!({"project":project,"path":repo}));
        let task=self.call("submit_task",json!({"project":project,"objective":format!("build {slug}"),"repo":repo,"template":"project-test"}))["id"].as_str().unwrap().to_owned();
        (project, task)
    }
}

#[test]
fn disk_pressure_queues_work_and_recovers_without_a_restart() {
    let daemon = Daemon::new();
    daemon.call(
        "runtime_storage_configure",
        json!({"min_free_bytes":1024_u64.pow(5)}),
    );
    let (_project, task) = daemon.submit("storage-pressure");
    let started = Instant::now();
    loop {
        let db = Store::open(&daemon.root).unwrap();
        let reasons = db
            .rows("SELECT reason FROM project_queue WHERE task=?", &[&task])
            .unwrap();
        if reasons
            .first()
            .is_some_and(|row| row["reason"].as_str().unwrap_or("").contains("disk space"))
        {
            assert!(
                db.rows(
                    "SELECT a.id FROM attempts a JOIN steps s ON s.id=a.step WHERE s.task=?",
                    &[&task]
                )
                .unwrap()
                .is_empty()
            );
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "task did not queue for disk space"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(
        daemon.call("runtime_storage_status", json!({}))["pressure"],
        true
    );
    daemon.call(
        "runtime_storage_configure",
        json!({"min_free_bytes":1048576}),
    );
    let started = Instant::now();
    loop {
        let db = Store::open(&daemon.root).unwrap();
        if db.task(&task).unwrap()["status"] == "succeeded" {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "task did not resume"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn two_projects_share_native_daemon_and_keep_results_and_workspaces_separate() {
    let daemon = Daemon::new();
    let (horde, horde_task) = daemon.submit("horde");
    let (hamster, hamster_task) = daemon.submit("hamster");
    let started = Instant::now();
    let mut overlapping = false;
    loop {
        let db = Store::open(&daemon.root).unwrap();
        let active:i64=db.conn.query_row("SELECT COUNT(DISTINCT p.project) FROM attempts a JOIN steps s ON s.id=a.step JOIN task_projects p ON p.task=s.task WHERE a.state='running'",[],|r|r.get(0)).unwrap();
        overlapping |= active == 2;
        let completed: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM tasks WHERE status='succeeded' AND id IN (?,?)",
                [&horde_task, &hamster_task],
                |r| r.get(0),
            )
            .unwrap();
        if completed == 2 {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "tasks did not complete: {:?}",
            db.rows("SELECT id,status FROM tasks", &[]).unwrap()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        overlapping,
        "both projects should receive native execution slots"
    );
    let db = Store::open(&daemon.root).unwrap();
    for (project, task, expected, other) in [
        (&horde, &horde_task, "horde", &hamster_task),
        (&hamster, &hamster_task, "hamster", &horde_task),
    ] {
        let scoped =
            daemon_client::request_scoped(&daemon.root, "list_tasks", json!({}), Some(project))
                .unwrap();
        assert_eq!(scoped.as_array().unwrap().len(), 1);
        assert_eq!(scoped[0]["id"], *task);
        assert!(
            daemon_client::request_scoped(
                &daemon.root,
                "inspect",
                json!({"task":other}),
                Some(project)
            )
            .is_err()
        );
        let workspace = horde::git::task_workspace(&db, task).unwrap();
        assert!(workspace.starts_with(daemon.root.join("projects").join(project)));
        assert_eq!(
            std::fs::read_to_string(workspace.join("result.txt")).unwrap(),
            expected
        );
        let inspection = daemon_client::request_scoped(
            &daemon.root,
            "inspect",
            json!({"task":task}),
            Some(project),
        )
        .unwrap();
        let bindings = inspection["project_runtime"]["bindings"]
            .as_array()
            .unwrap();
        assert_eq!(bindings.len(), 2);
        assert!(bindings.iter().all(|b| b["project"] == *project
            && b["isolation"] == "native"
            && b["account"].is_null()));
    }
}

#[test]
fn active_disk_pressure_requests_cleanup_and_resumes_the_same_attempt() {
    let daemon = Daemon::new();
    let (_, task) = daemon.submit_with_delay("active-storage", 4);
    let db = Store::open(&daemon.root).unwrap();
    let started = Instant::now();
    let attempt = loop {
        let rows = db.rows("SELECT a.* FROM attempts a JOIN steps s ON s.id=a.step WHERE s.task=? AND s.name='write' AND a.pid IS NOT NULL AND a.state='running'", &[&task]).unwrap();
        if let Some(row) = rows.first() {
            break row.clone();
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "command did not start"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    daemon.call("runtime_storage_configure",json!({"min_free_bytes":1024_u64.pow(5),"cleanup_command":["/bin/sh","-c","echo cleaned >> cleanup-hook-runs"]}));
    let started = Instant::now();
    loop {
        let held = db
            .rows(
                "SELECT seq FROM events WHERE task=? AND kind='storage.paused'",
                &[&task],
            )
            .unwrap();
        if !held.is_empty() {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "attempt did not pause"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(1300));
    let id = attempt["id"].as_str().unwrap();
    let active = db
        .rows("SELECT * FROM attempts WHERE id=?", &[&id])
        .unwrap();
    assert_eq!(active[0]["state"], "running");
    assert_eq!(active[0]["pid"], attempt["pid"]);
    assert!(horde::project_runtime::host_active(&db).unwrap() >= 1);
    let requests = db
        .rows(
            "SELECT id FROM messages WHERE task=? AND id LIKE 'storage-cleanup:%'",
            &[&task],
        )
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        std::fs::read_to_string(daemon.root.join("cleanup-hook-runs")).unwrap(),
        "cleaned\n"
    );
    assert_eq!(
        daemon.call("runtime_storage_status", json!({}))["paused"],
        true
    );
    daemon.call(
        "runtime_storage_configure",
        json!({"min_free_bytes":1048576}),
    );
    let started = Instant::now();
    loop {
        if db.task(&task).unwrap()["status"] == "succeeded" {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "held attempt did not resume"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
    assert_eq!(
        db.rows(
            "SELECT id FROM attempts WHERE step=?",
            &[&attempt["step"].as_str().unwrap()]
        )
        .unwrap()
        .len(),
        1
    );
    assert_eq!(
        db.rows(
            "SELECT seq FROM events WHERE task=? AND kind='storage.resumed'",
            &[&task]
        )
        .unwrap()
        .len(),
        1
    );
    assert!(
        daemon.call("runtime_storage_status", json!({}))["process_holds"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
