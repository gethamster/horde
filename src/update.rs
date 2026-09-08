//! Signed, versioned installs. The release key is embedded by release CI.
use crate::{management, store::Store};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub target: String,
    pub sha256: String,
    pub url: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: String,
    pub protocol: u32,
    pub schema_min: u32,
    pub schema_max: u32,
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub image: Option<String>,
}
pub fn verify(bytes: &[u8], signature: &[u8], key: &[u8]) -> Result<Manifest> {
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(bytes, signature)
        .map_err(|_| anyhow::anyhow!("release signature verification failed"))?;
    let manifest: Manifest = serde_json::from_slice(bytes)?;
    validate_version(&manifest.version)?;
    ensure!(
        manifest.protocol == 1
            && manifest.schema_min <= crate::store::SCHEMA_VERSION
            && manifest.schema_max >= crate::store::SCHEMA_VERSION,
        "release is incompatible with this runtime's protocol or database"
    );
    Ok(manifest)
}
pub fn validate_version(v: &str) -> Result<()> {
    ensure!(
        !v.is_empty()
            && v.len() < 64
            && v.chars()
                .all(|c| c.is_ascii_alphanumeric() || ".-".contains(c))
            && v.as_bytes()[0].is_ascii_digit(),
        "invalid release version"
    );
    Ok(())
}
fn version_core(value: &str) -> Result<Vec<u64>> {
    let core = value.split('-').next().context("invalid version")?;
    let numbers: Vec<u64> = core
        .split('.')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    ensure!(
        numbers.len() == 3,
        "version must have major.minor.patch components"
    );
    Ok(numbers)
}
pub fn target() -> Result<String> {
    let os = match std::env::consts::OS {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-musl",
        _ => bail!("unsupported platform"),
    };
    ensure!(
        ["aarch64", "x86_64"].contains(&std::env::consts::ARCH),
        "unsupported architecture"
    );
    Ok(format!("{}-{os}", std::env::consts::ARCH))
}
async fn download(client: &reqwest::Client, url: &str, max: usize) -> Result<Vec<u8>> {
    ensure!(
        url.starts_with("https://horde.sh/releases/"),
        "release URL is outside the trusted horde.sh release path"
    );
    let mut response = client.get(url).send().await?.error_for_status()?;
    ensure!(
        response.content_length().is_none_or(|n| n <= max as u64),
        "release download too large"
    );
    let mut bytes = vec![];
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= max,
            "release download too large"
        );
        bytes.extend(chunk);
    }
    Ok(bytes)
}
fn install_root() -> Result<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME required")?);
    Ok(crate::branding::install_dir(&home))
}

