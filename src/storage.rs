//! Host-owned disk policy; repositories cannot relax admission or retention.
use crate::{management, store::Store};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

mod cleanup;
pub(crate) mod control;
pub(crate) mod pressure;
pub use cleanup::{cleanup, restore};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub min_free_bytes: u64,
    pub warning_free_bytes: Option<u64>,
    pub resume_free_bytes: Option<u64>,
    pub watch_paths: Vec<PathBuf>,
    pub cleanup_command: Vec<String>,
    pub cleanup_timeout_seconds: u64,
    pub retention_seconds: u64,
    pub automatic_cleanup: bool,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            min_free_bytes: 2 * 1024_u64.pow(3),
            warning_free_bytes: None,
            resume_free_bytes: None,
            watch_paths: vec![],
            cleanup_command: vec![],
            cleanup_timeout_seconds: 30,
            retention_seconds: 7 * 86400,
            automatic_cleanup: true,
        }
    }
}
impl Policy {
    fn warning(&self) -> u64 {
        self.warning_free_bytes
            .unwrap_or(self.min_free_bytes + 8 * 1024_u64.pow(3))
    }
    fn resume(&self) -> u64 {
        self.resume_free_bytes
            .unwrap_or(self.warning() + 4 * 1024_u64.pow(3))
    }
}
pub fn policy(db: &Store) -> Result<Policy> {
    let policy = management::value(db, "storage.policy")?
        .map(|value| serde_json::from_str(&value))
        .transpose()?
        .unwrap_or_default();
    validate(&policy)?;
    Ok(policy)
}
fn validate(policy: &Policy) -> Result<()> {
    ensure!(
        (1024 * 1024..=1024_u64.pow(5)).contains(&policy.min_free_bytes),
        "min_free_bytes must be between 1 MiB and 1 PiB"
    );
    ensure!(
        (3600..=3650 * 86400).contains(&policy.retention_seconds),
        "retention_seconds must be between one hour and ten years"
    );
    ensure!(
        policy.warning() >= policy.min_free_bytes && policy.warning() <= 2 * 1024_u64.pow(5),
        "warning_free_bytes must be at least min_free_bytes and at most 2 PiB"
    );
    ensure!(
        policy.resume() > policy.warning() && policy.resume() <= 3 * 1024_u64.pow(5),
        "resume_free_bytes must exceed warning_free_bytes and be at most 3 PiB"
    );
    ensure!(
        (1..=300).contains(&policy.cleanup_timeout_seconds),
        "cleanup_timeout_seconds must be between 1 and 300"
    );
    ensure!(
        policy.watch_paths.len() <= 32 && policy.watch_paths.iter().all(|path| path.is_absolute()),
        "watch_paths must contain at most 32 absolute paths"
    );
    ensure!(
        policy.cleanup_command.len() <= 64
            && policy
                .cleanup_command
                .iter()
                .all(|s| s.len() <= 4096 && !s.contains('\0')),
        "invalid cleanup command arguments"
    );
    ensure!(
        policy
            .cleanup_command
            .first()
            .is_none_or(|program| Path::new(program).is_absolute()),
        "cleanup_command executable must be an absolute path"
    );
    Ok(())
}
fn volume(path: &Path, policy: &Policy) -> Value {
    let result = (|| -> Result<Value> {
        let existing = path
            .ancestors()
            .find(|parent| parent.exists())
            .context("storage path has no existing ancestor")?;
        let available = fs2::available_space(existing)?;
        let total = fs2::total_space(existing)?;
        Ok(
            json!({"path":path,"available_bytes":available,"total_bytes":total,"pressure":available < policy.warning()}),
        )
    })();
    result.unwrap_or_else(|error| json!({"path":path,"available_bytes":null,"total_bytes":null,"pressure":true,"error":error.to_string()}))
}
fn report(db: &Store, paths: Vec<PathBuf>) -> Result<Value> {
    let policy = policy(db)?;
    let volumes: Vec<_> = paths.iter().map(|path| volume(path, &policy)).collect();
    let previous = pressure::saved(db)?;
    let free = volumes
        .iter()
        .map(|v| v["available_bytes"].as_u64())
        .collect::<Option<Vec<_>>>()
        .and_then(|v| v.into_iter().min());
    let state = pressure::next(
        previous["state"].as_str().unwrap_or("healthy"),
        free,
        &policy,
    );
    Ok(
        json!({"policy":policy,"state":state,"pressure":state!="healthy","paused":state=="paused","warning_free_bytes":policy.warning(),"resume_free_bytes":policy.resume(),"volumes":volumes}),
    )
}
pub fn status(db: &Store) -> Result<Value> {
    let mut paths = vec![db.root.clone()];
    paths.extend(policy(db)?.watch_paths);
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(home.into());
    }
    for row in db.rows("SELECT DISTINCT t.id,t.repo,w.workspace FROM attempts a JOIN steps s ON s.id=a.step JOIN tasks t ON t.id=s.task JOIN workers w ON w.id=a.worker WHERE a.state IN ('running','uncertain')", &[])? {
        paths.push(PathBuf::from(row["repo"].as_str().context("repository")?));
        paths.push(crate::project_runtime::task_root(db, row["id"].as_str().context("task")?)?);
        if let Some(workspace) = row["workspace"].as_str().filter(|s| !s.is_empty()) { paths.push(workspace.into()); }
    }
    paths.sort();
    paths.dedup();
    let mut value = report(db, paths)?;
    value["incident"] = pressure::saved(db)?;
    value["process_holds"] = json!(db.rows(
        "SELECT key,value FROM runtime_settings WHERE key LIKE 'storage.pause:%'",
        &[]
    )?);
    value["cleanup_hook"] = management::value(db, "storage.cleanup_hook")?
        .map(|v| serde_json::from_str::<Value>(&v))
        .transpose()?
        .unwrap_or(Value::Null);
    value["last_cleanup"] = management::value(db, "storage.last_cleanup")?
        .map(|value| serde_json::from_str(&value))
        .transpose()?
        .unwrap_or(Value::Null);
    Ok(value)
}
pub fn admission(db: &Store, task: &str) -> Result<bool> {
    let row = db.task(task)?;
    let mut paths = vec![
        db.root.clone(),
        crate::project_runtime::task_root(db, task)?,
        PathBuf::from(row["repo"].as_str().context("repository")?),
    ];
    paths.sort();
    paths.dedup();
    let report = report(db, paths)?;
    if report["pressure"] == true || status(db)?["pressure"] == true {
        crate::project_runtime::queue(
            db,
            task,
            "disk space below runtime reserve or unavailable; waiting for storage recovery",
        )?;
        return Ok(false);
    }
    Ok(true)
}
pub fn dispatch(db: &Store, name: &str, args: &Value) -> Result<Option<Value>> {
    let value = match name {
        "runtime_storage_status" => status(db)?,
        "runtime_storage_configure" => {
            let mut updated = serde_json::to_value(policy(db)?)?;
            let fields = args
                .as_object()
                .context("storage settings must be an object")?;
            ensure!(
                fields.keys().any(|key| key != "project"),
                "storage setting required"
            );
            for (key, value) in fields {
                if key != "project" {
                    ensure!(updated.get(key).is_some(), "unknown storage setting: {key}");
                    updated[key] = value.clone();
                }
            }
            let updated: Policy = serde_json::from_value(updated)?;
            validate(&updated)?;
            db.atomic(|| {
                management::set(db, "storage.policy", &serde_json::to_string(&updated)?)?;
                management::event(db, "storage.configured", serde_json::to_value(&updated)?)
            })?;
            serde_json::to_value(updated)?
        }
        "runtime_storage_cleanup" => {
            let dry_run = args
                .get("dry_run")
                .map(|v| v.as_bool().context("dry_run must be boolean"))
                .transpose()?
                .unwrap_or(true);
            let limit = args
                .get("limit")
                .map(|v| v.as_u64().context("limit must be an integer"))
                .transpose()?
                .unwrap_or(16);
            ensure!(
                (1..=100).contains(&limit),
                "limit must be between 1 and 100"
            );
            cleanup(db, policy(db)?.retention_seconds, limit as usize, dry_run)?
        }
        _ => return Ok(None),
    };
    Ok(Some(value))
}

/// Called once per minute by the daemon; retains the most recent report only.
pub fn maintain(db: &Store) -> Result<()> {
    let policy = policy(db)?;
    if policy.automatic_cleanup {
        let result = cleanup(db, policy.retention_seconds, 16, false)?;
        management::set(db, "storage.last_cleanup", &result.to_string())?;
    }
    Ok(())
}
