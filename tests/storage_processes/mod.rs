use super::*;
use std::{os::unix::process::CommandExt, process::Command};

struct TestChild(std::process::Child);
impl std::ops::Deref for TestChild {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for TestChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for TestChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            // This test owns the child regardless of deliberately corrupted
            // production identity records, including while assertions unwind.
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
            let _ = self.0.wait();
        }
    }
}

fn counter(root: &Path) -> u64 {
    match std::fs::metadata(root.join("counter")) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => panic!("read test counter: {error}"),
    }
}
fn wait_for_progress(root: &Path, baseline: u64) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let current = counter(root);
        if current > baseline {
            return current;
        }
        assert!(
            Instant::now() < deadline,
            "counter did not advance beyond {baseline}; observed {current}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
async fn wait_for_progress_async(root: &Path, baseline: u64) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let current = counter(root);
        if current > baseline {
            return current;
        }
        assert!(
            Instant::now() < deadline,
            "counter did not advance beyond {baseline}; observed {current}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
async fn wait_for_stop(child: &TestChild) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut status = 0;
        let observed = unsafe {
            libc::waitpid(
                child.id() as i32,
                &mut status,
                libc::WUNTRACED | libc::WNOHANG,
            )
        };
        assert!(
            observed >= 0,
            "observe stopped child: {}",
            std::io::Error::last_os_error()
        );
        if observed != 0 {
            assert!(
                libc::WIFSTOPPED(status),
                "child exited before stop acknowledgment"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "child {} did not stop",
            child.id()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn child(root: &Path) -> TestChild {
    let mut command = Command::new("sh");
    command
        .args(["-c", "while :; do printf x >> counter; sleep 0.02; done"])
        .current_dir(root);
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    TestChild(command.spawn().unwrap())
}

#[tokio::test]
async fn pause_stops_same_process_and_resume_continues() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::store::Store::open(dir.path()).unwrap();
    let control = Control::new(dir.path(), "test");
    scope(control.clone(), async {
        let mut child = child(dir.path());
        let guard = register_process(child.id()).unwrap();
        wait_for_progress_async(dir.path(), 0).await;
        control.set_paused(true).unwrap();
        wait_for_stop(&child).await;
        let before = counter(dir.path());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            before,
            std::fs::metadata(dir.path().join("counter")).unwrap().len()
        );
        let receipts: i64 = db
            .conn
            .query_row(
                "SELECT count(*) FROM runtime_settings WHERE key LIKE 'storage.pause:%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(receipts, 1);
        db.conn.execute("UPDATE runtime_settings SET value=json_set(value,'$.retry_probe',true) WHERE key LIKE 'storage.pause:%'", []).unwrap();
        control.set_paused(true).unwrap();
        let unchanged: bool = db.conn.query_row("SELECT json_extract(value,'$.retry_probe') FROM runtime_settings WHERE key LIKE 'storage.pause:%'", [], |row| row.get(0)).unwrap();
        assert!(unchanged, "repeated holds must not rewrite applied receipts");
        control.set_paused(false).unwrap();
        wait_for_progress_async(dir.path(), before).await;
        drop(guard);
        child.wait().unwrap();
    })
    .await;
}

#[tokio::test]
async fn timeout_and_checkpoint_exclude_paused_time() {
    let dir = tempfile::tempdir().unwrap();
    let control = Control::new(dir.path(), "timeout");
    control.set_paused(true).unwrap();
    let other = control.clone();
    let resume = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        other.set_paused(false).unwrap();
    });
    let start = Instant::now();
    scope(control.clone(), async {
        timeout(Duration::from_millis(60), async {
            checkpoint().await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        })
        .await
        .unwrap();
    })
    .await;
    resume.await.unwrap();
    assert!(start.elapsed() >= Duration::from_millis(150));
    assert!(control.paused_duration() >= Duration::from_millis(150));
}

#[tokio::test]
async fn dropping_paused_process_guard_kills_group() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::store::Store::open(dir.path()).unwrap();
    let control = Control::new(dir.path(), "cancel");
    scope(control.clone(), async {
        let mut child = child(dir.path());
        let guard = register_process(child.id()).unwrap();
        wait_for_progress_async(dir.path(), 0).await;
        control.set_paused(true).unwrap();
        let receipts: i64 = db
            .conn
            .query_row(
                "SELECT count(*) FROM runtime_settings WHERE key LIKE 'storage.pause:%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(receipts, 1);
        drop(guard);
        let receipts: i64 = db
            .conn
            .query_row(
                "SELECT count(*) FROM runtime_settings WHERE key LIKE 'storage.pause:%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(receipts, 0);
        let status = child.wait().unwrap();
        assert!(!status.success());
        let before = std::fs::metadata(dir.path().join("counter"))
            .map(|m| m.len())
            .unwrap_or(0);
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            before,
            std::fs::metadata(dir.path().join("counter"))
                .map(|m| m.len())
                .unwrap_or(0)
        );
    })
    .await;
}

#[test]
fn mismatched_identity_is_never_signaled() {
    let dir = tempfile::tempdir().unwrap();
    crate::store::Store::open(dir.path()).unwrap();
    let control = Control::new(dir.path(), "identity");
    let child = child(dir.path());
    let pid = child.id();
    let guard = blocking_scope(Some(control.clone()), || register_process(pid)).unwrap();
    wait_for_progress(dir.path(), 0);
    control
        .state
        .lock()
        .unwrap()
        .processes
        .get_mut(&pid)
        .unwrap()
        .identity = "mismatch".into();
    assert!(control.set_paused(true).is_err());
    let baseline = counter(dir.path());
    wait_for_progress(dir.path(), baseline);
    drop(guard);
    drop(child);
}

#[tokio::test]
async fn executor_cannot_spawn_while_paused() {
    let dir = tempfile::tempdir().unwrap();
    let control = Control::new(dir.path(), "spawn");
    control.set_paused(true).unwrap();
    let marker = dir.path().join("started");
    let check_marker = marker.clone();
    let other = control.clone();
    let resume = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!check_marker.exists());
        other.set_paused(false).unwrap();
    });
    scope(control, async {
        let mut command = crate::executor::clean_command("sh");
        command
            .args(["-c", "printf done > started"])
            .current_dir(dir.path());
        let result = crate::executor::run_process(command, None, 1, None)
            .await
            .unwrap();
        assert_eq!(result["success"], true);
    })
    .await;
    resume.await.unwrap();
    assert!(marker.exists());
}

