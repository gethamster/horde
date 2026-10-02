//! Read only committed Git objects selected by a server-recorded checkpoint.
use crate::{projects, store::Store};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{io::Read, path::Path, process::Stdio};

const MAX_FILES: usize = 128;
const MAX_LIST: usize = 1024 * 1024;
const MAX_PART: usize = 64 * 1024;
const MAX_TOTAL: usize = 2 * 1024 * 1024;

fn commit(value: &str) -> Result<()> {
    ensure!(
        [40, 64].contains(&value.len())
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "expected refs must be full lowercase commit IDs"
    );
    Ok(())
}

/// Captures bounded stdout without invoking a shell, textconv, external diff,
/// credential helper, or network. Git diagnostics are not returned to callers.
fn bounded(repo: &Path, args: &[&str], limit: usize) -> Result<(Vec<u8>, bool)> {
    let mut child = crate::executor::clean_command("git")
        .current_dir(repo)
        .args(["--literal-pathspecs", "-c", "core.fsmonitor=false"])
        .args(args)
        .env("GIT_NO_LAZY_FETCH", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("cannot inspect Run Git objects")?;
    let mut output = Vec::new();
    let read = child
        .stdout
        .take()
        .context("Git stdout")?
        .take((limit + 1) as u64)
        .read_to_end(&mut output);
    let truncated = output.len() > limit;
    if truncated || read.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    read?;
    ensure!(
        truncated || status.success(),
        "cannot inspect Run Git objects"
    );
    output.truncate(limit);
    Ok((output, truncated))
}

fn text_blob(repo: &Path, mode: &str, oid: &str, limit: usize) -> Result<(Option<String>, bool)> {
    // Do not follow symlinks or read submodules. The blob ID comes from Git,
    // never from a path in the request or the working tree.
    if matches!(mode, "000000" | "160000") {
        return Ok((None, false));
    }
    commit(oid)?;
    let (size, _) = bounded(repo, &["cat-file", "-s", oid], 32)?;
    let size: usize = std::str::from_utf8(&size)?.trim().parse()?;
    if size > limit {
        return Ok((None, true));
    }
    if !matches!(mode, "100644" | "100755") {
        return Ok((None, false));
    }
    let (bytes, truncated) = bounded(repo, &["cat-file", "blob", oid], limit)?;
    if truncated {
        return Ok((None, true));
    }
    if bytes.contains(&0) {
        return Ok((None, false));
    }
    Ok((String::from_utf8(bytes).ok(), false))
}

fn file_change(
    repo: &Path,
    base: &str,
    head: &str,
    path: &str,
    raw: &str,
    budget: usize,
) -> Result<(Value, usize)> {
    let fields: Vec<_> = raw
        .trim_start_matches(':')
        .split_ascii_whitespace()
        .collect();
    ensure!(fields.len() == 5, "invalid Git change record");
    let (previous, previous_truncated) =
        text_blob(repo, fields[0], fields[2], MAX_PART.min(budget))?;
    let mut used = previous.as_ref().map_or(0, String::len);
    let (content, content_truncated) = text_blob(
        repo,
        fields[1],
        fields[3],
        MAX_PART.min(budget.saturating_sub(used)),
    )?;
    used += content.as_ref().map_or(0, String::len);
    // Capturing bounded stdout alone would still let Git load a huge blob.
    // Omit oversized diffs before Git's diff algorithm reads those objects.
    let (diff, diff_truncated) = if previous_truncated || content_truncated {
        (String::new(), true)
    } else {
        let (bytes, truncated) = bounded(
            repo,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--submodule=short",
                "--unified=3",
                base,
                head,
                "--",
                path,
            ],
            MAX_PART.min(budget.saturating_sub(used)),
        )?;
        // Lossy replacement can expand bytes past the remaining preview budget.
        match String::from_utf8(bytes) {
            Ok(diff) => (diff, truncated),
            Err(_) => (String::new(), true),
        }
    };
    used += diff.len();
    Ok((
        json!({"path":path,"status":fields[4],"diff":diff,
        "previousText":previous,"content":content,
        "truncated":diff_truncated || previous_truncated || content_truncated}),
        used,
    ))
}

/// The transport has already authorized the task against its bound project.
/// Reject stale checkpoints before reading source. Neither a repo path nor an
/// arbitrary Git revision can select another repository or commit.
pub fn run_changes(db: &Store, task: &str, base: &str, head: &str) -> Result<Value> {
    commit(base)?;
    commit(head)?;
    let _lock = super::run_lock(db, task)?;
    let context = super::run_context(db, task)?;
    let checkpoint = db.rows(
        "SELECT data FROM events WHERE task=? AND kind='run.checkpoint_verified' ORDER BY seq DESC LIMIT 1",
        &[&task],
    )?;
    let checkpoint: Value = serde_json::from_str(
        checkpoint
            .first()
            .and_then(|r| r["data"].as_str())
            .context("Run has no verified checkpoint")?,
    )?;
    ensure!(
        checkpoint["commit_sha"] == head && checkpoint["expected_main_head"] == base,
        "requested changes do not match the latest verified checkpoint"
    );
    let row = db.task(task)?;
    let repo = Path::new(row["repo"].as_str().context("Run repository")?);
    let project = context["project"].as_str().context("Run project")?;
    ensure!(
        projects::infer(db, repo)?.as_deref() == Some(project),
        "Run repository is not registered to its project"
    );
    let branch = format!("refs/heads/horde/{task}^{{commit}}");
    let (current, _) = bounded(repo, &["rev-parse", "--verify", &branch], 128)?;
    ensure!(
        std::str::from_utf8(&current)?.trim() == head,
        "Run head changed; refresh its verified checkpoint"
    );
    let (records, list_truncated) = bounded(
        repo,
        &[
            "diff",
            "--raw",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--abbrev=64",
            "-z",
            base,
            head,
            "--",
        ],
        MAX_LIST,
    )?;
    let mut parts = records.split(|b| *b == 0);
    let mut files = Vec::new();
    let mut used = 0;
    let mut truncated = list_truncated;
    while let Some(raw) = parts.next().filter(|b| !b.is_empty()) {
        let Some(path) = parts.next() else {
            truncated = true;
            break;
        };
        if files.len() == MAX_FILES || used >= MAX_TOTAL {
            truncated = true;
            break;
        }
        let (Ok(raw), Ok(path)) = (std::str::from_utf8(raw), std::str::from_utf8(path)) else {
            truncated = true;
            continue;
        };
        // Keep path display bounded; Git's literal pathspec prevents syntax
        // interpretation for filenames containing brackets, colons, or dashes.
        if path.len() > 4096 {
            truncated = true;
            continue;
        }
        let (file, bytes) = file_change(repo, base, head, path, raw, MAX_TOTAL - used)?;
        used += bytes;
        truncated |= file["truncated"] == true;
        files.push(file);
    }
    Ok(
        json!({"run_id":task,"project_id":project,"tenant_id":context["tenant_id"],
        "base_sha":base,"head_sha":head,"files":files,"truncated":truncated}),
    )
}
