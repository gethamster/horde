//! Daemon-owned progress budgets. Events retain timing without changing the database layout.
use crate::{config::Settings, store::Store, template::Step};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    future::Future,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::Instant;

fn millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn seconds(settings: &Settings, step: &Step, role: &str) -> u64 {
    step.step_budget_seconds
        .or_else(|| {
            settings
                .executors
                .get(role)
                .and_then(|r| r.step_budget_seconds)
        })
        .unwrap_or(settings.step_budget_seconds)
}

#[derive(Debug)]
pub struct Exhausted(pub Value);
impl std::fmt::Display for Exhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "step budget exhausted")
    }
}
impl std::error::Error for Exhausted {}

/// Record progress a worker reports about itself. A report that arrives after the
/// budget ran out cannot revive the attempt.
pub fn progress(db: &Store, worker: &str, reason: &str, fingerprint: &str) -> Result<()> {
    let Some(a) = db.rows("SELECT a.id,s.id AS step,s.task FROM attempts a JOIN steps s ON a.step=s.id WHERE a.worker=? AND a.state='running' ORDER BY a.started DESC LIMIT 1", &[&worker])?.into_iter().next() else { return Ok(()); };
    let attempt = a["id"].as_str().context("attempt")?;
    let timing = status(db, attempt)?;
    if timing.is_null() || timing["remaining_s"].as_f64().unwrap_or(0.0) <= 0.0 {
        return Ok(());
    }
    record(db, &a, worker, reason, fingerprint)
}

/// Record progress the daemon observed itself. Observation lags the change by the
/// sampling latency, so it is never gated on the remaining budget: a workspace that
/// changed is not idle, and the supervisor rules on the recorded time through `renew`.
fn observed(db: &Store, worker: &str, fingerprint: &str) -> Result<()> {
    let Some(a) = db.rows("SELECT a.id,s.id AS step,s.task FROM attempts a JOIN steps s ON a.step=s.id WHERE a.worker=? AND a.state='running' ORDER BY a.started DESC LIMIT 1", &[&worker])?.into_iter().next() else { return Ok(()); };
    record(db, &a, worker, "workspace_changed", fingerprint)
}

fn record(db: &Store, a: &Value, worker: &str, reason: &str, fingerprint: &str) -> Result<()> {
    let attempt = a["id"].as_str().context("attempt")?;
    let timing = status(db, attempt)?;
    let previous = db.rows("SELECT data FROM events WHERE kind='step.progress' AND json_extract(data,'$.attempt')=? AND json_extract(data,'$.reason')=? ORDER BY seq DESC LIMIT 1", &[&attempt,&reason])?;
    if previous
        .first()
        .and_then(|r| r["data"].as_str())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .is_some_and(|v| v["fingerprint"] == fingerprint)
    {
        return Ok(());
    }
    db.event(a["task"].as_str().context("task")?, "step.progress", json!({"step":a["step"],"attempt":attempt,"worker":worker,"reason":reason,"fingerprint":fingerprint,"at_ms":millis(),"elapsed_s":timing["elapsed_s"],"budget_s":timing["budget_s"],"remaining_s":timing["budget_s"]}))?;
    let control = BLOCKING_CONTROL
        .with(|slot| slot.borrow().clone())
        .or_else(|| COMMAND_CONTROL.try_with(Clone::clone).ok());
    if let Some(control) = control {
        let deadline = std::time::Instant::now()
            .checked_add(Duration::from_secs(
                timing["budget_s"].as_u64().context("budget")?,
            ))
            .context("step budget is too large")?;
        *control.deadline.lock().unwrap() = Some(deadline);
    }
    Ok(())
}