#[test]
fn blocking_scope_restores_outer_control_after_unwind() {
    let dir = tempfile::tempdir().unwrap();
    let control = Control::new(dir.path(), "outer");
    blocking_scope(Some(control.clone()), || {
        assert!(Arc::ptr_eq(&current().unwrap(), &control));
        let _ = std::panic::catch_unwind(|| blocking_scope(None, || panic!("test unwind")));
        assert!(Arc::ptr_eq(&current().unwrap(), &control));
    });
    assert!(current().is_none());
}

#[tokio::test]
async fn startup_environment_recovery_preserves_paused_orphans() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::store::Store::open(&dir.path().join("data")).unwrap();
    let plan = crate::template::compile(
        "simulated",
        &crate::template::load_templates(dir.path()).unwrap(),
        BTreeMap::from([("task".into(), "app test".into())]),
    )
    .unwrap();
    let task = db
        .submit(
            "app test",
            dir.path(),
            &crate::config::Settings::default(),
            &plan,
        )
        .unwrap();
    let control = Control::new(&db.root, "orphan");
    scope(control.clone(), async {
        let mut child = child(dir.path());
        let guard = register_process(child.id()).unwrap();
        let identity = crate::environment::process_identity(child.id()).unwrap();
        db.conn.execute("INSERT INTO app_environments(id,task,attempt,kind,state,spec,workspace,pid,created,expires) VALUES('paused-app',?,'orphan','process','starting','{}',?,?,0,1)", rusqlite::params![task,dir.path().to_str(),child.id()]).unwrap();
        db.conn.execute("INSERT INTO app_process_identity VALUES('paused-app',?)", [&identity]).unwrap();
        control.set_paused(true).unwrap();
        crate::environment::reconcile(&db).await.unwrap();
        assert!(child.try_wait().unwrap().is_none());
        let state: String = db.conn.query_row("SELECT state FROM app_environments WHERE id='paused-app'", [], |r|r.get(0)).unwrap();
        assert_eq!(state,"starting");
        db.conn.execute("UPDATE app_environments SET state='cleanup_pending' WHERE id='paused-app'", []).unwrap();
        crate::environment::cleanup_pending(&db).await.unwrap();
        assert!(child.try_wait().unwrap().is_none());
        let state: String = db.conn.query_row("SELECT state FROM app_environments WHERE id='paused-app'", [], |r|r.get(0)).unwrap();
        assert_eq!(state,"cleanup_pending");
        drop(guard);
        child.wait().unwrap();
    }).await;
}