fn request(root: &Path, name: &str) -> Result<Value> {
    use std::io::{BufRead, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(root.join("daemon.sock"))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    writeln!(stream, "{}", json!({"method":name,"args":{}}))?;
    let mut line = String::new();
    std::io::BufReader::new(stream).read_line(&mut line)?;
    let v: Value = serde_json::from_str(&line)?;
    ensure!(
        v["error"].is_null(),
        "runtime request failed: {}",
        v["error"]
    );
    Ok(v["result"].clone())
}
fn phase(db: &Store, state: &str, version: &str) -> Result<()> {
    management::set(db, "update_state", state)?;
    management::event(
        db,
        "update.progress",
        json!({"state":state,"version":version}),
    )
}
// The service manager may kill the updater child when its daemon exits. The
// replacement daemon owns completion, with the helper only observing its health.
#[derive(Debug, Serialize, Deserialize)]
struct Handoff {
    version: String,
    executable: PathBuf,
    operation: Option<String>,
    state: String,
}

fn prepare_handoff(
    db: &Store,
    version: &str,
    executable: &Path,
    operation: Option<&str>,
) -> Result<()> {
    let intent = Handoff {
        version: version.into(),
        executable: executable.canonicalize()?,
        operation: operation.map(str::to_owned),
        state: "pending".into(),
    };
    management::set(db, "update_handoff", &serde_json::to_string(&intent)?)
}

/// Called once startup has bound its interfaces and completed recovery, before
/// scheduling work. An old or unexpected binary must leave the runtime held.
pub fn complete_handoff(db: &Store) -> Result<()> {
    complete_handoff_as(db, env!("CARGO_PKG_VERSION"), &std::env::current_exe()?)
}

fn complete_handoff_as(db: &Store, version: &str, executable: &Path) -> Result<()> {
    use rusqlite::OptionalExtension;
    let tx =
        rusqlite::Transaction::new_unchecked(&db.conn, rusqlite::TransactionBehavior::Immediate)?;
    let value: Option<String> = tx
        .query_row(
            "SELECT value FROM runtime_settings WHERE key='update_handoff'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let Some(value) = value else {
        return Ok(());
    };
    let mut intent: Handoff = serde_json::from_str(&value)?;
    if intent.state != "pending" {
        return Ok(());
    }
    use std::os::unix::fs::MetadataExt;
    let identity = |path: &Path| std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()));
    let expected = identity(&intent.executable);
    let matches =
        version == intent.version && expected.is_some() && identity(executable) == expected;
    intent.state = if matches { "completed" } else { "blocked" }.into();
    let evidence = if matches {
        json!({"version":version,"updated":true,"executable":executable})
    } else {
        json!({"error":"update startup identity mismatch; runtime held for inspection", "expected_version":intent.version,"version":version,"executable":executable})
    };
    for (key, value) in [
        ("update_handoff", serde_json::to_string(&intent)?),
        (
            "update_state",
            if matches { "healthy" } else { "blocked" }.into(),
        ),
        ("draining", if matches { "false" } else { "true" }.into()),
    ] {
        tx.execute("INSERT INTO runtime_settings VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", rusqlite::params![key,value])?;
    }
    if let Some(id) = &intent.operation {
        tx.execute("UPDATE runtime_operations SET state=?,result=? WHERE id=? AND runtime='local' AND action='runtime_update' AND state='running'",
            rusqlite::params![if matches { "succeeded" } else { "blocked" }, evidence.to_string(), id])?;
    }
    tx.execute(
        "INSERT INTO management_events(kind,data,created) VALUES('update.startup',?,?)",
        rusqlite::params![evidence.to_string(), crate::store::now()],
    )?;
    tx.commit()?;
    Ok(())
}

fn handoff_completed(db: &Store) -> Result<bool> {
    Ok(management::value(db, "update_handoff")?
        .map(|v| serde_json::from_str::<Handoff>(&v))
        .transpose()?
        .is_some_and(|intent| intent.state == "completed"))
}

// Serializes timeout/rollback with replacement startup so a late successful
// startup cannot resume work after the helper has decided to roll back.
fn cancel_handoff(db: &Store) -> Result<bool> {
    let tx =
        rusqlite::Transaction::new_unchecked(&db.conn, rusqlite::TransactionBehavior::Immediate)?;
    let value: String = tx.query_row(
        "SELECT value FROM runtime_settings WHERE key='update_handoff'",
        [],
        |r| r.get(0),
    )?;
    let mut intent: Handoff = serde_json::from_str(&value)?;
    if intent.state == "completed" {
        return Ok(false);
    }
    intent.state = "blocked".into();
    tx.execute(
        "UPDATE runtime_settings SET value=? WHERE key='update_handoff'",
        [serde_json::to_string(&intent)?],
    )?;
    tx.commit()?;
    Ok(true)
}