pub fn status(db: &Store, attempt: &str) -> Result<Value> {
    let events = db.rows("SELECT kind,data FROM events WHERE kind IN ('step.budget_started','step.progress','step.budget_finished') AND json_extract(data,'$.attempt')=? ORDER BY seq", &[&attempt])?;
    let mut started = None;
    let mut last = 0;
    for event in events {
        let data: Value = serde_json::from_str(event["data"].as_str().context("event")?)?;
        match event["kind"].as_str() {
            Some("step.budget_started") => {
                last = data["at_ms"].as_u64().unwrap_or(0);
                started = Some(data);
            }
            Some("step.progress") => last = last.max(data["at_ms"].as_u64().unwrap_or(0)),
            Some("step.budget_finished") => return Ok(data["timing"].clone()),
            _ => {}
        }
    }
    let Some(started) = started else {
        return Ok(Value::Null);
    };
    let finished = db
        .rows("SELECT finished FROM attempts WHERE id=?", &[&attempt])?
        .first()
        .and_then(|v| v["finished"].as_u64());
    let end = finished
        .map(|s| s.saturating_mul(1000))
        .unwrap_or_else(millis);
    let elapsed = end.saturating_sub(started["at_ms"].as_u64().unwrap_or(end)) as f64 / 1000.0;
    if started["budget_exempt"] == true {
        return Ok(exempt_timing(elapsed));
    }
    let idle = end.saturating_sub(last) as f64 / 1000.0;
    let budget = started["budget_s"].as_u64().unwrap_or(0);
    Ok(
        json!({"elapsed_s":elapsed,"idle_s":idle,"budget_s":budget,"remaining_s":(budget as f64-idle).max(0.0)}),
    )
}