#[tokio::test]
async fn receipt_write_failure_still_stops_every_owned_group_and_retries() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::store::Store::open(dir.path()).unwrap();
    let control = Control::new(dir.path(), "full-disk");
    scope(control.clone(), async {
        let mut first = child(dir.path());
        let mut second = child(dir.path());
        let first_guard = register_process(first.id()).unwrap();
        let second_guard = register_process(second.id()).unwrap();
        wait_for_progress_async(dir.path(), 0).await;
        db.conn.execute_batch("CREATE TRIGGER no_pause_receipt BEFORE INSERT ON runtime_settings WHEN NEW.key GLOB 'storage.pause:*' BEGIN SELECT RAISE(ABORT, 'simulated disk full'); END;").unwrap();
        let error = control.set_paused(true).unwrap_err().to_string();
        assert!(error.contains("simulated disk full"));
        assert!(control.is_paused());
        wait_for_stop(&first).await;
        wait_for_stop(&second).await;
        let before = counter(dir.path());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(before, std::fs::metadata(dir.path().join("counter")).unwrap().len());
        let receipts: i64 = db.conn.query_row("SELECT count(*) FROM runtime_settings WHERE key LIKE 'storage.pause:%'", [], |row|row.get(0)).unwrap();
        assert_eq!(receipts, 0);
        db.conn.execute_batch("DROP TRIGGER no_pause_receipt;").unwrap();
        control.set_paused(true).unwrap();
        let receipts: i64 = db.conn.query_row("SELECT count(*) FROM runtime_settings WHERE key LIKE 'storage.pause:%'", [], |row|row.get(0)).unwrap();
        assert_eq!(receipts, 2);
        db.conn.execute_batch("CREATE TRIGGER no_receipt_delete BEFORE DELETE ON runtime_settings WHEN OLD.key GLOB 'storage.pause:*' BEGIN SELECT RAISE(ABORT, 'simulated delete failure'); END;").unwrap();
        assert!(control.set_paused(false).is_err());
        assert!(control.is_paused(), "failed receipt cleanup must retain workflow gate");
        assert!(control.state.lock().unwrap().processes.values().all(|process| !process.paused));
        control.set_paused(true).unwrap();
        assert!(control.state.lock().unwrap().processes.values().all(|process| process.paused));
        wait_for_stop(&first).await;
        wait_for_stop(&second).await;
        let before = counter(dir.path());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(before, std::fs::metadata(dir.path().join("counter")).unwrap().len());
        db.conn.execute_batch("DROP TRIGGER no_receipt_delete;").unwrap();
        control.set_paused(false).unwrap();
        drop(first_guard);
        drop(second_guard);
        first.wait().unwrap();
        second.wait().unwrap();
    }).await;
}

