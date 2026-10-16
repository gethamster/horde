//! Host pressure transitions and cooperative recovery, separate from workflow state.
use super::{Policy, control};
use crate::{management, store::Store};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{future::Future, time::Duration};

pub(super) fn next(previous: &str, free: Option<u64>, policy: &Policy) -> &'static str {
    let Some(free) = free else {
        return "paused";
    };
    if free < policy.min_free_bytes {
        return "paused";
    }
    if free >= policy.resume() {
        return "healthy";
    }
    if previous == "paused" {
        return "paused";
    }
    if free < policy.warning() || previous == "cleanup_requested" {
        return "cleanup_requested";
    }
    "healthy"
}
pub(super) fn saved(db: &Store) -> Result<Value> {
    Ok(management::value(db, "storage.pressure")?
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or(Value::Null))
}
fn millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
/// Persist transitions only; status itself always takes fresh filesystem measurements.
fn refresh(db: &Store) -> Result<Value> {
    let report = super::status(db)?;
    let previous = saved(db)?;
    if previous["state"] == report["state"] {
        if std::fs::read(db.root.join("storage-pressure.json"))
            .ok()
            .as_deref()
            != Some(previous.to_string().as_bytes())
        {
            write_notice(db, &previous)?;
        }
        return Ok(previous);
    }
    let incident = if previous["state"] == "healthy" || previous.is_null() {
        crate::store::id()
    } else {
        previous["id"]
            .as_str()
            .context("pressure incident")?
            .to_owned()
    };
    let state = json!({"id":incident,"state":report["state"],"at_ms":millis(),"volumes":report["volumes"],"warning_free_bytes":report["warning_free_bytes"],"resume_free_bytes":report["resume_free_bytes"]});
    db.atomic(|| {
        management::set(db, "storage.pressure", &state.to_string())?;
        management::event(db, "storage.health_changed", state.clone())
    })?;
    write_notice(db, &state)?;
    Ok(state)
}
fn write_notice(db: &Store, state: &Value) -> Result<()> {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new_in(&db.root)?;
    file.write_all(state.to_string().as_bytes())?;
    file.persist(db.root.join("storage-pressure.json"))?;
    Ok(())
}
fn notify(db: &Store, task: &str, attempt: &str, worker: &str, incident: &Value) -> Result<()> {
    if incident["state"] == "healthy" {
        return Ok(());
    }
    let id = incident["id"].as_str().context("pressure incident")?;
    let message = format!("storage-cleanup:{id}:{attempt}");
    if !db
        .rows("SELECT id FROM messages WHERE id=?", &[&message])?
        .is_empty()
    {
        return Ok(());
    }
    db.steer(task, &message,
        "This host is short of disk space. Stop starting large downloads. If you can safely reclaim your own disposable caches, do so; preserve source changes and recovery artifacts. Critical pressure suspends owned processes until the host has recovery headroom. Check HORDE_STORAGE_PRESSURE_FILE for host state. An operator may clean or expand storage; do not retry the attempt to escape this hold.",
        &json!([]), false, Some(worker))?;
    Ok(())
}

/// Scope the entire budget supervisor so timeouts, commands and tools share the hold.
pub(crate) async fn supervise<F: Future<Output = Result<Value>>>(
    db: &Store,
    task: &str,
    attempt: &str,
    worker: &str,
    pressure: tokio::sync::watch::Receiver<Value>,
    work: F,
) -> Result<Value> {
    let control = control::Control::new(&db.root, attempt);
    control::scope(control.clone(), async {
        control.set_paused(pressure.has_changed().is_err() || pressure.borrow()["state"] == "paused")?;
        let mut ticker = tokio::time::interval(Duration::from_millis(250));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let work = async { control::checkpoint().await; work.await };
        tokio::pin!(work);
        let mut reported_pause = false;
        loop {
            tokio::select! {
                biased;
                _ = ticker.tick() => {
                    let incident = match pressure.has_changed() {
                        Ok(_) => pressure.borrow().clone(),
                        Err(error) => {
                            eprintln!("Storage health unavailable; holding attempt {attempt}: {error:#}");
                            json!({"state":"paused","id":"health-unavailable"})
                        }
                    };
                    if let Err(error) = notify(db, task, attempt, worker, &incident) {
                        eprintln!("Storage cleanup request for {attempt}: {error:#}");
                    }
                    let paused = incident["state"] == "paused";
                    if let Err(error) = control.set_paused(paused) {
                        eprintln!("Storage process hold incomplete for {attempt}; retrying: {error:#}");
                        continue;
                    }
                    if paused != reported_pause {
                        let event = db.event(task, if paused { "storage.paused" } else { "storage.resumed" }, json!({"attempt":attempt,"worker":worker,"at_ms":millis(),"incident":incident["id"]}));
                        match event {
                            Ok(()) => reported_pause = paused,
                            Err(error) => eprintln!("Storage pause receipt for {attempt}: {error:#}"),
                        }
                    }
                }
                result = &mut work => return result,
            }
        }
    }).await
}

