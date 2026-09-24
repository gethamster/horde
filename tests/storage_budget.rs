use horde::{budget, config::Settings, git, store::Store, template};
use serde_json::json;
use std::{collections::BTreeMap, path::Path};

struct Fixture {
    _dir: tempfile::TempDir,
    db: Store,
    task: String,
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
            BTreeMap::from([("task".into(), "budget".into())]),
        )
        .unwrap();
        let task = db
            .submit("budget", &repo, &Settings::default(), &plan)
            .unwrap();
        let step = db.steps(&task).unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let w = db.register(&task, Some(&step)).unwrap();
        let worker = w["id"].as_str().unwrap().to_owned();
        git::allocate(&db, &task, &worker).unwrap();
        db.claim(&task, &worker, &[".".into()]).unwrap();
        db.conn
            .execute("UPDATE steps SET state='running' WHERE id=?", [&step])
            .unwrap();
        db.conn.execute("INSERT INTO attempts(id,step,worker,state,started) VALUES('attempt',?,?,'running',?)",rusqlite::params![step,worker,horde::store::now()]).unwrap();
        Self {
            _dir: dir,
            db,
            task,
        }
    }
}

#[test]
fn status_excludes_only_paused_time_since_actual_progress() {
    let f = Fixture::new();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    for (kind, at, extra) in [
        ("step.budget_started", now - 10_000, json!({"budget_s": 10})),
        ("storage.paused", now - 9_000, json!({})),
        ("storage.resumed", now - 5_000, json!({})),
        ("step.progress", now - 4_000, json!({})),
        ("storage.paused", now - 3_000, json!({})),
    ] {
        let mut data = extra;
        data["attempt"] = json!("attempt");
        data["at_ms"] = json!(at);
        f.db.event(&f.task, kind, data).unwrap();
    }
    // Exercise an ongoing hold after wall time has advanced, independent of
    // whether recording the fixture happens to be fast on this machine.
    std::thread::sleep(std::time::Duration::from_millis(150));
    let timing = budget::status(&f.db, "attempt").unwrap();
    assert!(
        (timing["idle_s"].as_f64().unwrap() - 1.0).abs() < 0.1,
        "{timing}"
    );
    // The hold remains open while the fixture writes events. That time must
    // count as paused too, regardless of how long the filesystem takes.
    let elapsed = timing["elapsed_s"].as_f64().unwrap();
    let paused = timing["paused_s"].as_f64().unwrap();
    assert!(paused >= 7.0, "{timing}");
    assert!((elapsed - paused - 3.0).abs() < 0.001, "{timing}");
    assert!(
        (timing["remaining_s"].as_f64().unwrap() - 9.0).abs() < 0.1,
        "{timing}"
    );
}