#[tokio::test]
async fn dropping_guard_after_failed_resume_removes_pending_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::store::Store::open(dir.path()).unwrap();
    let control = Control::new(dir.path(), "failed-resume-cleanup");
    scope(control.clone(), async {
        let mut child = child(dir.path());
        let guard = register_process(child.id()).unwrap();
        control.set_paused(true).unwrap();
        wait_for_stop(&child).await;
        db.conn.execute_batch("CREATE TRIGGER no_receipt_delete BEFORE DELETE ON runtime_settings WHEN OLD.key GLOB 'storage.pause:*' BEGIN SELECT RAISE(ABORT, 'simulated delete failure'); END;").unwrap();
        assert!(control.set_paused(false).is_err());
        {
            let state = control.state.lock().unwrap();
            let process = state.processes.get(&child.id()).unwrap();
            assert!(!process.paused);
            assert!(!process.receipt_synced);
        }
        let receipts: i64 = db.conn.query_row("SELECT count(*) FROM runtime_settings WHERE key LIKE 'storage.pause:%'", [], |row| row.get(0)).unwrap();
        assert_eq!(receipts, 1);
        db.conn.execute_batch("DROP TRIGGER no_receipt_delete").unwrap();
        drop(guard);
        child.wait().unwrap();
        let receipts: i64 = db.conn.query_row("SELECT count(*) FROM runtime_settings WHERE key LIKE 'storage.pause:%'", [], |row| row.get(0)).unwrap();
        assert_eq!(receipts, 0);
        assert!(control.state.lock().unwrap().processes.is_empty());
    }).await;
}

#[test]
fn unavailable_identity_is_inconclusive_for_a_live_process() {
    let dir = tempfile::tempdir().unwrap();
    let control = Control::new(dir.path(), "identity-unavailable");
    let mut child = child(dir.path());
    let pid = child.id();
    let guard = blocking_scope(Some(control), || register_process(pid)).unwrap();
    assert!(verify_identity(pid, "unavailable", None).is_err());
    assert!(child.try_wait().unwrap().is_none());
    drop(guard);
    child.wait().unwrap();
    assert!(!verify_identity(pid, "unavailable", None).unwrap());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn linux_owned_groups_use_proc_identity_and_preserve_legacy_verification() {
    let dir = tempfile::tempdir().unwrap();
    let control = Control::new(dir.path(), "linux-process-identity");
    let mut child = child(dir.path());
    let pid = child.id();
    let identity = crate::environment::process_identity(pid).unwrap();
    assert!(identity.starts_with("linux-proc-v1:"));
    assert_eq!(crate::environment::process_identity(pid), Some(identity));
    let old = crate::executor::clean_command("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart=", "-o", "pgid="])
        .output()
        .unwrap();
    assert!(old.status.success());
    let legacy = crate::store::hash(&old.stdout);
    assert_eq!(
        crate::environment::process_identity_for(pid, &legacy),
        Some(legacy.clone())
    );
    let guard = blocking_scope(Some(control.clone()), || register_process(pid)).unwrap();
    // A receipt from the previous runtime must still prove this exact process,
    // including pressure signals and cancellation after an upgrade.
    control
        .state
        .lock()
        .unwrap()
        .processes
        .get_mut(&pid)
        .unwrap()
        .identity = legacy;
    control.set_paused(true).unwrap();
    wait_for_stop(&child).await;
    control.set_paused(false).unwrap();
    drop(guard);
    child.wait().unwrap();
    assert!(crate::environment::process_identity(pid).is_none());
}

