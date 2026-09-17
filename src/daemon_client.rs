//! Start a local daemon and query its private Unix socket.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    path::Path,
    time::Duration,
};

/// How long a caller waits for the daemon to answer.
///
/// The daemon grants a connection [`crate::runtime::REQUEST_READ_TIMEOUT`] just
/// to deliver its request line, and serving one request can open a nested
/// request back onto this same socket: an `agent_setup` inspection asks itself
/// for `runtime_status` while it is being served. A caller whose deadline is no
/// larger than the work it waits for reports a healthy daemon as unreachable
/// the moment the host is busy, so this budget has to dominate both the
/// daemon's own deadline and any nested [`PROBE_TIMEOUT`].
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a liveness check waits before calling the daemon unreachable.
///
/// Probes answer "is the daemon serving right now?", sometimes from inside the
/// daemon that is serving them, so they must finish well inside
/// [`REQUEST_TIMEOUT`] while still leaving room for a loaded host. A daemon
/// that is not listening at all fails to connect immediately and never spends
/// this budget.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub fn running(root: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(root.join("daemon.sock")).is_ok()
}

pub fn request(root: &Path, method: &str, args: Value) -> Result<Value> {
    send(root, method, args, REQUEST_TIMEOUT)
}

/// Ask the daemon a question whose only purpose is to learn whether it is
/// serving. Callers treat a failure as "no daemon", so this must never be the
/// outermost request of a nested pair.
pub fn probe(root: &Path, method: &str, args: Value) -> Result<Value> {
    send(root, method, args, PROBE_TIMEOUT)
}

fn send(root: &Path, method: &str, args: Value, timeout: Duration) -> Result<Value> {
    let mut stream = std::os::unix::net::UnixStream::connect(root.join("daemon.sock"))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    writeln!(
        stream,
        "{}",
        json!({"method":method,"args":args,"token":crate::branding::var("HORDE_WORKER_TOKEN").ok()})
    )?;
    let mut response = String::new();
    std::io::BufReader::new(stream).read_line(&mut response)?;
    let response: Value = serde_json::from_str(&response).context("invalid daemon response")?;
    if let Some(error) = response.get("error") {
        bail!("{}", error.as_str().unwrap_or("daemon request failed"));
    }
    Ok(response["result"].clone())
}

pub async fn start(root: &Path) -> Result<Value> {
    let skill_pack = crate::skill_catalog::check(root)?;
    if running(root) {
        return Ok(json!({"running":true,"skill_pack":skill_pack}));
    }
    std::fs::create_dir_all(root)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("daemon.log"))?;
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .arg("--data-dir")
        .arg(root)
        .arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    for _ in 0..50 {
        if running(root) {
            return Ok(json!({"pid":child.id(),"running":true,"skill_pack":skill_pack}));
        }
        if child.try_wait()?.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!(
        "daemon failed to start; inspect {}",
        root.join("daemon.log").display()
    )
}

#[cfg(test)]
mod deadline_tests {
    use super::*;

    /// A caller that gives up before the daemon's own deadline turns a busy
    /// host into a phantom "daemon unavailable": the request was still being
    /// served when the caller stopped listening. Budgets must therefore shrink
    /// strictly inwards, from caller to daemon to nested probe.
    #[test]
    fn request_budgets_outlast_the_work_they_wait_on() {
        assert!(
            REQUEST_TIMEOUT > crate::runtime::REQUEST_READ_TIMEOUT,
            "a request must outwait the daemon's own read deadline"
        );
        assert!(
            REQUEST_TIMEOUT > PROBE_TIMEOUT,
            "a request must outwait the liveness probe it may nest"
        );
        assert!(
            PROBE_TIMEOUT >= Duration::from_secs(5),
            "a probe must survive a loaded host, not just an idle one"
        );
    }
}
