use crate::{config::Settings, store::Store};
use anyhow::{Context, Result, bail};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
pub fn tools() -> Vec<Value> {
    vec![
        json!({"type":"function","function":{"name":"read_file","description":"Read a UTF-8 file in this worktree","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}}),
        json!({"type":"function","function":{"name":"search","description":"Search repository files using ripgrep","parameters":{"type":"object","properties":{"pattern":{"type":"string"}},"required":["pattern"]}}}),
        json!({"type":"function","function":{"name":"write_file","description":"Write a complete UTF-8 file; requires exclusive write claim","parameters":{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}}}),
        json!({"type":"function","function":{"name":"apply_patch","description":"Apply a unified Git diff after validating every changed path against exclusive claims","parameters":{"type":"object","properties":{"patch":{"type":"string"}},"required":["patch"]}}}),
        json!({"type":"function","function":{"name":"command","description":"Execute argv in the workspace. Environment excludes provider credentials. Changes are checked against claims.","parameters":{"type":"object","properties":{"argv":{"type":"array","items":{"type":"string"}}},"required":["argv"]}}}),
    ]
}
fn safe_path(root: &Path, p: &str) -> Result<PathBuf> {
    let p = crate::store::scope(p)?;
    let root = root.canonicalize()?;
    let target = root.join(p);
    let mut ancestor = Some(target.as_path());
    while let Some(path) = ancestor {
        if path == root {
            break;
        }
        if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
            bail!("native file paths cannot traverse symlinks");
        }
        ancestor = path.parent();
    }
    let mut existing = target.as_path();
    while !existing.exists() {
        existing = existing.parent().context("invalid parent")?;
    }
    if !existing.canonicalize()?.starts_with(&root) {
        bail!("symlink escapes workspace");
    }
    // Git metadata and symlinks to it are never exposed through native file tools.
    if existing
        .canonicalize()?
        .components()
        .any(|c| c.as_os_str() == ".git")
    {
        bail!("Git metadata is reserved");
    }
    Ok(target)
}
pub async fn call(
    db: &Store,
    wid: &str,
    name: &str,
    args: &Value,
    settings: &Settings,
    allowed: &[String],
) -> Result<Value> {
    if !allowed.iter().any(|x| x == name) {
        bail!("tool {name} not allowed for step");
    }
    let w = db.worker(wid)?;
    let root = Path::new(w["workspace"].as_str().context("workspace")?);
    let path = || -> Result<PathBuf> { safe_path(root, args["path"].as_str().context("path")?) };
    match name {
        "read_file" => {
            let p = path()?;
            if std::fs::metadata(&p)?.len() > 1024 * 1024 {
                bail!("file exceeds 1 MiB");
            }
            Ok(json!({"content":std::fs::read_to_string(p)?}))
        }
        "write_file" => {
            let p = path()?;
            db.check_write(wid, args["path"].as_str().context("path")?)?;
            std::fs::create_dir_all(p.parent().context("parent")?)?;
            let content = args["content"].as_str().context("content")?;
            let changed = std::fs::read(&p).ok().as_deref() != Some(content.as_bytes());
            std::fs::write(&p, content)?;
            if changed {
                crate::budget::progress(
                    db,
                    wid,
                    "file_write",
                    &crate::store::hash(json!([args["path"], content]).to_string().as_bytes()),
                )?;
            }
            Ok(json!({"written":true}))
        }
        "apply_patch" => {
            let patch = args["patch"].as_str().context("patch")?;
            if patch.len() > 1024 * 1024 {
                bail!("patch exceeds 1 MiB");
            }
            let patch = patch.to_owned();
            let data_root = db.root.clone();
            let root = root.to_owned();
            let wid = wid.to_owned();
            crate::budget::blocking(move || {
                let db = Store::open(&data_root)?;
                let root = root.as_path();
                let wid = wid.as_str();
                let scratch = db.root.join(format!("patch-{}", crate::store::id()));
                std::fs::write(&scratch, &patch)?;
                let result = (|| -> Result<Value> {
                    let stats = crate::budget::command_output(
                        crate::executor::clean_command("git")
                            .args(["apply", "--numstat", "-z"])
                            .arg(&scratch)
                            .current_dir(root),
                    )?;
                    if !stats.status.success() {
                        bail!(
                            "invalid unified diff: {}",
                            String::from_utf8_lossy(&stats.stderr)
                        );
                    }
                    let mut before = Vec::new();
                    for record in stats.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
                        let record = std::str::from_utf8(record)?;
                        let file = record
                            .splitn(3, '\t')
                            .nth(2)
                            .context("patch contains unsupported rename metadata")?;
                        db.check_write(wid, file)?;
                        let path = safe_path(root, file)?;
                        before.push((file.to_owned(), std::fs::read(path).ok()));
                    }
                    let check = crate::budget::command_output(
                        crate::executor::clean_command("git")
                            .args(["apply", "--check"])
                            .arg(&scratch)
                            .current_dir(root),
                    )?;
                    if !check.status.success() {
                        bail!(
                            "patch does not apply: {}",
                            String::from_utf8_lossy(&check.stderr)
                        );
                    }
                    let applied = crate::budget::command_output(
                        crate::executor::clean_command("git")
                            .arg("apply")
                            .arg(&scratch)
                            .current_dir(root),
                    )?;
                    if !applied.status.success() {
                        bail!("patch failed: {}", String::from_utf8_lossy(&applied.stderr));
                    }
                    for (file, old) in before {
                        let current = std::fs::read(safe_path(root, &file)?).ok();
                        if current != old {
                            let fingerprint = current
                                .as_deref()
                                .map(crate::store::hash)
                                .unwrap_or_else(|| "deleted".into());
                            crate::budget::progress(
                                &db,
                                wid,
                                "file_write",
                                &crate::store::hash(
                                    json!([file, fingerprint]).to_string().as_bytes(),
                                ),
                            )?;
                        }
                    }
                    Ok(json!({"applied":true}))
                })();
                let _ = std::fs::remove_file(scratch);
                result
            })
            .await
        }
        "search" => {
            let mut command = crate::executor::clean_command("rg");
            command
                .args([
                    "--line-number",
                    "--max-count",
                    "50",
                    "--",
                    args["pattern"].as_str().context("pattern")?,
                    ".",
                ])
                .current_dir(root);
            let out =
                crate::executor::run_process(command, None, settings.timeout_seconds, None).await?;
            if out["exit_code"].as_i64().is_none_or(|c| c > 1) {
                bail!("search failed: {}", out["stderr"]);
            }
            Ok(
                json!({"matches":out["stdout"].as_str().unwrap_or("").chars().take(32000).collect::<String>()}),
            )
        }
        "command" => {
            if !settings.allow_commands {
                bail!("command execution disabled in settings");
            }
            let argv: Vec<String> = serde_json::from_value(args["argv"].clone())?;
            if argv.is_empty() {
                bail!("empty argv");
            }
            let attempt:Option<String>=db.conn.query_row("SELECT id FROM attempts WHERE worker=? AND state='running' ORDER BY started DESC LIMIT 1",[wid],|r|r.get(0)).optional()?;
            let out = crate::executor::run_command(
                &argv,
                root,
                settings.timeout_seconds,
                attempt.as_deref().map(|a| (db, a)),
            )
            .await?;
            let data_root = db.root.clone();
            let worker = wid.to_owned();
            crate::budget::blocking(move || {
                crate::git::validate_scope(&Store::open(&data_root)?, &worker)
            })
            .await?;
            Ok(out)
        }
        _ => bail!("unknown native tool {name}"),
    }
}
