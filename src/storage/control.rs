//! Live process ownership for disk-pressure holds. Crash receipts never authorize signals.
use anyhow::{Context, Result, bail, ensure};
use serde_json::json;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    task::Poll,
    time::{Duration, Instant},
};

#[derive(Default)]
struct State {
    paused: Option<Instant>,
    elapsed: Duration,
    processes: BTreeMap<u32, Process>,
}
struct Process {
    identity: String,
    paused: bool,
    receipt_synced: bool,
}
pub(crate) struct Control {
    root: PathBuf,
    attempt: String,
    state: Mutex<State>,
}
impl Control {
    pub(crate) fn new(root: &Path, attempt: &str) -> Arc<Self> {
        Arc::new(Self {
            root: root.into(),
            attempt: attempt.into(),
            state: Mutex::new(State::default()),
        })
    }
    pub(crate) fn is_paused(&self) -> bool {
        self.state.lock().unwrap().paused.is_some()
    }
    pub(crate) fn paused_duration(&self) -> Duration {
        let state = self.state.lock().unwrap();
        state.elapsed
            + state
                .paused
                .map(|start| start.elapsed())
                .unwrap_or_default()
    }
    pub(crate) fn pressure_file(&self) -> PathBuf {
        self.root.join("storage-pressure.json")
    }
    pub(crate) fn set_paused(&self, paused: bool) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if paused && state.paused.is_none() {
            state.paused = Some(Instant::now());
        }
        if !paused && state.paused.is_none() {
            return Ok(());
        }
        let mut errors = Vec::new();
        for (&pid, process) in &mut state.processes {
            if (process.paused != paused || !process.receipt_synced)
                && let Err(error) = self.apply(pid, process, paused)
            {
                errors.push(format!("process {pid}: {error:#}"));
            }
        }
        ensure!(
            errors.is_empty(),
            "storage hold degraded: {}",
            errors.join("; ")
        );
        if !paused && let Some(start) = state.paused.take() {
            state.elapsed += start.elapsed();
        }
        Ok(())
    }
    fn receipt_key(&self, pid: u32) -> String {
        format!("storage.pause:{}:{pid}", self.attempt)
    }
    fn clear_receipt(&self, pid: u32) -> Result<()> {
        let db = crate::store::Store::open(&self.root)?;
        db.conn.execute(
            "DELETE FROM runtime_settings WHERE key=?",
            [self.receipt_key(pid)],
        )?;
        Ok(())
    }
    fn apply(&self, pid: u32, process: &mut Process, paused: bool) -> Result<()> {
        if !verify_identity(
            pid,
            &process.identity,
            crate::environment::process_identity(pid),
        )? {
            return Ok(());
        }
        // Try recording intent first, but full storage must not prevent stopping a
        // live group whose ownership this guard already proves. Retain and retry
        // failed receipts; persisted PIDs alone never authorize future signals.
        let receipt = if paused {
            (|| -> Result<()> {
                let db = crate::store::Store::open(&self.root)?;
                crate::management::set(&db, &self.receipt_key(pid), &json!({"attempt":self.attempt,"pid":pid,"identity":process.identity,"state":"paused","created":crate::store::now()}).to_string())
            })()
        } else {
            Ok(())
        };
        if process.paused != paused {
            let signal = if paused { libc::SIGSTOP } else { libc::SIGCONT };
            if unsafe { libc::kill(-(pid as i32), signal) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error.into());
                }
            }
            process.paused = paused;
        }
        let receipt = if paused {
            receipt
        } else {
            self.clear_receipt(pid)
        };
        process.receipt_synced = receipt.is_ok();
        receipt.context("process signal applied but durable pause receipt unavailable")
    }
}

