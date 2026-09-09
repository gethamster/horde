//! Start a local daemon and query its private Unix socket.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    path::Path,
    time::Duration,
};

pub fn running(root: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(root.join("daemon.sock")).is_ok()
}

pub fn request(root: &Path, method: &str, args: Value) -> Result<Value> {
    let mut stream = std::os::unix::net::UnixStream::connect(root.join("daemon.sock"))?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
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
    if running(root) {
        return Ok(json!({"running":true}));
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
            return Ok(json!({"pid":child.id(),"running":true}));
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
