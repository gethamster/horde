//! A disposable runtime check without provider calls or user runtime state.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(15);
const INTERVAL: Duration = Duration::from_millis(50);

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Exercise the actual daemon scheduler in a fresh repository and private runtime.
/// Simulated steps do not verify provider authentication or worktree integration.
pub fn verify(binary: &Path) -> Result<Value> {
    let temporary = tempfile::Builder::new()
        .prefix("horde-smoke-")
        .tempdir_in("/tmp")?;
    let home = temporary.path().join("home");
    let repo = temporary.path().join("repo");
    let data = temporary.path().join("data");
    prepare(&home, &repo)?;
    // Explicitly activate an empty local catalog: simulated steps need no skills,
    // and a relocated executable must never fetch a release pack during this check.
    std::fs::create_dir(&data)?;
    crate::skill_catalog::install(&data, &crate::skills::Packet::new())?;
    let log = std::fs::File::create(temporary.path().join("daemon.log"))?;
    let mut daemon = Process(
        isolated(binary, &home)
            .arg("--data-dir")
            .arg(&data)
            .arg("daemon")
            .current_dir(&repo)
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .context("start isolated smoke daemon")?,
    );
    let deadline = Instant::now() + TIMEOUT;
    wait_ready(&mut daemon, &data, deadline)?;
    let submitted = request(
        &data,
        "submit_task",
        json!({"objective":"Verify the isolated runtime", "repo":repo, "template":"simulated"}),
    )?;
    let task = submitted["id"].as_str().context("smoke task ID missing")?;
    wait_completed(&mut daemon, &data, task, deadline)
}

fn isolated(program: &Path, home: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::null());
    command
}

fn prepare(home: &Path, repo: &Path) -> Result<()> {
    std::fs::create_dir_all(home.join(".config/horde"))?;
    std::fs::create_dir_all(repo)?;
    let defaults = crate::config::Settings::default();
    let settings = crate::config::Settings {
        providers: defaults
            .providers
            .keys()
            .map(|name| {
                (
                    name.clone(),
                    crate::config::Provider {
                        kind: "simulated".into(),
                        ..Default::default()
                    },
                )
            })
            .collect(),
        executors: defaults
            .executors
            .keys()
            .map(|role| {
                (
                    role.clone(),
                    crate::config::Executor {
                        provider: Some("simulated".into()),
                        ..Default::default()
                    },
                )
            })
            .collect(),
        default_template: "simulated".into(),
        allow_commands: false,
        ..defaults
    };
    std::fs::write(
        home.join(".config/horde/config.toml"),
        toml::to_string(&settings)?,
    )?;
    git(home, repo, &["init", "-q"])?;
    git(home, repo, &["config", "user.name", "Horde smoke check"])?;
    git(home, repo, &["config", "user.email", "smoke@horde.invalid"])?;
    git(
        home,
        repo,
        &["commit", "--allow-empty", "-qm", "Smoke fixture"],
    )
}

fn git(home: &Path, repo: &Path, args: &[&str]) -> Result<()> {
    let mut child = Process(
        isolated(Path::new("git"), home)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start Git for smoke fixture")?,
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = child.0.try_wait()? {
            ensure!(
                status.success(),
                "Git smoke fixture command failed: {status}"
            );
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Git smoke fixture command timed out"
        );
        std::thread::sleep(INTERVAL);
    }
}

fn running(daemon: &mut Process, deadline: Instant) -> Result<()> {
    if let Some(status) = daemon.0.try_wait()? {
        bail!("smoke daemon exited: {status}");
    }
    ensure!(Instant::now() < deadline, "isolated smoke check timed out");
    Ok(())
}

fn wait_ready(daemon: &mut Process, data: &Path, deadline: Instant) -> Result<()> {
    loop {
        running(daemon, deadline)?;
        if request(data, "list_tasks", json!({})).is_ok() {
            return Ok(());
        }
        std::thread::sleep(INTERVAL);
    }
}

fn wait_completed(
    daemon: &mut Process,
    data: &Path,
    task: &str,
    deadline: Instant,
) -> Result<Value> {
    loop {
        running(daemon, deadline)?;
        let inspected = request(data, "inspect", json!({"task":task}))?;
        let status = inspected["task"]["status"].as_str().unwrap_or("");
        if status == "succeeded" {
            let steps = inspected["steps"]
                .as_array()
                .context("smoke steps missing")?;
            ensure!(
                steps.len() == 4 && steps.iter().all(|step| step["state"] == "succeeded"),
                "smoke task completed without four successful steps"
            );
            return Ok(json!({
                "status":"passed", "steps":steps.len(), "provider_calls":0,
                "scope":"scheduler_and_task_completion"
            }));
        }
        ensure!(
            !["failed", "blocked", "cancelled"].contains(&status),
            "smoke task ended with status {status}"
        );
        std::thread::sleep(INTERVAL);
    }
}

fn request(data: &Path, method: &str, args: Value) -> Result<Value> {
    let mut stream = UnixStream::connect(data.join("daemon.sock"))?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    writeln!(stream, "{}", json!({"method":method,"args":args}))?;
    let mut line = String::new();
    BufReader::new(stream.take(1024 * 1024 + 1)).read_line(&mut line)?;
    ensure!(line.len() <= 1024 * 1024, "smoke response exceeds 1 MiB");
    let response: Value = serde_json::from_str(&line)?;
    if let Some(error) = response.get("error") {
        bail!("smoke request {method} failed: {error}");
    }
    response
        .get("result")
        .cloned()
        .context("smoke response result missing")
}
