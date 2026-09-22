use crate::config::ExecutorConfig;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

pub(crate) fn lock(kind: &str) -> Result<std::fs::File> {
    use fs2::FileExt;
    // The CLI commands use HOME, even when daemons have different XDG config roots.
    let directory =
        PathBuf::from(std::env::var_os("HOME").context("HOME required for provider login")?)
            .join(".horde-provider-logins");
    std::fs::create_dir_all(&directory)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(directory.join(format!("provider-login-{kind}.lock")))?;
    file.try_lock_exclusive().context(
        "a login for this shared provider credential store is already active in another daemon",
    )?;
    Ok(file)
}

pub(super) fn command(config: &ExecutorConfig) -> tokio::process::Command {
    let mut command = tokio::process::Command::from(crate::executor::clean_command(
        config.program.as_deref().unwrap_or(&config.kind),
    ));
    command
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .env("BROWSER", "/usr/bin/false")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    command
}

#[derive(Serialize, Deserialize)]
struct Receipt {
    pid: u32,
    identity: String,
}

pub(crate) struct Guard {
    pid: u32,
    path: PathBuf,
    stopped: AtomicBool,
}

impl Guard {
    pub(crate) fn record(root: &Path, id: &str, pid: u32) -> Result<Self> {
        let directory = root.join("provider-logins");
        let guard = Self {
            pid,
            path: directory.join(format!("{id}.json")),
            stopped: AtomicBool::new(false),
        };
        std::fs::create_dir_all(&directory)?;
        if let Some(identity) = crate::environment::process_identity(pid) {
            // Unfinished writes stay outside the directory scanned during recovery.
            let mut temporary = tempfile::NamedTempFile::new_in(root)?;
            temporary.write_all(&serde_json::to_vec(&Receipt { pid, identity })?)?;
            temporary.as_file().sync_all()?;
            temporary.persist(&guard.path)?;
            std::fs::File::open(&directory)?.sync_all()?;
        } else {
            // A very fast CLI may have exited already. A live process must be recoverable.
            ensure!(
                !crate::executor::process_alive(pid as i32),
                "cannot record provider login process identity"
            );
        }
        Ok(guard)
    }
    pub(crate) fn stop(&self) {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            unsafe {
                libc::kill(-(self.pid as i32), libc::SIGKILL);
            }
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.stop();
        let _ = std::fs::remove_file(&self.path);
    }
}

pub(super) fn recover(root: &Path) -> Result<()> {
    let directory = root.join("provider-logins");
    if !directory.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        ensure!(
            entry.file_type()?.is_file(),
            "invalid login process receipt"
        );
        let bytes = std::fs::read(entry.path())?;
        ensure!(bytes.len() <= 4096, "invalid login process receipt");
        let receipt: Receipt =
            serde_json::from_slice(&bytes).context("invalid login process receipt")?;
        ensure!(
            receipt.pid > 1 && receipt.pid <= i32::MAX as u32,
            "invalid login process pid"
        );
        if crate::executor::process_alive(receipt.pid as i32) {
            if let Some(identity) = crate::environment::process_identity(receipt.pid) {
                if identity == receipt.identity {
                    unsafe {
                        libc::kill(-(receipt.pid as i32), libc::SIGKILL);
                    }
                }
                // A reused PID belongs to another process and is deliberately untouched.
            } else {
                anyhow::bail!("cannot verify interrupted login process identity");
            }
        }
        std::fs::remove_file(entry.path())?;
    }
    Ok(())
}

pub(super) async fn verify(
    config: &ExecutorConfig,
    root: &Path,
    id: &str,
    remaining: Duration,
    receiver: &mut tokio::sync::mpsc::Receiver<super::Control>,
) -> Result<&'static str> {
    let mut cmd = command(config);
    cmd.args(if config.kind == "codex" {
        ["login", "status"]
    } else {
        ["auth", "status"]
    })
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    let guard = match Guard::record(
        root,
        &format!("{id}-verify"),
        child.id().context("verification pid missing")?,
    ) {
        Ok(guard) => guard,
        Err(error) => {
            let _ = child.kill().await;
            return Err(error);
        }
    };
    let timeout = tokio::time::sleep(remaining.min(Duration::from_secs(15)));
    tokio::pin!(timeout);
    let result = loop {
        tokio::select! {
            status = child.wait() => break status.map(|s| if s.success() { "succeeded" } else { "failed" }),
            _ = &mut timeout => break Ok("expired"),
            control = receiver.recv() => {
                if matches!(control, Some(super::Control::Cancel) | None) { break Ok("cancelled"); }
            }
        }
    };
    guard.stop();
    let _ = child.wait().await;
    Ok(result?)
}

/// Strip terminal controls, including OSC hyperlinks, while retaining their visible label.
pub(super) fn plain_text(bytes: &[u8]) -> String {
    let mut result = String::new();
    let text = String::from_utf8_lossy(bytes);
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' || next == '\u{1b}' && chars.next() == Some('\\') {
                            break;
                        }
                    }
                }
                _ => (),
            }
        } else if !ch.is_control() || matches!(ch, '\n' | '\t') {
            result.push(ch);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_escapes_are_removed_without_losing_visible_login_instructions() {
        assert_eq!(plain_text(b"\x1b[32mVisit \x1b]8;;https://example.invalid\x1b\\https://example.invalid\x1b]8;;\x07\x1b[0m\r\nCode\tABCD\x00"), "Visit https://example.invalid\nCode\tABCD");
        assert_eq!(plain_text(b"hello\x1bX world"), "hello world");
    }

    #[test]
    fn interrupted_temporary_receipt_does_not_block_recovery() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("provider-logins")).unwrap();
        let mut temporary = tempfile::NamedTempFile::new_in(root.path()).unwrap();
        temporary.write_all(b"{\"pid\":").unwrap();
        recover(root.path()).unwrap();
    }

    #[test]
    fn recovery_leaves_reused_process_identity_untouched() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("provider-logins");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(
            directory.join("old.json"),
            serde_json::to_vec(&Receipt {
                pid: std::process::id(),
                identity: "different-process-identity".into(),
            })
            .unwrap(),
        )
        .unwrap();
        recover(root.path()).unwrap();
        assert!(crate::executor::process_alive(std::process::id() as i32));
        assert!(!directory.join("old.json").exists());
    }
}