struct Watcher(tokio::task::JoinHandle<()>);
impl Drop for Watcher {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub async fn supervise<F>(
    db: &Store,
    task: &str,
    step: &str,
    attempt: &str,
    worker: &str,
    budget_s: Option<u64>,
    work: F,
) -> Result<Value>
where
    F: std::future::Future<Output = Result<Value>>,
{
    let start = Instant::now();
    let Some(budget_s) = budget_s else {
        db.event(task, "step.budget_started", json!({"step":step,"attempt":attempt,"worker":worker,"budget_exempt":true,"budget_s":null,"elapsed_s":0,"remaining_s":null,"at_ms":millis()}))?;
        let control = CommandControl {
            deadline: std::sync::Arc::new(std::sync::Mutex::new(None)),
            cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        let result = COMMAND_CONTROL.scope(control, work).await;
        db.event(task, "step.budget_finished", json!({"step":step,"attempt":attempt,"worker":worker,"timing":exempt_timing(start.elapsed().as_secs_f64())}))?;
        return result;
    };
    anyhow::ensure!(budget_s > 0, "step_budget_seconds must be positive");
    // The baseline precedes the clock so the first write the work makes is a change
    // against it, not part of it.
    let observed = std::sync::Arc::new(tokio::sync::Mutex::new(
        workspace_fingerprint(db, worker).await.ok().flatten(),
    ));
    let start = Instant::now();
    let initial_deadline = start
        .checked_add(Duration::from_secs(budget_s))
        .context("step budget is too large")?;
    let started_ms = millis();
    db.event(task, "step.budget_started", json!({"step":step,"attempt":attempt,"worker":worker,"budget_s":budget_s,"elapsed_s":0,"remaining_s":budget_s,"at_ms":started_ms}))?;
    let root = db.root.clone();
    let watched_worker = worker.to_owned();
    let watched = observed.clone();
    let _watcher = Watcher(tokio::task::spawn_local(async move {
        let Ok(db) = Store::open(&root) else {
            return;
        };
        loop {
            let _ = observe(&db, &watched_worker, &watched).await;
            tokio::time::sleep(Duration::from_millis(OBSERVER_INTERVAL_MS)).await;
        }
    }));
    let control = CommandControl {
        deadline: std::sync::Arc::new(std::sync::Mutex::new(Some(initial_deadline.into_std()))),
        cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };
    let deadline_control = control.deadline.clone();
    let work = COMMAND_CONTROL.scope(control, work);
    tokio::pin!(work);
    let mut last_progress = start;
    let mut last_ms = started_ms;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let budget = Duration::from_secs(budget_s);
    // The sample the deadline takes before ruling, while it is in flight. The work
    // keeps being polled underneath it, so a slow sample delays the verdict and never
    // starves the step it is judging.
    let mut sample: Option<std::pin::Pin<Box<dyn Future<Output = Result<()>> + '_>>> = None;
    let result = loop {
        let deadline = last_progress
            .checked_add(budget)
            .context("step budget is too large")?;
        *deadline_control.lock().unwrap() = Some(deadline.into_std());
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline), if sample.is_none() => {
                // A mutation may have committed just before the timer became runnable.
                renew(db, attempt, &mut last_ms, &mut last_progress)?;
                if last_progress.elapsed() < budget { continue; }
                // The poll samples the workspace on its own schedule, which a loaded host
                // stretches without bound. Exhaustion is a claim that nothing changed for a
                // whole budget, so it is ruled on a sample taken now, not on the last one the
                // poll happened to finish.
                sample = Some(Box::pin(observe(db, worker, &observed)));
            }
            _ = async { match sample.as_mut() { Some(s) => s.await, None => std::future::pending().await } }, if sample.is_some() => {
                sample = None;
                renew(db, attempt, &mut last_ms, &mut last_progress)?;
                if last_progress.elapsed() < budget { continue; }
                let timing = timing(start, last_progress, budget_s);
                let value = json!({"error":"step budget exhausted","elapsed_s":timing["elapsed_s"],"idle_s":timing["idle_s"],"budget_s":budget_s});
                db.event(task, "step.budget_exhausted", json!({"step":step,"attempt":attempt,"worker":worker,"error":"step budget exhausted","elapsed_s":timing["elapsed_s"],"idle_s":timing["idle_s"],"budget_s":budget_s,"remaining_s":0}))?;
                break Err(Exhausted(value).into());
            }
            result = &mut work => {
                // Idleness ends when the work does; a sample that returns later than
                // that reports on the workspace the work left behind, not on time it
                // spent after finishing.
                let finished = Instant::now();
                renew(db, attempt, &mut last_ms, &mut last_progress)?;
                if finished.saturating_duration_since(last_progress) >= budget {
                    let _ = match sample.take() {
                        Some(sample) => sample.await,
                        None => observe(db, worker, &observed).await,
                    };
                    renew(db, attempt, &mut last_ms, &mut last_progress)?;
                }
                if finished.saturating_duration_since(last_progress) >= budget {
                    let timing = timing(start, last_progress, budget_s);
                    let value = json!({"error":"step budget exhausted","elapsed_s":timing["elapsed_s"],"idle_s":timing["idle_s"],"budget_s":budget_s});
                    db.event(task, "step.budget_exhausted", json!({"step":step,"attempt":attempt,"worker":worker,"error":"step budget exhausted","elapsed_s":timing["elapsed_s"],"idle_s":timing["idle_s"],"budget_s":budget_s,"remaining_s":0}))?;
                    break Err(Exhausted(value).into());
                }
                break result;
            },
            _ = tick.tick() => { renew(db, attempt, &mut last_ms, &mut last_progress)?; }
        }
    };
    drop(sample);
    // Dropping the work future stops owned async command process groups.
    db.event(task,"step.budget_finished",json!({"step":step,"attempt":attempt,"worker":worker,"timing":timing(start,last_progress,budget_s)}))?;
    result
}
fn exempt_timing(elapsed: f64) -> Value {
    json!({"elapsed_s":elapsed,"idle_s":null,"budget_s":null,"remaining_s":null,"budget_exempt":true})
}
fn timing(start: Instant, progress: Instant, budget: u64) -> Value {
    let idle = progress.elapsed().as_secs_f64();
    json!({"elapsed_s":start.elapsed().as_secs_f64(),"idle_s":idle,"budget_s":budget,"remaining_s":(budget as f64-idle).max(0.0)})
}
fn renew(db: &Store, attempt: &str, last_ms: &mut u64, progress: &mut Instant) -> Result<bool> {
    let latest = db.rows("SELECT data FROM events WHERE kind='step.progress' AND json_extract(data,'$.attempt')=? ORDER BY seq DESC LIMIT 1", &[&attempt])?;
    let at = latest
        .first()
        .and_then(|v| v["data"].as_str())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v["at_ms"].as_u64())
        .unwrap_or(*last_ms);
    if at <= *last_ms {
        return Ok(false);
    }
    *last_ms = at;
    *progress = Instant::now()
        .checked_sub(Duration::from_millis(millis().saturating_sub(at)))
        .unwrap_or_else(Instant::now);
    Ok(true)
}

