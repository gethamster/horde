use std::{
    path::Path,
    process::Child,
    time::{Duration, Instant},
};

/// Normal fixture cleanup exercises shutdown and flushes instrumentation. Tests
/// that simulate a crash still kill their daemon explicitly at the crash point.
pub fn stop_daemon(child: &mut Child, root: &Path) {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    let _ = std::fs::write(root.join("shutdown.request"), b"");
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}
