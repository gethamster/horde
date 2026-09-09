use horde::{
    budget,
    config::Settings,
    git, native, protocol,
    store::Store,
    template::{self, Step},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};

struct Fixture {
    _dir: tempfile::TempDir,
    db: Store,
    task: String,
    step: String,
    worker: String,
    token: String,
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
        let token = w["token"].as_str().unwrap().to_owned();
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
            step,
            worker,
            token,
        }
    }
    async fn run(
        &self,
        work: impl std::future::Future<Output = anyhow::Result<Value>>,
    ) -> anyhow::Result<Value> {
        budget::supervise(
            &self.db,
            &self.task,
            &self.step,
            "attempt",
            &self.worker,
            1,
            work,
        )
        .await
    }
    fn events(&self, kind: &str) -> Vec<Value> {
        self.db
            .rows(
                "SELECT data FROM events WHERE task=? AND kind=? ORDER BY seq",
                &[&self.task, &kind],
            )
            .unwrap()
            .into_iter()
            .map(|v| serde_json::from_str(v["data"].as_str().unwrap()).unwrap())
            .collect()
    }
}
#[test]
fn defaults_and_overrides_are_global_then_role_then_step() {
    let mut settings = Settings::default();
    let mut step: Step = serde_json::from_value(json!({"id":"work"})).unwrap();
    assert_eq!(budget::seconds(&settings, &step, "planner"), 600);
    assert_eq!(budget::seconds(&settings, &step, "reviewer"), 600);
    assert_eq!(budget::seconds(&settings, &step, "worker"), 1800);
    settings.step_budget_seconds = 42;
    assert_eq!(budget::seconds(&settings, &step, "worker"), 42);
    settings
        .executors
        .get_mut("worker")
        .unwrap()
        .step_budget_seconds = Some(8);
    assert_eq!(budget::seconds(&settings, &step, "worker"), 8);
    step.step_budget_seconds = Some(3);
    assert_eq!(budget::seconds(&settings, &step, "worker"), 3);
    let dir = tempfile::tempdir().unwrap();
    for text in [
        "step_budget_seconds = 0",
        "[executors.worker]\nstep_budget_seconds = 0",
    ] {
        std::fs::write(dir.path().join("config.toml"), text).unwrap();
        assert!(Settings::load_dir(dir.path()).is_err());
    }
    assert!(
        template::validate(&[
            serde_json::from_value(json!({"id":"bad","step_budget_seconds":0})).unwrap()
        ])
        .is_err()
    );
}
#[tokio::test(flavor = "current_thread")]
async fn valid_nonidentical_coordination_calls_cannot_extend_the_budget() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let start = Instant::now();
            let result = f
                .run(async {
                    for n in 0..100 {
                        protocol::dispatch(
                            &f.db,
                            "read_context",
                            json!({"after":n}),
                            Some(&f.token),
                        )?;
                        protocol::dispatch(
                            &f.db,
                            "set_worker_status",
                            json!({"status":if n%2==0 {"working"} else {"idle"}}),
                            Some(&f.token),
                        )?;
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    Ok(json!({"result":"never","accepted":true}))
                })
                .await;
            let error = result.unwrap_err();
            assert!(error.is::<budget::Exhausted>());
            assert!(start.elapsed() < Duration::from_secs(3));
            f.db.finish(&f.step, "attempt", &f.worker, Err(error))
                .unwrap();
            let inspect =
                protocol::dispatch(&f.db, "inspect", json!({"task":f.task}), None).unwrap();
            assert_eq!(inspect["attempts"][0]["state"], "failed");
            let result: Value =
                serde_json::from_str(inspect["attempts"][0]["result"].as_str().unwrap()).unwrap();
            assert_eq!(result["error"], "step budget exhausted");
            assert_eq!(result["budget_s"], 1);
            assert!(result["elapsed_s"].as_f64().unwrap() >= 1.0);
            assert_eq!(inspect["attempts"][0]["timing"]["remaining_s"], 0.0);
            assert_eq!(f.events("step.budget_exhausted").len(), 1);
            assert!(f.events("step.progress").is_empty());
            let metrics = horde::metrics::report(&f.db, &f.task).unwrap();
            assert_eq!(
                metrics["steps"][0]["attempts"][0]["timing"],
                inspect["attempts"][0]["timing"]
            );
            assert!(
                metrics["steps"][0]["attempt_elapsed_seconds"]
                    .as_f64()
                    .unwrap()
                    >= 1.0
            );
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn changed_native_writes_renew_the_window_but_identical_writes_do_not() {
    tokio::task::LocalSet::new().run_until(async {
        for changing in [true,false] {
            let f=Fixture::new();
            let result=f.run(async {
                for n in 0..5 {
                    native::call(&f.db,&f.worker,"write_file",&json!({"path":"result.txt","content":if changing {n.to_string()}else{"same".into()}}),&Settings::default(),&["write_file".into()]).await?;
                    tokio::time::sleep(Duration::from_millis(350)).await;
                }
                Ok(json!({"result":"changed","accepted":true}))
            }).await;
            assert_eq!(result.is_ok(),changing,"{result:?}");
            let writes=f.events("step.progress").into_iter().filter(|e|e["reason"].as_str().unwrap().starts_with("file_write")).count();
            assert_eq!(writes,if changing {5}else{1});
        }
    }).await;
}
#[tokio::test(flavor = "current_thread")]
async fn duplicate_artifacts_do_not_renew_and_real_proposals_do() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let result = f
                .run(async {
                    for n in 0..8 {
                        protocol::dispatch(
                            &f.db,
                            "put_artifact",
                            json!({"name":format!("name-{n}"),"content":"same evidence"}),
                            Some(&f.token),
                        )?;
                        tokio::time::sleep(Duration::from_millis(300)).await;
                    }
                    Ok(json!({"accepted":true,"result":"never"}))
                })
                .await;
            assert!(result.unwrap_err().is::<budget::Exhausted>());
            assert_eq!(
                f.events("step.progress")
                    .iter()
                    .filter(|e| e["reason"] == "artifact_committed")
                    .count(),
                1
            );
            let f = Fixture::new();
            let mut spec = Store::step(&f.db.steps(&f.task).unwrap()[0]).unwrap();
            spec.role = "planner".into();
            f.db.conn
                .execute(
                    "UPDATE steps SET spec=? WHERE id=?",
                    rusqlite::params![serde_json::to_string(&spec).unwrap(), f.step],
                )
                .unwrap();
            let result = f
                .run(async {
                    tokio::time::sleep(Duration::from_millis(600)).await;
                    protocol::dispatch(
                        &f.db,
                        "propose_steps",
                        json!({"steps":[{"id":"implementation","scope":["src"]}]}),
                        Some(&f.token),
                    )?;
                    tokio::time::sleep(Duration::from_millis(600)).await;
                    Ok(json!({"accepted":true,"result":"planned"}))
                })
                .await;
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(f.events("step.progress").len(), 1);
        })
        .await;
}
#[tokio::test(flavor = "current_thread")]
async fn command_writes_are_observed_and_expired_commands_are_stopped() {
    tokio::task::LocalSet::new().run_until(async {
        let f=Fixture::new();
        let result=f.run(async {
            native::call(&f.db,&f.worker,"command",&json!({"argv":["sh","-c","sleep 0.4; echo one > a; sleep 0.4; echo two > a; sleep 0.4; echo three > a; sleep 0.4"]}),&Settings::default(),&["command".into()]).await?;
            Ok(json!({"accepted":true,"result":"done"}))
        }).await;
        assert!(result.is_ok(),"{result:?}");assert!(!f.events("step.progress").is_empty());
        let f=Fixture::new();
        let result=f.run(async {
            native::call(&f.db,&f.worker,"command",&json!({"argv":["sh","-c","sleep 30"]}),&Settings::default(),&["command".into()]).await
        }).await;
        assert!(result.unwrap_err().is::<budget::Exhausted>());
        let pid:i32=f.db.conn.query_row("SELECT pid FROM attempts WHERE id='attempt'",[],|r|r.get(0)).unwrap();
        for _ in 0..50 {if !horde::executor::process_alive(pid) {return;} tokio::time::sleep(Duration::from_millis(20)).await;}
        panic!("expired process survived: {pid}");
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn slow_blocking_git_is_cancelled_without_stalling_the_daemon_timer() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let path = f._dir.path().join("hook-pid");
            let marker = path.clone();
            let started = Instant::now();
            let result = f
                .run(async move {
                    budget::blocking(move || {
                        let mut command = std::process::Command::new("sh");
                        command
                            .args(["-c", "echo $$ > \"$1\"; sleep 30", "hook"])
                            .arg(marker);
                        budget::command_output(&mut command)?;
                        Ok(json!({"accepted":true}))
                    })
                    .await
                })
                .await;
            assert!(result.unwrap_err().is::<budget::Exhausted>());
            assert!(started.elapsed() < Duration::from_secs(3));
            let pid: i32 = std::fs::read_to_string(path)
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            for _ in 0..50 {
                if unsafe { libc::kill(pid, 0) } != 0 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            panic!("expired blocking command is still alive");
        })
        .await;
}
