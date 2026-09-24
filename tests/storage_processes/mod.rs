use super::*;
use std::{os::unix::process::CommandExt, process::Command};

fn child(root: &Path) -> std::process::Child {
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
    command.spawn().unwrap()
}

#[tokio::test]
async fn pause_stops_same_process_and_resume_continues() {
    let dir = tempfile::tempdir().unwrap();
    let db = crate::store::Store::open(dir.path()).unwrap();
    let control = Control::new(dir.path(), "test");
    scope(control.clone(), async {
        let mut child = child(dir.path());
        let guard = register_process(child.id()).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        control.set_paused(true).unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        let before = std::fs::metadata(dir.path().join("counter")).unwrap().len();
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
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(std::fs::metadata(dir.path().join("counter")).unwrap().len() > before);
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
    crate::store::Store::open(dir.path()).unwrap();
    let control = Control::new(dir.path(), "cancel");
    scope(control.clone(), async {
        let mut child = child(dir.path());
        let guard = register_process(child.id()).unwrap();
        control.set_paused(true).unwrap();
        drop(guard);
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
    let mut child = child(dir.path());
    let pid = child.id();
    let guard = blocking_scope(Some(control.clone()), || register_process(pid)).unwrap();
    control
        .state
        .lock()
        .unwrap()
        .processes
        .get_mut(&pid)
        .unwrap()
        .identity = "mismatch".into();
    assert!(control.set_paused(true).is_err());
    std::thread::sleep(Duration::from_millis(100));
    assert!(std::fs::metadata(dir.path().join("counter")).unwrap().len() > 1);
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    child.wait().unwrap();
    drop(guard);
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
        tokio::time::sleep(Duration::from_millis(100)).await;
        db.conn.execute_batch("CREATE TRIGGER no_pause_receipt BEFORE INSERT ON runtime_settings WHEN NEW.key GLOB 'storage.pause:*' BEGIN SELECT RAISE(ABORT, 'simulated disk full'); END;").unwrap();
        let error = control.set_paused(true).unwrap_err().to_string();
        assert!(error.contains("simulated disk full"));
        assert!(control.is_paused());
        tokio::time::sleep(Duration::from_millis(40)).await;
        let before = std::fs::metadata(dir.path().join("counter")).unwrap().len();
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
        tokio::time::sleep(Duration::from_millis(40)).await;
        let before = std::fs::metadata(dir.path().join("counter")).unwrap().len();
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