fn process_absent(pid: u32) -> bool {
    (unsafe { libc::kill(pid as i32, 0) }) != 0
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

fn verify_identity(pid: u32, expected: &str, actual: Option<String>) -> Result<bool> {
    let Some(actual) = actual else {
        let absent = process_absent(pid);
        ensure!(
            absent,
            "cannot verify live process {pid}; refusing storage pressure signal"
        );
        return Ok(false);
    };
    ensure!(
        actual == expected && unsafe { libc::getpgid(pid as i32) } == pid as i32,
        "owned process {pid} identity changed; refusing storage pressure signal"
    );
    Ok(true)
}

tokio::task_local! { static ACTIVE: Arc<Control>; }
thread_local! { static BLOCKING: RefCell<Option<Arc<Control>>> = const { RefCell::new(None) }; }
pub(crate) async fn scope<F: Future>(control: Arc<Control>, future: F) -> F::Output {
    ACTIVE.scope(control, future).await
}
pub(crate) fn current() -> Option<Arc<Control>> {
    ACTIVE
        .try_with(Arc::clone)
        .ok()
        .or_else(|| BLOCKING.with(|slot| slot.borrow().clone()))
}
pub(crate) fn blocking_scope<T>(control: Option<Arc<Control>>, work: impl FnOnce() -> T) -> T {
    struct Restore(Option<Arc<Control>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            BLOCKING.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let _restore = Restore(BLOCKING.with(|slot| slot.replace(control)));
    work()
}

pub(crate) struct ProcessGuard {
    control: Option<Arc<Control>>,
    pid: u32,
}
impl Drop for ProcessGuard {
    fn drop(&mut self) {
        let Some(control) = &self.control else {
            return;
        };
        let mut state = control.state.lock().unwrap();
        if let Some(process) = state.processes.remove(&self.pid) {
            // Cancellation of a paused operation must kill its descendants too.
            if crate::environment::process_identity(self.pid).as_deref() == Some(&process.identity)
                && unsafe { libc::getpgid(self.pid as i32) } == self.pid as i32
            {
                unsafe {
                    libc::kill(-(self.pid as i32), libc::SIGKILL);
                }
            }
            if let Err(error) = control.clear_receipt(self.pid) {
                eprintln!("storage pause receipt cleanup failed: {error}");
            }
        }
    }
}
pub(crate) fn register_process(pid: u32) -> Result<ProcessGuard> {
    let Some(control) = current() else {
        return Ok(ProcessGuard { control: None, pid });
    };
    ensure!(
        pid > 1 && pid <= i32::MAX as u32,
        "invalid storage process PID"
    );
    let group = unsafe { libc::getpgid(pid as i32) };
    if group != pid as i32 {
        let absent =
            group == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if absent || process_absent(pid) {
            return Ok(ProcessGuard { control: None, pid });
        }
        bail!("storage process must own its session group");
    }
    let identity = match crate::environment::process_identity(pid) {
        Some(identity) => identity,
        // Fast children can be reaped between the group and identity probes.
        None if process_absent(pid) => return Ok(ProcessGuard { control: None, pid }),
        None => bail!("cannot identify owned storage process"),
    };
    let mut state = control.state.lock().unwrap();
    ensure!(
        !state.processes.contains_key(&pid),
        "storage process already registered"
    );
    let mut process = Process {
        identity,
        paused: false,
        receipt_synced: true,
    };
    if state.paused.is_some()
        && let Err(error) = control.apply(pid, &mut process, true)
    {
        if !process.paused {
            return Err(error);
        }
        eprintln!("storage hold degraded for process {pid}: {error:#}");
    }
    state.processes.insert(pid, process);
    drop(state);
    Ok(ProcessGuard {
        control: Some(control),
        pid,
    })
}
pub(crate) async fn checkpoint() {
    while current().is_some_and(|control| control.is_paused()) {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
/// Pause-aware deadlines also gate polling, so held workflows cannot launch more work.
pub(crate) async fn timeout<F: Future>(duration: Duration, work: F) -> Result<F::Output> {
    let control = current();
    if control.is_none() {
        return tokio::time::timeout(duration, work)
            .await
            .context("operation timed out");
    }
    let initial = control
        .as_ref()
        .map(|c| c.paused_duration())
        .unwrap_or_default();
    let start = Instant::now();
    tokio::pin!(work);
    loop {
        let paused = control.as_ref().is_some_and(|c| c.is_paused());
        let excluded = control
            .as_ref()
            .map(|c| c.paused_duration().saturating_sub(initial))
            .unwrap_or_default();
        if !paused && start.elapsed().saturating_sub(excluded) >= duration {
            bail!("operation timed out");
        }
        tokio::select! {
            biased;
            result = std::future::poll_fn(|cx| {
                if control.as_ref().is_some_and(|c|c.is_paused()) { Poll::Pending } else { work.as_mut().poll(cx) }
            }) => return Ok(result),
            _ = tokio::time::sleep(Duration::from_millis(10)) => {}
        }
    }
}

#[cfg(test)]
#[path = "../../tests/storage_processes/mod.rs"]
mod tests;
