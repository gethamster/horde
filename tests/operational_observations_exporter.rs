//! Real component CLI lifecycle: no model/daemon startup and no secret in controller.
use std::{
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn operational_observations_exporter_does_not_start_worker_daemon() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        run_exporter(signal);
    }
}

fn run_exporter(signal: i32) {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("exporter.json");
    std::fs::write(&config,serde_json::json!({"schema_version":1,"endpoint":"http://signals:8080/v1/observations","token_file":"/run/system/telemetry/token","tenant_id":"foundry","telemetry_project_id":"system-operations","diagnostic_thread_id":"foundry-operations","project_ids":["default"]}).to_string()).unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_horde"))
        .arg("--data-dir")
        .arg(root.path())
        .arg("observations-export")
        .arg("--config")
        .arg(config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while !root
        .path()
        .join("operational-observations-exporter.lock")
        .exists()
    {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "exporter failed to initialize"
        );
        assert!(
            child.try_wait().unwrap().is_none(),
            "exporter exited before initialization"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(80));
    assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
    let status = child.wait().unwrap();
    assert!(status.success());
    let db = horde::store::Store::open(root.path()).unwrap();
    assert_eq!(
        db.conn
            .query_row("SELECT count(*) FROM attempts", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.conn
            .query_row("SELECT count(*) FROM tasks", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(!root.path().join("daemon.sock").exists());
    assert!(!root.path().join("daemon.pid").exists());
}