pub async fn run(
    root: &Path,
    version: Option<&str>,
    check: bool,
    operation: Option<&str>,
) -> Result<Value> {
    let key = option_env!("HORDE_RELEASE_PUBLIC_KEY").context(
        "this build has no release verification key; install an official signed release",
    )?;
    let key = hex::decode(key).context("invalid embedded release key")?;
    if let Some(v) = version {
        validate_version(v)?;
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()?;
    let base = version
        .map(|v| format!("https://horde.sh/releases/v{v}"))
        .unwrap_or_else(|| "https://horde.sh/releases/latest".into());
    let bytes = download(&client, &format!("{base}/manifest.json"), 1024 * 1024).await?;
    let sig = download(&client, &format!("{base}/manifest.sig"), 64).await?;
    let manifest = verify(&bytes, &sig, &key)?;
    ensure!(
        version.is_none_or(|v| v == manifest.version),
        "requested release version mismatch"
    );
    if version.is_none() {
        ensure!(
            !manifest.version.contains('-'),
            "prerelease requires explicit version"
        );
    }
    let available = if version.is_some() {
        manifest.version != env!("CARGO_PKG_VERSION")
    } else {
        let target = version_core(&manifest.version)?;
        let current = version_core(env!("CARGO_PKG_VERSION"))?;
        target > current || (target == current && env!("CARGO_PKG_VERSION").contains('-'))
    };
    if check {
        return Ok(
            json!({"installed":env!("CARGO_PKG_VERSION"),"available":manifest.version,"update_available":available}),
        );
    }
    if version.is_none() && !available {
        return Ok(json!({"version":env!("CARGO_PKG_VERSION"),"updated":false}));
    }
    let install = install_root()?;
    ensure!(
        install.join("current").exists(),
        "installation is not managed by install.sh; update using its owning installer"
    );
    std::fs::create_dir_all(&install)?;
    use fs2::FileExt;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(install.join("update.lock"))?;
    lock.try_lock_exclusive()
        .context("another update is in progress")?;
    let db = Store::open(root)?;
    phase(&db, "downloading", &manifest.version)?;
    let artifact = manifest
        .artifacts
        .iter()
        .find(|a| a.target == target().unwrap_or_default())
        .context("release has no artifact for this platform")?;
    let archive = download(&client, &artifact.url, 256 * 1024 * 1024).await?;
    ensure!(
        crate::store::hash(&archive) == artifact.sha256,
        "release artifact checksum mismatch"
    );
    let stage = install.join(format!("staging-{}", crate::store::id()));
    std::fs::create_dir(&stage)?;
    let mut tar = tar::Archive::new(archive.as_slice());
    let mut found = false;
    for e in tar.entries()? {
        let mut e = e?;
        ensure!(
            e.path()?.as_ref() == Path::new("horde")
                && e.header().entry_type().is_file()
                && e.size() <= 256 * 1024 * 1024,
            "unexpected release archive entry"
        );
        ensure!(!found, "duplicate release binary");
        e.unpack(stage.join("horde"))?;
        found = true;
    }
    ensure!(found, "release binary missing");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(stage.join("horde"), std::fs::Permissions::from_mode(0o755))?;
    std::fs::File::open(stage.join("horde"))?.sync_all()?;
    let output = std::process::Command::new(stage.join("horde"))
        .arg("--version")
        .output()?;
    ensure!(
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).trim()
                == format!("horde {}", manifest.version),
        "release binary version check failed"
    );
    phase(&db, "verified", &manifest.version)?;
    let was_running = request(root, "runtime_status").is_ok();
    if was_running {
        request(root, "runtime_drain")?;
        phase(&db, "draining", &manifest.version)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1800);
        loop {
            if request(root, "runtime_status")?["drained"] == true {
                break;
            }
            if std::time::Instant::now() >= deadline {
                phase(&db, "blocked", &manifest.version)?;
                bail!(
                    "update drain timed out; runtime remains draining; use horde runtime resume to cancel"
                );
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
    let backup = root.join(format!("pre-update-{}.sqlite3", crate::store::id()));
    db.conn
        .execute("VACUUM INTO ?", [backup.to_str().context("backup path")?])?;
    let previous = std::fs::read_link(install.join("current"))?;
    let release = install.join(format!("{}-{}", manifest.version, crate::store::id()));
    std::fs::rename(&stage, &release)?;
    if was_running {
        prepare_handoff(&db, &manifest.version, &release.join("horde"), operation)?;
    }
    let link = install.join("next");
    if link.symlink_metadata().is_ok() {
        std::fs::remove_file(&link)?;
    }
    std::os::unix::fs::symlink(&release, &link)?;
    std::fs::rename(&link, install.join("current"))?;
    std::fs::File::open(&install)?.sync_all()?;
    phase(&db, "restarting", &manifest.version)?;
    if was_running {
        management::set(&db, "restart_requested", "true")?;
        request(root, "shutdown")?;
        for _ in 0..300 {
            if request(root, "runtime_status").is_err() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        if management::value(&db, "service_installed")?.as_deref() != Some("true")
            && crate::branding::var("HORDE_SUPERVISED").as_deref() != Ok("1")
        {
            let log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(root.join("daemon.log"))?;
            std::process::Command::new(release.join("horde"))
                .args(["--data-dir"])
                .arg(root)
                .arg("start")
                .stdin(std::process::Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log)
                .spawn()?;
        }
        for _ in 0..300 {
            if let Ok(v) = request(root, "runtime_status")
                && v["version"] == manifest.version
                && handoff_completed(&db)?
            {
                request(root, "runtime_resume")?;
                phase(&db, "healthy", &manifest.version)?;
                return Ok(json!({"version":manifest.version,"updated":true}));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        if !cancel_handoff(&db)? {
            return Ok(json!({"version":manifest.version,"updated":true}));
        }
        let schema: i64 = db.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if schema > i64::from(crate::store::SCHEMA_VERSION) {
            phase(&db, "failed", &manifest.version)?;
            bail!(
                "updated runtime failed health and migrated the database; automatic rollback is unsafe; retain the new binary and inspect the pre-update backup"
            );
        }
        // Roll back only while the stored schema remains readable by this binary.
        std::os::unix::fs::symlink(previous, &link)?;
        std::fs::rename(link, install.join("current"))?;
        phase(&db, "failed", &manifest.version)?;
        bail!(
            "updated runtime failed health check; previous launcher restored; inspect daemon.log and restart service"
        );
    }
    phase(&db, "healthy", &manifest.version)?;
    Ok(json!({"version":manifest.version,"updated":true}))
}

pub async fn release(version: &str) -> Result<Manifest> {
    validate_version(version)?;
    let key = hex::decode(
        option_env!("HORDE_RELEASE_PUBLIC_KEY")
            .context("this build has no release verification key")?,
    )?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()?;
    let base = format!("https://horde.sh/releases/v{version}");
    let bytes = download(&client, &format!("{base}/manifest.json"), 1024 * 1024).await?;
    let signature = download(&client, &format!("{base}/manifest.sig"), 64).await?;
    let manifest = verify(&bytes, &signature, &key)?;
    ensure!(manifest.version == version, "release version mismatch");
    Ok(manifest)
}

#[cfg(test)]
mod handoff_tests {
    use super::*;

    fn pending(root: &Path, executable: &Path) -> Result<()> {
        let db = Store::open(root)?;
        management::set(&db, "draining", "true")?;
        db.conn.execute("INSERT INTO runtime_operations(id,runtime,action,args,state,created) VALUES('parent:update','local','runtime_update','{}','running',0)", [])?;
        prepare_handoff(&db, "0.2.1", executable, Some("parent:update"))
    }

    fn operation(db: &Store) -> String {
        db.conn
            .query_row(
                "SELECT state FROM runtime_operations WHERE id='parent:update'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn replacement_finishes_durable_update_without_original_helper() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let executable = temp.path().join("new-task");
        std::fs::write(&executable, b"fixture")?;
        let root = temp.path().join("data");
        pending(&root, &executable)?; // Drop original connection, as when helper is killed.
        let db = Store::open(&root)?;
        let alias = temp.path().join("alias-task");
        std::fs::hard_link(&executable, &alias)?;
        complete_handoff_as(&db, "0.2.1", &alias)?;
        assert_eq!(operation(&db), "succeeded");
        assert!(!management::draining(&db)?);
        assert_eq!(
            management::value(&db, "update_state")?.as_deref(),
            Some("healthy")
        );
        complete_handoff_as(&db, "0.2.1", &executable)?;
        let count: i64 = db.conn.query_row(
            "SELECT COUNT(*) FROM management_events WHERE kind='update.startup'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(count, 1);
        assert!(
            !cancel_handoff(&db)?,
            "timeout must not roll back completed startup"
        );
        Ok(())
    }

    #[test]
    fn mismatched_startup_stays_held_and_cannot_replay_completion() -> Result<()> {
        for wrong_version in [true, false] {
            let temp = tempfile::tempdir()?;
            let executable = temp.path().join("new-task");
            let old = temp.path().join("old-task");
            std::fs::write(&executable, b"new")?;
            std::fs::write(&old, b"old")?;
            let root = temp.path().join("data");
            pending(&root, &executable)?;
            let db = Store::open(&root)?;
            complete_handoff_as(
                &db,
                if wrong_version { "0.2.0" } else { "0.2.1" },
                if wrong_version { &executable } else { &old },
            )?;
            assert_eq!(operation(&db), "blocked");
            assert!(management::draining(&db)?);
            complete_handoff_as(&db, "0.2.1", &executable)?;
            assert_eq!(operation(&db), "blocked");
            assert!(management::draining(&db)?);
        }
        Ok(())
    }

    #[test]
    fn timed_out_handoff_cannot_resume_after_rollback() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let executable = temp.path().join("new-task");
        std::fs::write(&executable, b"new")?;
        let root = temp.path().join("data");
        pending(&root, &executable)?;
        let db = Store::open(&root)?;
        assert!(cancel_handoff(&db)?);
        complete_handoff_as(&db, "0.2.1", &executable)?;
        assert_eq!(operation(&db), "running");
        assert!(management::draining(&db)?);
        assert!(!handoff_completed(&db)?);
        Ok(())
    }
}
