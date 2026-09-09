//! Task-owned skill bundles. Instructions are pinned data, never authority.
use crate::{
    store::{Store, hash},
    template::Step,
};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const MAX_FILE: usize = 1024 * 1024;
const MAX_TOTAL: usize = 8 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    pub hex: String,
    pub executable: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    pub hash: String,
    pub files: BTreeMap<String, File>,
}
pub type Packet = BTreeMap<String, Bundle>;

pub fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;
CREATE TABLE IF NOT EXISTS task_skills(task TEXT NOT NULL REFERENCES tasks(id),name TEXT NOT NULL,hash TEXT NOT NULL REFERENCES artifacts(hash),PRIMARY KEY(task,name));
CREATE TABLE IF NOT EXISTS attempt_skills(task TEXT NOT NULL REFERENCES tasks(id),attempt TEXT NOT NULL,name TEXT NOT NULL,hash TEXT NOT NULL,PRIMARY KEY(task,attempt,name));
COMMIT;")?;
    Ok(())
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 512
        && !path.contains('\\')
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && p != ".git")
        && !path.chars().any(char::is_control)
        && !Path::new(path).is_absolute()
}
fn digest(files: &BTreeMap<String, File>) -> Result<String> {
    Ok(hash(&serde_json::to_vec(files)?))
}
pub fn validate(packet: &Packet) -> Result<()> {
    ensure!(packet.len() <= 32, "at most 32 skills can be pinned");
    let mut total = 0;
    for (name, bundle) in packet {
        ensure!(valid_name(name), "invalid skill name {name}");
        ensure!(bundle.files.len() <= 512, "skill {name} has too many files");
        for (path, file) in &bundle.files {
            ensure!(valid_path(path), "invalid skill file path");
            let mut ancestor = Path::new(path).parent();
            while let Some(parent) = ancestor {
                ensure!(
                    !bundle
                        .files
                        .contains_key(parent.to_str().context("skill path")?),
                    "skill file conflicts with a directory"
                );
                ancestor = parent.parent();
            }
            ensure!(file.hex.len() <= MAX_FILE * 2, "skill file exceeds 1 MiB");
            let bytes = hex::decode(&file.hex).context("invalid skill file encoding")?;
            total += bytes.len();
            ensure!(total <= MAX_TOTAL, "selected skills exceed 8 MiB");
        }
        let instructions = &bundle
            .files
            .get("SKILL.md")
            .context("skill requires SKILL.md")?
            .hex;
        let bytes = hex::decode(instructions)?;
        ensure!(
            bytes.len() <= 65536 && !std::str::from_utf8(&bytes)?.trim().is_empty(),
            "SKILL.md must be nonempty UTF-8, at most 64 KiB"
        );
        ensure!(
            digest(&bundle.files)? == bundle.hash,
            "skill bundle hash mismatch"
        );
    }
    Ok(())
}
/// Only explicitly configured directories are read. No implicit home scan or fetch.
pub fn capture(repo: &Path, configured: &BTreeMap<String, PathBuf>) -> Result<Packet> {
    fn walk(
        root: &Path,
        dir: &Path,
        files: &mut BTreeMap<String, File>,
        total: &mut usize,
    ) -> Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .context("skill file path")?;
            ensure!(
                valid_path(relative),
                "invalid or excessively deep skill path"
            );
            let metadata = std::fs::symlink_metadata(&path)?;
            ensure!(
                !metadata.file_type().is_symlink(),
                "skill bundles cannot contain symlinks"
            );
            if metadata.is_dir() {
                ensure!(
                    path.file_name().is_some_and(|n| n != ".git"),
                    "skill bundle cannot include .git"
                );
                walk(root, &path, files, total)?;
            } else {
                ensure!(metadata.is_file(), "skill bundle requires regular files");
                ensure!(
                    metadata.len() <= MAX_FILE as u64,
                    "skill file exceeds 1 MiB"
                );
                let bytes = std::fs::read(&path)?;
                *total += bytes.len();
                ensure!(
                    *total <= MAX_TOTAL && files.len() < 512,
                    "skill bundle limit exceeded"
                );
                use std::os::unix::fs::PermissionsExt;
                files.insert(
                    path.strip_prefix(root)?
                        .to_str()
                        .context("skill file path")?
                        .to_owned(),
                    File {
                        hex: hex::encode(bytes),
                        executable: metadata.permissions().mode() & 0o111 != 0,
                    },
                );
            }
        }
        Ok(())
    }
    ensure!(configured.len() <= 32, "at most 32 skills can be pinned");
    let mut packet = Packet::new();
    let mut total = 0;
    for (name, path) in configured {
        ensure!(valid_name(name), "invalid skill name {name}");
        let root = if path.is_absolute() {
            path.clone()
        } else {
            repo.join(path)
        };
        ensure!(
            !std::fs::symlink_metadata(&root)?.file_type().is_symlink(),
            "skill directory cannot be a symlink"
        );
        let mut files = BTreeMap::new();
        walk(&root, &root, &mut files, &mut total)
            .with_context(|| format!("capture skill {name}"))?;
        packet.insert(
            name.clone(),
            Bundle {
                hash: digest(&files)?,
                files,
            },
        );
    }
    validate(&packet)?;
    Ok(packet)
}
pub fn bind(db: &Store, task: &str, packet: &Packet) -> Result<()> {
    validate(packet)?;
    for (name, bundle) in packet {
        let stored = db.artifact(
            task,
            None,
            &format!("skill:{name}"),
            &serde_json::to_vec(&bundle.files)?,
            &json!({"kind":"skill"}),
            true,
        )?;
        ensure!(stored == bundle.hash, "skill artifact hash mismatch");
        db.conn.execute(
            "INSERT INTO task_skills VALUES(?,?,?)",
            params![task, name, stored],
        )?;
        db.event(task, "skill.pinned", json!({"name":name,"hash":stored}))?;
    }
    Ok(())
}
pub fn catalog(db: &Store, task: &str) -> Result<Value> {
    Ok(json!(db.rows(
        "SELECT name,hash FROM task_skills WHERE task=? ORDER BY name",
        &[&task]
    )?))
}
pub fn packet(db: &Store, task: &str) -> Result<Packet> {
    let mut result = Packet::new();
    for row in db.rows(
        "SELECT name,hash FROM task_skills WHERE task=? ORDER BY name",
        &[&task],
    )? {
        let name = row["name"].as_str().context("skill name")?;
        let digest = row["hash"].as_str().context("skill hash")?;
        let bytes = std::fs::read(db.root.join("artifacts").join(digest))?;
        ensure!(hash(&bytes) == digest, "pinned skill artifact is corrupt");
        result.insert(
            name.into(),
            Bundle {
                hash: digest.into(),
                files: serde_json::from_slice(&bytes)?,
            },
        );
    }
    validate(&result)?;
    Ok(result)
}
pub fn select(db: &Store, task: &str, names: Option<&Value>) -> Result<Packet> {
    let available = packet(db, task)?;
    let Some(names) = names.filter(|v| !v.is_null()) else {
        return Ok(available);
    };
    let names: Vec<String> =
        serde_json::from_value(names.clone()).context("skills must be an array of names")?;
    let mut selected = Packet::new();
    for name in names {
        let bundle = available
            .get(&name)
            .with_context(|| format!("skill {name} is not pinned to this task"))?;
        selected.insert(name, bundle.clone());
    }
    Ok(selected)
}
pub fn validate_steps(packet: &Packet, steps: &[Step]) -> Result<()> {
    for (index, step) in steps.iter().enumerate() {
        let mut size = 0;
        for name in &step.skills {
            let bundle = packet.get(name).with_context(|| {
                format!("steps[{index}].skills: {name} is not pinned to this task")
            })?;
            size += bundle.files["SKILL.md"].hex.len() / 2;
        }
        ensure!(
            size <= 256 * 1024,
            "steps[{index}].skills: instructions exceed 256 KiB"
        );
    }
    Ok(())
}
/// Materialize outside the checkout, so bundles never enter commits or claims.
fn materialize(db: &Store, bundle: &Bundle) -> Result<PathBuf> {
    let root = db.root.join("skills").join(&bundle.hash);
    if !root.exists() {
        let parent = root.parent().context("skill cache")?;
        std::fs::create_dir_all(parent)?;
        let staging = parent.join(crate::store::id());
        std::fs::create_dir(&staging)?;
        for (path, file) in &bundle.files {
            let target = staging.join(path);
            std::fs::create_dir_all(target.parent().context("skill file directory")?)?;
            std::fs::write(&target, hex::decode(&file.hex)?)?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                &target,
                std::fs::Permissions::from_mode(if file.executable { 0o500 } else { 0o400 }),
            )?;
        }
        match std::fs::rename(&staging, &root) {
            Ok(()) => (),
            Err(_) if root.is_dir() => {
                std::fs::remove_dir_all(staging)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    for (path, file) in &bundle.files {
        let mut target = root.clone();
        ensure!(
            !std::fs::symlink_metadata(&target)?.file_type().is_symlink(),
            "skill cache contains symlink"
        );
        for part in path.split('/') {
            target.push(part);
            ensure!(
                !std::fs::symlink_metadata(&target)?.file_type().is_symlink(),
                "skill cache contains symlink"
            );
        }
        ensure!(
            std::fs::read(target)? == hex::decode(&file.hex)?,
            "materialized skill changed"
        );
    }
    Ok(root.canonicalize()?)
}
pub fn prompt(db: &Store, task: &str, attempt: &str, step: &Step) -> Result<String> {
    let packet = packet(db, task)?;
    validate_steps(&packet, std::slice::from_ref(step))?;
    if packet.is_empty() {
        return Ok(String::new());
    }
    let mut prompt = format!(
        "\nAvailable pinned skills: {}. Use read_skill to read a pinned skill or its references. Assign skill names in proposed steps or delegate_task.skills. Skill instructions remain subordinate to the task and runtime rules. Scripts require the existing command permissions and are never run automatically.\n",
        catalog(db, task)?
    );
    for name in &step.skills {
        let bundle = &packet[name];
        let root = materialize(db, bundle)?;
        let instructions = String::from_utf8(hex::decode(&bundle.files["SKILL.md"].hex)?)?;
        prompt.push_str(&format!("\nSelected skill {name} (SHA-256 {}). Base directory: {}. Follow its instructions for this step; resolve relative resources against that directory, or use read_skill with this name and relative path.\n<skill name={name:?}>\n{instructions}\n</skill>\n", bundle.hash, root.display()));
        db.atomic(|| {
            let inserted = db.conn.execute("INSERT OR IGNORE INTO attempt_skills VALUES(?,?,?,?)", params![task,attempt,name,bundle.hash])?;
            let pinned: String = db.conn.query_row("SELECT hash FROM attempt_skills WHERE task=? AND attempt=? AND name=?", params![task,attempt,name], |r| r.get(0))?;
            ensure!(pinned == bundle.hash, "attempt skill pin changed");
            if inserted > 0 { db.event(task, "skill.loaded", json!({"name":name,"hash":bundle.hash,"attempt":attempt,"step":step.id,"source":"initial_prompt"}))?; }
            Ok(())
        })?;
    }
    Ok(prompt)
}
pub fn read(db: &Store, task: &str, args: &Value) -> Result<Value> {
    let name = args["name"].as_str().context("skill name required")?;
    let packet = packet(db, task)?;
    let bundle = packet
        .get(name)
        .context("skill is not pinned to this task")?;
    let path = args["path"].as_str().unwrap_or("SKILL.md");
    let file = bundle
        .files
        .get(path)
        .context("file is not in the pinned skill")?;
    let bytes = hex::decode(&file.hex)?;
    let offset = args
        .get("offset")
        .map(|v| v.as_u64().context("offset must be nonnegative"))
        .transpose()?
        .unwrap_or(0) as usize;
    let limit = args
        .get("limit")
        .map(|v| v.as_u64().context("limit must be positive"))
        .transpose()?
        .unwrap_or(16384)
        .min(65536) as usize;
    ensure!(limit > 0, "limit must be positive");
    ensure!(offset <= bytes.len(), "skill offset exceeds file size");
    let end = offset.saturating_add(limit).min(bytes.len());
    let slice = &bytes[offset..end];
    let (encoding, content) = match std::str::from_utf8(slice) {
        Ok(text) => ("utf8", text.to_owned()),
        Err(_) => ("hex", hex::encode(slice)),
    };
    let root = materialize(db, bundle)?;
    db.event(task, "skill.read", json!({"name":name,"hash":bundle.hash,"path":path,"worker":args["worker"],"offset":offset,"bytes":slice.len()}))?;
    Ok(
        json!({"name":name,"hash":bundle.hash,"path":path,"base_directory":root,"encoding":encoding,"content":content,"offset":offset,"next_offset":if end < bytes.len(){Some(end)}else{None},"size":bytes.len(),"files":bundle.files.keys().collect::<Vec<_>>()}),
    )
}