#[test]
fn registration_after_child_exit_does_not_create_an_owned_process() {
    let dir = tempfile::tempdir().unwrap();
    let control = Control::new(dir.path(), "already-exited");
    let mut command = Command::new("sh");
    command.args(["-c", "exit 0"]);
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let pid = child.id();
    assert!(child.wait().unwrap().success());
    blocking_scope(Some(control.clone()), || {
        let guard = register_process(pid).expect("an exited child needs no storage tracking");
        assert!(guard.control.is_none());
        assert!(control.state.lock().unwrap().processes.is_empty());
    });
}

#[test]
fn registration_still_rejects_live_process_without_owned_group() {
    let dir = tempfile::tempdir().unwrap();
    let control = Control::new(dir.path(), "wrong-group");
    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    let result = blocking_scope(Some(control.clone()), || register_process(child.id()));
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(result.is_err());
    assert!(control.state.lock().unwrap().processes.is_empty());
}

#[tokio::test]
async fn fast_executor_commands_complete_under_storage_supervision() {
    let dir = tempfile::tempdir().unwrap();
    crate::store::Store::open(dir.path()).unwrap();
    let control = Control::new(dir.path(), "fast-commands");
    scope(control.clone(), async {
        for _ in 0..32 {
            let mut command = crate::executor::clean_command("sh");
            command.args(["-c", "exit 0"]);
            let result = crate::executor::run_process(command, None, 1, None)
                .await
                .unwrap();
            assert_eq!(result["success"], true);
            assert!(control.state.lock().unwrap().processes.is_empty());
        }
    })
    .await;
}