/// How long the background poll rests between workspace samples. It keeps progress
/// timestamps fresh for `status`; it is not what decides exhaustion.
const OBSERVER_INTERVAL_MS: u64 = 250;

/// Sample the workspace and record a change since the previous sample. Samples are
/// serialized through `previous` so the poll and the deadline check never compare
/// interleaved reads of a workspace that is still changing.
async fn observe(
    db: &Store,
    worker: &str,
    previous: &tokio::sync::Mutex<Option<String>>,
) -> Result<()> {
    let mut previous = previous.lock().await;
    let Some(fingerprint) = workspace_fingerprint(db, worker).await? else {
        return Ok(());
    };
    if previous.as_ref().is_some_and(|p| p != &fingerprint) {
        observed(db, worker, &fingerprint)?;
    }
    *previous = Some(fingerprint);
    Ok(())
}

async fn workspace_fingerprint(db: &Store, worker: &str) -> Result<Option<String>> {
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;
    let worker = db.worker(worker)?;
    let checkout = if let Some(step) = worker["step"].as_str() {
        let rows = db.rows("SELECT spec FROM steps WHERE id=?", &[&step])?;
        let spec = Store::step(rows.first().context("step")?)?;
        spec.workspace == Some(crate::template::CommandWorkspace::Checkout)
    } else {
        false
    };
    let root = if checkout {
        std::path::PathBuf::from(
            db.task(worker["task"].as_str().context("task")?)?["repo"]
                .as_str()
                .context("repo")?,
        )
    } else if let Some(path) = worker["workspace"].as_str() {
        std::path::PathBuf::from(path)
    } else {
        // Command/environment steps execute in the task's integrated worktree.
        db.root
            .join("workspaces")
            .join(worker["task"].as_str().context("task")?)
            .join("integrated")
    };
    if !root.exists() {
        return Ok(None);
    }
    let mut hash = Sha256::new();
    for args in [
        vec!["rev-parse", "HEAD"],
        vec!["diff", "--no-ext-diff", "--binary", "HEAD"],
        vec!["ls-files", "--others", "--exclude-standard", "-z"],
    ] {
        let mut command = crate::executor::clean_command("git");
        command.args(&args).current_dir(&root);
        let output = crate::executor::run_process(command, None, 5, None).await?;
        anyhow::ensure!(output["success"] == true, "workspace scan failed");
        let text = output["stdout"].as_str().context("git output")?;
        hash.update(text.as_bytes());
        if args[0] == "ls-files" {
            for name in text.split('\0').filter(|p| !p.is_empty()) {
                let relative = crate::store::scope(name)?;
                let path = root.join(relative);
                let metadata = tokio::fs::symlink_metadata(&path).await?;
                if metadata.file_type().is_symlink() {
                    hash.update(
                        tokio::fs::read_link(path)
                            .await?
                            .as_os_str()
                            .as_encoded_bytes(),
                    );
                } else if metadata.is_file() {
                    let mut file = tokio::fs::File::open(path).await?;
                    let mut bytes = [0; 65536];
                    loop {
                        let n = file.read(&mut bytes).await?;
                        if n == 0 {
                            break;
                        }
                        hash.update(&bytes[..n]);
                    }
                }
            }
        }
    }
    Ok(Some(hex::encode(hash.finalize())))
}

