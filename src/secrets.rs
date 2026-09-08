//! Application bundles are references in SQLite; values stay in private source files.
use crate::store::{Store, hash};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecretConfig {
    pub bundles: BTreeMap<String, PathBuf>,
}
pub fn config_path() -> PathBuf {
    crate::branding::config_dir().join("secrets.toml")
}
pub fn parse(text: &str) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (key, value) = line
            .split_once('=')
            .context("application bundle requires KEY=VALUE lines")?;
        let key = key.trim();
        ensure!(
            !key.is_empty()
                && key.bytes().enumerate().all(|(i, c)| c == b'_'
                    || c.is_ascii_alphabetic()
                    || i > 0 && c.is_ascii_digit()),
            "invalid environment variable name"
        );
        ensure!(!key.starts_with("HORDE_"), "HORDE_ variables are reserved");
        let value = value.trim();
        let value = if value.starts_with('"') || value.starts_with('\'') {
            ensure!(
                value.len() >= 2 && value.as_bytes().first() == value.as_bytes().last(),
                "unterminated quoted environment value"
            );
            value[1..value.len() - 1].to_owned()
        } else {
            value.split(" #").next().unwrap_or("").trim_end().to_owned()
        };
        ensure!(
            !value.contains('\0'),
            "NUL is not supported in environment values"
        );
        ensure!(
            values.insert(key.into(), value).is_none(),
            "duplicate environment variable"
        );
    }
    Ok(values)
}
pub fn load_bundle(name: &str) -> Result<(String, BTreeMap<String, String>)> {
    let file = config_path();
    let config: SecretConfig = toml::from_str(
        &std::fs::read_to_string(&file)
            .context("configure application bundles in user secrets.toml")?,
    )?;
    let path = config
        .bundles
        .get(name)
        .context("unknown application bundle")?;
    let path = if path.is_absolute() {
        path.clone()
    } else {
        file.parent().context("config directory")?.join(path)
    };
    ensure!(
        std::fs::metadata(&path)?.permissions().mode() & 0o077 == 0,
        "application bundle must be private (chmod 600)"
    );
    let bytes = std::fs::read(&path)?;
    ensure!(
        bytes.len() <= 1024 * 1024,
        "application bundle exceeds 1 MiB"
    );
    Ok((hash(&bytes), parse(std::str::from_utf8(&bytes)?)?))
}
pub fn select(db: &Store, oid: &str, names: &[String]) -> Result<()> {
    for name in names {
        let (version, _) = load_bundle(name)?;
        db.conn.execute(
            "INSERT INTO task_bundles VALUES(?,?,?)",
            rusqlite::params![oid, name, version],
        )?;
    }
    Ok(())
}
pub fn inherit(db: &Store, parent: &str, child: &str, narrow: Option<&Value>) -> Result<()> {
    let rows = db.rows(
        "SELECT name,version FROM task_bundles WHERE task=?",
        &[&parent],
    )?;
    let selected: Vec<String> = if let Some(n) = narrow {
        serde_json::from_value(n.clone())?
    } else {
        rows.iter()
            .filter_map(|r| r["name"].as_str().map(str::to_owned))
            .collect()
    };
    ensure!(
        selected
            .iter()
            .all(|n| rows.iter().any(|r| r["name"] == *n)),
        "child cannot expand inherited secret access"
    );
    db.conn
        .execute("DELETE FROM task_bundles WHERE task=?", [child])?;
    for row in rows {
        if selected.iter().any(|n| row["name"] == *n) {
            db.conn.execute(
                "INSERT INTO task_bundles VALUES(?,?,?)",
                rusqlite::params![child, row["name"].as_str(), row["version"].as_str()],
            )?;
        }
    }
    Ok(())
}
pub fn values(db: &Store, oid: &str) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for row in db.rows(
        "SELECT name,version FROM task_bundles WHERE task=? ORDER BY name",
        &[&oid],
    )? {
        let name = row["name"].as_str().context("bundle")?;
        let remote = db
            .root
            .join("remote-secrets")
            .join(oid)
            .join(hash(name.as_bytes()));
        let (version, values) = if remote.exists() {
            let packet: Value = serde_json::from_slice(&std::fs::read(remote)?)?;
            (
                packet["version"]
                    .as_str()
                    .context("bundle version")?
                    .to_owned(),
                serde_json::from_value::<BTreeMap<String, String>>(packet["values"].clone())?,
            )
        } else {
            ensure!(
                db.rows("SELECT task FROM remote_origins WHERE task=?", &[&oid])?
                    .is_empty(),
                "remote application bundle must be reacquired from its caller"
            );
            load_bundle(name)?
        };
        ensure!(
            row["version"] == version,
            "application bundle changed; explicitly refresh bundles before resuming"
        );
        for (k, v) in values {
            if let Some(old) = result.insert(k, v.clone()) {
                ensure!(old == v, "selected bundles define conflicting values");
            }
        }
    }
    Ok(result)
}
pub fn redact_values(text: &str, values: &BTreeMap<String, String>) -> String {
    let mut result = text.to_owned();
    let mut values: Vec<_> = values.values().filter(|s| !s.is_empty()).collect();
    values.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for secret in values {
        result = result.replace(secret, "[REDACTED]");
    }
    result
}
pub fn redact_json(value: &Value, values: &BTreeMap<String, String>) -> Value {
    fn walk(v: &Value, secrets: &BTreeMap<String, String>) -> Value {
        match v {
            Value::String(s) => Value::String(redact_values(s, secrets)),
            Value::Array(v) => Value::Array(v.iter().map(|v| walk(v, secrets)).collect()),
            Value::Object(v) => Value::Object(
                v.iter()
                    .map(|(k, v)| (k.clone(), walk(v, secrets)))
                    .collect(),
            ),
            v => v.clone(),
        }
    }
    walk(value, values)
}
pub fn redact(db: &Store, oid: &str, value: &Value) -> Value {
    let values = match values(db, oid) {
        Ok(v) => v,
        Err(_) => {
            return serde_json::json!({"output_withheld":"application bundle unavailable or changed"});
        }
    };
    fn walk(v: &Value, secrets: &BTreeMap<String, String>) -> Value {
        match v {
            Value::String(s) => Value::String(redact_values(s, secrets)),
            Value::Array(v) => Value::Array(v.iter().map(|v| walk(v, secrets)).collect()),
            Value::Object(v) => Value::Object(
                v.iter()
                    .map(|(k, v)| (k.clone(), walk(v, secrets)))
                    .collect(),
            ),
            v => v.clone(),
        }
    }
    walk(value, &values)
}
pub struct PrivateEnv {
    pub path: PathBuf,
}
impl PrivateEnv {
    pub fn create(root: &Path, values: &BTreeMap<String, String>) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
        let path = root.join(format!("{}.env", crate::store::id()));
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        for (k, v) in values {
            ensure!(
                !v.contains(['\n', '\r']),
                "multiline environment file values unsupported; use process injection"
            );
            writeln!(f, "{k}={v}")?;
        }
        f.sync_all()?;
        Ok(Self { path })
    }
}
impl Drop for PrivateEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