#[test]
fn fixture_reaps_child_when_identity_mismatch_assertion_unwinds() {
    let dir = tempfile::tempdir().unwrap();
    crate::store::Store::open(dir.path()).unwrap();
    let control = Control::new(dir.path(), "panic-cleanup");
    let pid = std::sync::atomic::AtomicU32::new(0);
    use std::sync::atomic::Ordering;
    let result = std::panic::catch_unwind(|| {
        let child = child(dir.path());
        pid.store(child.id(), Ordering::SeqCst);
        let _guard =
            blocking_scope(Some(control.clone()), || register_process(child.id())).unwrap();
        control
            .state
            .lock()
            .unwrap()
            .processes
            .get_mut(&child.id())
            .unwrap()
            .identity = "mismatch".into();
        panic!("exercise test fixture cleanup");
    });
    assert!(result.is_err());
    let pid = pid.load(Ordering::SeqCst);
    assert!(pid > 1);
    assert!(
        process_absent(pid),
        "fixture must kill and reap its child during unwinding"
    );
    assert!(control.state.lock().unwrap().processes.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn executor_launches_do_not_block_current_thread_http_responses() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::task::LocalSet::new().run_until(async {
        let dir = tempfile::tempdir().unwrap();
        crate::store::Store::open(dir.path()).unwrap();
        let control = Control::new(dir.path(), "parallel-launches");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::task::spawn_local(async move {
            for _ in 0..32 {
                let (mut stream, _) = listener.accept().await.unwrap();
                tokio::task::spawn_local(async move {
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        stream.read_exact(&mut byte).await.unwrap();
                        request.extend_from_slice(&byte);
                    }
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await.unwrap();
                });
            }
        });
        let started = Instant::now();
        let mut launches = Vec::new();
        for _ in 0..32 {
            let control = control.clone();
            launches.push(tokio::task::spawn_local(scope(control, async {
                let mut command = crate::executor::clean_command("sh");
                command.args(["-c", "exit 0"]);
                // The parent waits for pre_exec/exec. Model a slow launch handshake,
                // rather than delaying an already-running process or the HTTP server.
                unsafe { command.pre_exec(|| { libc::usleep(100_000); Ok(()) }); }
                let result = crate::executor::run_process(command, None, 5, None).await.unwrap();
                assert_eq!(result["success"], true);
            })));
        }
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let requests = (0..32).map(|_| async {
            let response = client.get(format!("http://{address}/")).send().await.unwrap();
            assert_eq!(response.status(), 200);
            assert_eq!(response.text().await.unwrap(), "ok");
        });
        futures_util::future::join_all(requests).await;
        let elapsed = started.elapsed();
        for launch in launches { launch.await.unwrap(); }
        server.await.unwrap();
        assert!(control.state.lock().unwrap().processes.is_empty());
        eprintln!("32 HTTP responses during 32 slow process launches: {elapsed:?}");
        assert!(elapsed < Duration::from_secs(2), "launches stalled the reactor: {elapsed:?}");
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn never_paused_process_cleanup_does_not_wait_for_sqlite_writer() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::task::LocalSet::new().run_until(async {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::store::Store::open(dir.path()).unwrap();
        let control = Control::new(dir.path(), "unpaused-cleanup");
        let mut children = Vec::new();
        let mut guards = Vec::new();
        for _ in 0..32 {
            let child = child(dir.path());
            let guard = blocking_scope(Some(control.clone()), || register_process(child.id())).unwrap();
            children.push(child);
            guards.push(guard);
        }
        let path = db.root.join("state.sqlite3");
        let (ready, locked) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let connection = rusqlite::Connection::open(path).unwrap();
            connection.execute_batch("BEGIN IMMEDIATE").unwrap();
            ready.send(()).unwrap();
            std::thread::sleep(Duration::from_secs(3));
            connection.execute_batch("ROLLBACK").unwrap();
        });
        locked.recv().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::task::spawn_local(async move {
            for _ in 0..32 {
                let (mut stream, _) = listener.accept().await.unwrap();
                tokio::task::spawn_local(async move {
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        stream.read_exact(&mut byte).await.unwrap();
                        request.extend_from_slice(&byte);
                    }
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await.unwrap();
                });
            }
        });
        let started = Instant::now();
        let drops: Vec<_> = guards.into_iter().map(|guard| tokio::task::spawn_local(async move { drop(guard) })).collect();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        futures_util::future::join_all((0..32).map(|_| async {
            let response = client.get(format!("http://{address}/")).send().await.unwrap();
            assert_eq!(response.status(), 200);
            assert_eq!(response.text().await.unwrap(), "ok");
        })).await;
        for task in drops { task.await.unwrap(); }
        let elapsed = started.elapsed();
        for child in &mut children { child.wait().unwrap(); }
        writer.join().unwrap();
        server.await.unwrap();
        assert!(control.state.lock().unwrap().processes.is_empty());
        let receipts: i64 = db.conn.query_row("SELECT count(*) FROM runtime_settings WHERE key LIKE 'storage.pause:%'", [], |row| row.get(0)).unwrap();
        assert_eq!(receipts, 0);
        eprintln!("32 HTTP responses during never-paused cleanup with SQLite writer: {elapsed:?}");
        assert!(elapsed < Duration::from_secs(2), "unnecessary receipt cleanup stalled the reactor: {elapsed:?}");
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_during_executor_launch_drops_group_and_storage_ownership() {
    use std::os::fd::AsRawFd;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::task::LocalSet::new().run_until(async {
        let dir = tempfile::tempdir().unwrap();
        crate::store::Store::open(dir.path()).unwrap();
        let control = Control::new(dir.path(), "cancel-launch");
        let (parent, gate) = std::os::unix::net::UnixStream::pair().unwrap();
        parent.set_nonblocking(true).unwrap();
        let mut parent = tokio::net::UnixStream::from_std(parent).unwrap();
        let root = dir.path().to_owned();
        let handle = tokio::task::spawn_local(scope(control.clone(), async move {
            let mut command = crate::executor::clean_command("sh");
            command.args(["-c", "sh -c 'while :; do printf x >> counter; sleep 0.01; done' & echo $$ $! > pids; wait"])
                .current_dir(root);
            unsafe { command.pre_exec(move || {
                let fd = gate.as_raw_fd();
                let byte = [1u8];
                if libc::write(fd, byte.as_ptr().cast(), 1) != 1 { return Err(std::io::Error::last_os_error()); }
                let mut release = [0u8];
                if libc::read(fd, release.as_mut_ptr().cast(), 1) != 1 { return Err(std::io::Error::last_os_error()); }
                Ok(())
            }); }
            crate::executor::run_process(command, None, 30, None).await
        }));
        let mut byte = [0];
        tokio::time::timeout(Duration::from_secs(5), parent.read_exact(&mut byte)).await.unwrap().unwrap();
        // The async checkpoint passed, but spawn has not returned. Keep registration
        // blocked after exec so cancellation happens before the blocking result exists.
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
        let (unlock_tx, unlock_rx) = std::sync::mpsc::channel();
        let held = control.clone();
        let registration = std::thread::spawn(move || {
            let state = held.state.lock().unwrap();
            locked_tx.send(()).unwrap();
            let released = unlock_rx.recv_timeout(Duration::from_secs(10));
            drop(state);
            released.unwrap();
        });
        locked_rx.await.unwrap();
        parent.write_all(&[1]).await.unwrap();
        let progress = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let pids: Vec<i32> = std::fs::read_to_string(dir.path().join("pids"))
                    .unwrap_or_default().split_whitespace().filter_map(|p| p.parse().ok()).collect();
                if counter(dir.path()) > 0 && pids.len() == 2 { break pids; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await;
        handle.abort();
        let cancelled = handle.await;
        unlock_tx.send(()).unwrap();
        registration.join().unwrap();
        let pids = progress.unwrap();
        assert!(cancelled.unwrap_err().is_cancelled());
        let leader = pids[0];
        let deadline = Instant::now() + Duration::from_secs(5);
        while !process_absent(leader as u32) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let stopped = process_absent(leader as u32);
        let before = counter(dir.path());
        tokio::time::sleep(Duration::from_millis(100)).await;
        let after = counter(dir.path());
        // Always clean up our group even when the assertions below find a regression.
        unsafe { libc::kill(-leader, libc::SIGKILL); }
        assert!(stopped, "cancelled launch leader was not killed/reaped");
        assert_eq!(before, after, "descendant survived cancellation during launch");
        assert!(control.state.lock().unwrap().processes.is_empty());
        let db = crate::store::Store::open(dir.path()).unwrap();
        let receipts: i64 = db.conn.query_row("SELECT count(*) FROM runtime_settings WHERE key LIKE 'storage.pause:cancel-launch:%'", [], |r| r.get(0)).unwrap();
        assert_eq!(receipts, 0);
    }).await;
}

#[test]
fn cancelled_queued_executor_launch_never_starts_a_process() {
    use std::os::unix::ffi::OsStrExt;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("late-launch");
    runtime.block_on(async {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let occupied = tokio::task::spawn_blocking(move || {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        started_rx.await.unwrap();
        let path = std::ffi::CString::new(marker.as_os_str().as_bytes()).unwrap();
        let mut command = crate::executor::clean_command("sh");
        command.args(["-c", "exit 0"]);
        unsafe {
            command.pre_exec(move || {
                // Record the actual pre-exec side effect before any late guard can
                // kill the child, so immediate post-spawn cleanup cannot mask it.
                let fd = libc::open(
                    path.as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                    0o600,
                );
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(fd);
                Ok(())
            });
        }
        let mut launch = Box::pin(crate::executor::run_process(command, None, 5, None));
        assert!(futures_util::poll!(launch.as_mut()).is_pending());
        drop(launch);
        release_tx.send(()).unwrap();
        occupied.await.unwrap();
        // Drain the single-thread blocking queue after the cancelled launch job.
        tokio::task::spawn_blocking(|| ()).await.unwrap();
        assert!(
            !marker.exists(),
            "cancelled queued command reached pre_exec"
        );
    });
}