/// Add timing to inspect/metrics without changing the authoritative attempt rows.
pub fn annotate(db: &Store, mut attempts: Vec<Value>) -> Result<Vec<Value>> {
    for attempt in &mut attempts {
        let mut timing = status(db, attempt["id"].as_str().context("attempt")?)?;
        if timing.is_null() {
            let end = attempt["finished"]
                .as_i64()
                .unwrap_or_else(crate::store::now);
            let elapsed = (end - attempt["started"].as_i64().unwrap_or(end)).max(0);
            timing = json!({"elapsed_s":elapsed,"idle_s":null,"budget_s":null,"remaining_s":null});
        }
        attempt["timing"] = timing;
    }
    Ok(attempts)
}

#[derive(Clone)]
struct CommandControl {
    deadline: std::sync::Arc<std::sync::Mutex<Option<std::time::Instant>>>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
tokio::task_local! { static COMMAND_CONTROL: CommandControl; }
thread_local! { static BLOCKING_CONTROL: std::cell::RefCell<Option<CommandControl>> = const { std::cell::RefCell::new(None) }; }

/// Keep synchronous Git work off the daemon event loop and cancel its process group
/// if the awaiting attempt is dropped or its shared deadline expires.
pub async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    struct Cancel(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Cancel {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _cancel = Cancel(cancelled.clone());
    let control = COMMAND_CONTROL
        .try_with(|c| CommandControl {
            deadline: c.deadline.clone(),
            cancelled,
        })
        .ok();
    tokio::task::spawn_blocking(move || {
        BLOCKING_CONTROL.with(|slot| *slot.borrow_mut() = control);
        let result = work();
        BLOCKING_CONTROL.with(|slot| *slot.borrow_mut() = None);
        result
    })
    .await
    .context("blocking step operation")?
}

pub fn command_output(command: &mut std::process::Command) -> Result<std::process::Output> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::sync::atomic::Ordering;
    let Some(control) = BLOCKING_CONTROL.with(|slot| slot.borrow().clone()) else {
        return Ok(command.output()?);
    };
    let expired = || {
        control.cancelled.load(Ordering::SeqCst)
            || control
                .deadline
                .lock()
                .unwrap()
                .is_some_and(|deadline| std::time::Instant::now() >= deadline)
    };
    anyhow::ensure!(!expired(), "step budget exhausted");
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    struct Group(u32);
    impl Drop for Group {
        fn drop(&mut self) {
            unsafe {
                libc::kill(-(self.0 as i32), libc::SIGKILL);
            }
        }
    }
    let _group = Group(child.id());
    let stdout = child.stdout.take().context("stdout")?;
    let stderr = child.stderr.take().context("stderr")?;
    std::thread::scope(|scope| {
        // Drop before scoped reader joins, including on try_wait errors.
        let _join_guard = Group(child.id());
        let read = |pipe: Box<dyn Read + Send>| -> Result<Vec<u8>> {
            let mut bytes = vec![];
            pipe.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            anyhow::ensure!(bytes.len() <= 8 * 1024 * 1024, "Git output exceeds 8 MiB");
            Ok(bytes)
        };
        let out = scope.spawn(move || read(Box::new(stdout)));
        let err = scope.spawn(move || read(Box::new(stderr)));
        let status = loop {
            if expired() {
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.wait();
                anyhow::bail!("step budget exhausted");
            }
            if let Some(status) = child.try_wait()? {
                break status;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        // Hooks may have left descendants holding the output pipes open.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        Ok(std::process::Output {
            status,
            stdout: out
                .join()
                .map_err(|_| anyhow::anyhow!("Git stdout reader"))??,
            stderr: err
                .join()
                .map_err(|_| anyhow::anyhow!("Git stderr reader"))??,
        })
    })
}
