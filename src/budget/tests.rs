use super::*;
use std::{collections::BTreeMap, path::Path, sync::Arc};

struct Fixture {
    _dir: tempfile::TempDir,
    db: Store,
    task: String,
    step: String,
    worker: String,
    control: Arc<pressure::Control>,
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
            crate::git::run(&repo, &args).unwrap();
        }
        let db = Store::open(&dir.path().join("data")).unwrap();
        let plan = crate::template::compile(
            "simulated",
            &crate::template::load_templates(Path::new("absent")).unwrap(),
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
        let worker = db.register(&task, Some(&step)).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        db.conn.execute("INSERT INTO attempts(id,step,worker,state,started) VALUES('attempt',?,?,'running',?)", rusqlite::params![step,worker,crate::store::now()]).unwrap();
        let control = pressure::Control::new(&db.root, "attempt");
        Self {
            _dir: dir,
            db,
            task,
            step,
            worker,
            control,
        }
    }

    async fn run(
        &self,
        budget: Option<u64>,
        work: impl Future<Output = Result<Value>>,
    ) -> Result<Value> {
        pressure::scope(
            self.control.clone(),
            supervise(
                &self.db,
                &self.task,
                &self.step,
                "attempt",
                &self.worker,
                budget,
                work,
            ),
        )
        .await
    }

    async fn pause(&self, duration: Duration) {
        self.control.set_paused(true).unwrap();
        self.db
            .event(
                &self.task,
                "storage.paused",
                json!({"attempt":"attempt","at_ms":millis()}),
            )
            .unwrap();
        tokio::time::sleep(duration).await;
        self.control.set_paused(false).unwrap();
        self.db
            .event(
                &self.task,
                "storage.resumed",
                json!({"attempt":"attempt","at_ms":millis()}),
            )
            .unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn pause_longer_than_budget_preserves_remaining_active_idle() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let result = f
                .run(Some(1), async {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    f.pause(Duration::from_millis(1200)).await;
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    Ok(json!({"ok": true}))
                })
                .await
                .unwrap();
            assert_eq!(result["ok"], true);
            let timing = status(&f.db, "attempt").unwrap();
            assert!(timing["paused_s"].as_f64().unwrap() >= 1.2, "{timing}");
            assert!(timing["idle_s"].as_f64().unwrap() < 1.0, "{timing}");
            let count: u64 =
                f.db.conn
                    .query_row(
                        "SELECT count(*) FROM events WHERE kind='step.progress'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
            assert_eq!(count, 0, "suspension must not fabricate progress");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn resume_does_not_reset_idle_budget() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let result = f
                .run(Some(1), async {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    f.pause(Duration::from_millis(1200)).await;
                    tokio::time::sleep(Duration::from_millis(650)).await;
                    Ok(json!({"ok": true}))
                })
                .await;
            assert!(result.unwrap_err().is::<Exhausted>());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn blocking_command_survives_pause_beyond_its_original_deadline() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let result = f
                .run(Some(1), async {
                    let work = blocking(|| {
                        let output = command_output(
                            std::process::Command::new("sh").args(["-c", "sleep 0.7; printf done"]),
                        )?;
                        anyhow::ensure!(output.status.success(), "command failed");
                        Ok(json!({"output": String::from_utf8_lossy(&output.stdout)}))
                    });
                    let pause = async {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        f.pause(Duration::from_millis(1200)).await;
                    };
                    tokio::join!(work, pause).0
                })
                .await
                .unwrap();
            assert_eq!(result["output"], "done");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_exempt_suspended_blocking_command_kills_it() {
    tokio::task::LocalSet::new().run_until(async {
        let f = Fixture::new();
        let marker = f._dir.path().join("pid");
        let path = marker.clone();
        let work = f.run(None, async {
            blocking(move || {
                let output = command_output(std::process::Command::new("sh").args(["-c", "echo $$ > \"$1\"; sleep 30", "test"]).arg(path))?;
                Ok(json!({"success": output.status.success()}))
            }).await
        });
        let cancel = async {
            while !marker.exists() { tokio::time::sleep(Duration::from_millis(10)).await; }
            f.control.set_paused(true).unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        {
            tokio::pin!(work);
            tokio::select! { result = &mut work => panic!("finished prematurely: {result:?}"), _ = cancel => {} }
        }
        let pid: i32 = std::fs::read_to_string(marker).unwrap().trim().parse().unwrap();
        for _ in 0..100 {
            if unsafe { libc::kill(pid, 0) } != 0 { return; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("suspended command survived cancellation: {pid}");
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn held_blocking_command_waits_before_spawn_and_cancels_without_spawning() {
    tokio::task::LocalSet::new().run_until(async {
        for cancel in [false, true] {
            let f = Fixture::new();
            let marker = f._dir.path().join("started");
            let path = marker.clone();
            f.control.set_paused(true).unwrap();
            let (entered, waiting) = tokio::sync::oneshot::channel();
            let work = f.run(None, async {
                blocking(move || {
                    let _ = entered.send(());
                    let output = command_output(std::process::Command::new("sh").args(["-c", "printf started > \"$1\"", "test"]).arg(path))?;
                    Ok(json!({"success": output.status.success()}))
                }).await
            });
            {
                tokio::pin!(work);
                let check = async {
                    waiting.await.unwrap();
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    assert!(!marker.exists(), "paused command wrote before resume");
                    let count: u64 = f.db.conn.query_row("SELECT count(*) FROM runtime_settings WHERE key LIKE 'storage.pause:%'", [], |r| r.get(0)).unwrap();
                    assert_eq!(count, 0, "paused command spawned before resume");
                };
                tokio::select! { result = &mut work => panic!("held command returned: {result:?}"), _ = check => {} }
                if !cancel {
                    f.control.set_paused(false).unwrap();
                    assert_eq!(work.await.unwrap()["success"], true);
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(marker.exists(), !cancel);
            f.control.set_paused(false).unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(marker.exists(), !cancel, "cancelled command spawned on resume");
        }
    }).await;
}