struct Hook(Option<tokio::task::JoinHandle<()>>);
impl Drop for Hook {
    fn drop(&mut self) {
        if let Some(handle) = &self.0 {
            handle.abort();
        }
    }
}
fn claim_hook(db: &Store, incident: &Value, policy: &Policy) -> Result<bool> {
    if incident["state"] == "healthy" || policy.cleanup_command.is_empty() {
        return Ok(false);
    }
    db.atomic(|| {
        let last = management::value(db, "storage.cleanup_hook")?
            .map(|s| serde_json::from_str::<Value>(&s))
            .transpose()?;
        if last
            .as_ref()
            .is_some_and(|last| last["incident"] == incident["id"])
        {
            return Ok(false);
        }
        management::set(
            db,
            "storage.cleanup_hook",
            &json!({"incident":incident["id"],"state":"started","at_ms":millis()}).to_string(),
        )?;
        Ok(true)
    })
}
async fn cleanup_hook(db: &Store, policy: Policy, incident: Value) -> Result<()> {
    let mut command = crate::executor::clean_command(&policy.cleanup_command[0]);
    command
        .args(&policy.cleanup_command[1..])
        .current_dir(&db.root)
        .env(
            "HORDE_STORAGE_PRESSURE_FILE",
            db.root.join("storage-pressure.json"),
        );
    let outcome =
        crate::executor::run_process(command, None, policy.cleanup_timeout_seconds, None).await;
    let record = match outcome {
        Ok(result) => {
            json!({"incident":incident["id"],"state":"finished","success":result["success"],"exit_code":result["exit_code"],"at_ms":millis()})
        }
        Err(_) => {
            json!({"incident":incident["id"],"state":"failed","success":false,"at_ms":millis()})
        }
    };
    // Arbitrary hook output can contain secrets; retain outcome metadata only.
    management::set(db, "storage.cleanup_hook", &record.to_string())?;
    management::event(db, "storage.cleanup_hook_finished", record)
}
/// The cleanup lane remains runnable while worker process groups are suspended.
pub(crate) async fn monitor(root: std::path::PathBuf, pressure: tokio::sync::watch::Sender<Value>) {
    let mut hook = Hook(None);
    loop {
        let sample_root = root.clone();
        let hook_ready = hook.0.as_ref().is_none_or(|handle| handle.is_finished());
        let result = tokio::task::spawn_blocking(move || -> Result<_> {
            let db = Store::open(&sample_root)?;
            let incident = refresh(&db)?;
            let policy = super::policy(&db)?;
            let claimed = hook_ready && claim_hook(&db, &incident, &policy)?;
            Ok((incident, policy, claimed))
        })
        .await;
        match result {
            Ok(Ok((incident, policy, claimed))) => {
                pressure.send_replace(incident.clone());
                if claimed {
                    let root = root.clone();
                    hook.0 = Some(tokio::task::spawn_local(async move {
                        let result =
                            async { cleanup_hook(&Store::open(&root)?, policy, incident).await }
                                .await;
                        if let Err(error) = result {
                            eprintln!("Storage cleanup hook: {error:#}");
                        }
                    }));
                }
            }
            other => {
                pressure.send_replace(json!({"state":"paused","id":"health-unavailable"}));
                eprintln!("Storage pressure monitor: {other:?}");
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Store, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let db = Store::open_with_config_dir(dir.path(), &dir.path().join("config")).unwrap();
        let plan = crate::template::compile(
            "simulated",
            &crate::template::load_templates(dir.path()).unwrap(),
            std::collections::BTreeMap::from([("task".into(), "pressure".into())]),
        )
        .unwrap();
        let task = db
            .submit(
                "pressure",
                dir.path(),
                &crate::config::Settings::default(),
                &plan,
            )
            .unwrap();
        let worker = db.register(&task, None).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        (dir, db, task, worker)
    }

    #[tokio::test]
    async fn shared_pressure_holds_and_resumes_owned_work_and_fails_closed() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (dir, db, task, worker) = fixture();
                let counter = dir.path().join("counter");
                let size = || std::fs::metadata(&counter).map(|m| m.len()).unwrap_or(0);
                let (sender, receiver) = tokio::sync::watch::channel(
                    json!({"state":"paused","id":"health-unavailable"}),
                );
                let root = dir.path().to_owned();
                let running = tokio::task::spawn_local(async move {
                    supervise(&db, &task, "shared-pressure", &worker, receiver, async {
                        let mut command = crate::executor::clean_command("sh");
                        command
                            .current_dir(root)
                            .args(["-c", "while :; do printf x >> counter; sleep 0.01; done"]);
                        crate::executor::run_process(command, None, 30, None).await
                    })
                    .await
                });
                tokio::time::sleep(Duration::from_millis(300)).await;
                assert_eq!(size(), 0, "unknown initial health must not launch work");
                for _ in 0..2 {
                    sender.send_replace(json!({"state":"healthy","id":"sample"}));
                    let before = size();
                    tokio::time::timeout(Duration::from_secs(3), async {
                        while size() <= before {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    })
                    .await
                    .unwrap();
                    sender.send_replace(json!({"state":"paused","id":"pressure"}));
                    tokio::time::sleep(Duration::from_millis(350)).await;
                    let stopped = size();
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    assert_eq!(
                        size(),
                        stopped,
                        "the existing process group must stop on shared pressure"
                    );
                }
                sender.send_replace(json!({"state":"healthy","id":"sample"}));
                tokio::time::sleep(Duration::from_millis(300)).await;
                drop(sender);
                tokio::time::sleep(Duration::from_millis(350)).await;
                let stopped = size();
                tokio::time::sleep(Duration::from_millis(150)).await;
                assert_eq!(
                    size(),
                    stopped,
                    "a closed monitor must not retain its last healthy sample"
                );
                running.abort();
                let _ = running.await;
            })
            .await;
    }

    #[tokio::test]
    async fn failed_monitor_measurement_cannot_reuse_persisted_healthy_state() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (dir, db, _, _) = fixture();
                let healthy = refresh(&db).unwrap();
                assert_eq!(healthy["state"], "healthy");
                management::set(&db, "storage.policy", "invalid JSON").unwrap();
                let (sender, mut receiver) = tokio::sync::watch::channel(healthy.clone());
                let monitor = tokio::task::spawn_local(monitor(dir.path().to_owned(), sender));
                tokio::time::timeout(Duration::from_secs(3), receiver.changed())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(receiver.borrow()["state"], "paused");
                assert_eq!(
                    saved(&db).unwrap(),
                    healthy,
                    "the failed probe could not update the old durable state"
                );
                db.conn
                    .execute(
                        "DELETE FROM runtime_settings WHERE key='storage.policy'",
                        [],
                    )
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(3), receiver.changed())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(receiver.borrow()["state"], "healthy");
                monitor.abort();
                let _ = monitor.await;
            })
            .await;
    }

    #[tokio::test]
    async fn thirty_two_http_workers_remain_responsive_during_a_blocked_storage_measurement() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        tokio::task::LocalSet::new().run_until(async {
            let (dir, db, task, worker) = fixture();
            let healthy = refresh(&db).unwrap();
            db.conn.pragma_update(None, "journal_mode", "DELETE").unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/token", listener.local_addr().unwrap());
            let server = tokio::task::spawn_local(async move {
                for _ in 0..32 {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    tokio::task::spawn_local(async move {
                        let mut request = Vec::new();
                        while !request.ends_with(b"\r\n\r\n") {
                            let mut byte = [0];
                            socket.read_exact(&mut byte).await.unwrap();
                            request.push(byte[0]);
                            assert!(request.len() < 4096);
                        }
                        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await.unwrap();
                    });
                }
            });
            let path = dir.path().join("state.sqlite3");
            let (ready, locked) = std::sync::mpsc::channel();
            let blocker = std::thread::spawn(move || {
                let connection = rusqlite::Connection::open(path).unwrap();
                connection.execute_batch("BEGIN EXCLUSIVE").unwrap();
                ready.send(()).unwrap();
                std::thread::sleep(Duration::from_secs(2));
                connection.execute_batch("ROLLBACK").unwrap();
            });
            locked.recv_timeout(Duration::from_secs(3)).unwrap();
            let (sender, receiver) = tokio::sync::watch::channel(healthy);
            let monitor = tokio::task::spawn_local(monitor(dir.path().to_owned(), sender));
            let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(1)).build().unwrap();
            let started = std::time::Instant::now();
            let requests = (0..32).map(|number| {
                let client = &client; let url = &url; let db = &db; let task = &task; let worker = &worker;
                let receiver = receiver.clone();
                async move {
                    supervise(db, task, &format!("http-{number}"), worker, receiver, async {
                        let response = client.get(url).send().await?;
                        anyhow::ensure!(response.status().is_success(), "mock metadata failed");
                        anyhow::ensure!(response.text().await? == "ok", "mock metadata body changed");
                        Ok(json!({"completed":true}))
                    }).await
                }
            });
            let results = tokio::time::timeout(Duration::from_secs(1), futures_util::future::join_all(requests)).await;
            let elapsed = started.elapsed();
            blocker.join().unwrap();
            monitor.abort(); let _ = monitor.await;
            let results = results.expect("blocked storage must not stall current-thread HTTP work");
            assert!(results.iter().all(|result| result.as_ref().is_ok_and(|value| value["completed"] == true)), "{results:?}");
            server.await.unwrap();
            eprintln!("32 supervised HTTP completions while storage is locked: {}ms", elapsed.as_millis());
        }).await;
    }

    #[test]
    fn hysteresis_and_failed_probe_hold_until_healthy() {
        let policy = Policy {
            min_free_bytes: 10,
            warning_free_bytes: Some(20),
            resume_free_bytes: Some(30),
            ..Policy::default()
        };
        assert_eq!(next("healthy", Some(21), &policy), "healthy");
        assert_eq!(next("healthy", Some(19), &policy), "cleanup_requested");
        assert_eq!(
            next("cleanup_requested", Some(21), &policy),
            "cleanup_requested"
        );
        assert_eq!(next("cleanup_requested", Some(9), &policy), "paused");
        assert_eq!(next("paused", Some(29), &policy), "paused");
        assert_eq!(next("paused", Some(30), &policy), "healthy");
        assert_eq!(next("healthy", None, &policy), "paused");
    }
    #[tokio::test]
    async fn cleanup_hook_runs_once_per_incident_and_records_only_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let db = Store::open_with_config_dir(dir.path(), &dir.path().join("config")).unwrap();
        let policy = Policy {
            cleanup_command: vec![
                "/bin/sh".into(),
                "-c".into(),
                "echo secret; echo ran >> hook-calls".into(),
            ],
            ..Policy::default()
        };
        let incident = json!({"id":"one","state":"cleanup_requested"});
        assert!(claim_hook(&db, &incident, &policy).unwrap());
        assert!(!claim_hook(&db, &incident, &policy).unwrap());
        cleanup_hook(&db, policy.clone(), incident.clone())
            .await
            .unwrap();
        assert!(!claim_hook(&db, &incident, &policy).unwrap());
        let record = management::value(&db, "storage.cleanup_hook")
            .unwrap()
            .unwrap();
        assert!(!record.contains("secret"));
        assert!(record.contains("finished"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("hook-calls")).unwrap(),
            "ran\n"
        );
        assert!(claim_hook(&db, &json!({"id":"two","state":"paused"}), &policy).unwrap());
    }
}
