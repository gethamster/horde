use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_EXCERPT_BYTES: usize = 1200;

fn git(repo: &Path, args: &[&str], maximum: usize) -> Result<Vec<u8>> {
    let mut child = Command::new("git")
        .current_dir(repo)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .context("git output")?
        .take((maximum + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        let _ = child.kill();
        let _ = child.wait();
        anyhow::bail!("git evidence exceeds limit")
    }
    ensure!(child.wait()?.success(), "git evidence unavailable");
    Ok(bytes)
}

pub(super) fn git_text(repo: &Path, args: &[&str], maximum: usize) -> Result<String> {
    Ok(String::from_utf8(git(repo, args, maximum)?)?
        .trim()
        .to_owned())
}

fn git_excerpt(repo: &Path, args: &[&str]) -> Result<(String, bool)> {
    let mut child = Command::new("git")
        .current_dir(repo)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut bytes = Vec::new();
    child
        .stdout
        .take()
        .context("git output")?
        .take((MAX_EXCERPT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > MAX_EXCERPT_BYTES;
    if truncated {
        let _ = child.kill();
        let _ = child.wait();
        bytes.truncate(MAX_EXCERPT_BYTES);
    } else {
        ensure!(child.wait()?.success(), "git diff excerpt unavailable");
    }
    Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}

pub(super) fn workspace(db: &crate::store::Store, task: &str) -> Result<PathBuf> {
    Ok(crate::project_runtime::task_root(db, task)?
        .join("workspaces")
        .join(task)
        .join("integrated"))
}

pub(crate) fn observed_head(db: &crate::store::Store, task: &str) -> Option<String> {
    let path = workspace(db, task).ok()?;
    path.exists()
        .then(|| git_text(&path, &["rev-parse", "HEAD"], 128).ok())
        .flatten()
}

pub(super) fn manifest(repo: &Path, base: &str, head: &str) -> Result<Value> {
    let names = git(
        repo,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--find-renames",
            "--name-status",
            "-z",
            base,
            head,
            "--",
        ],
        MAX_MANIFEST_BYTES,
    )?;
    let mut parts = names
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty());
    let mut paths = Vec::new();
    while let Some(status) = parts.next() {
        let status = std::str::from_utf8(status)?;
        let old = std::str::from_utf8(parts.next().context("missing changed path")?)?;
        let (path, previous) = if status.starts_with('R') || status.starts_with('C') {
            (
                std::str::from_utf8(parts.next().context("missing rename destination")?)?,
                Some(old),
            )
        } else {
            (old, None)
        };
        paths.push(json!({"status":status,"path":path,"previous_path":previous,"binary":false}));
    }
    // Numstat marks binary entries with '-' in both line-count columns.
    let stats = git(
        repo,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--find-renames",
            "--numstat",
            "-z",
            base,
            head,
            "--",
        ],
        MAX_MANIFEST_BYTES,
    )?;
    let mut binary = std::collections::BTreeSet::new();
    let mut stat = stats.split(|byte| *byte == 0);
    while let Some(item) = stat.next() {
        if item.is_empty() {
            break;
        }
        let fields = item.splitn(3, |byte| *byte == b'\t').collect::<Vec<_>>();
        ensure!(fields.len() == 3, "invalid numstat record");
        let renamed = fields[2].is_empty();
        let path = if renamed {
            let _ = stat.next().context("rename source")?;
            stat.next().context("rename destination")?
        } else {
            fields[2]
        };
        if fields[0] == b"-" && fields[1] == b"-" {
            binary.insert(std::str::from_utf8(path)?.to_owned());
        }
    }
    for row in &mut paths {
        row["binary"] = json!(binary.contains(row["path"].as_str().unwrap_or("")));
    }
    let mut excerpts = Vec::new();
    for (index, row) in paths.iter_mut().enumerate() {
        if row["binary"] == true {
            row["covered"] = json!(false);
            row["uncovered_reason"] = json!("binary_path");
            continue;
        }
        if index >= 8 {
            row["covered"] = json!(false);
            row["uncovered_reason"] = json!("diff_excerpt_omitted");
            continue;
        }
        let path = row["path"].as_str().context("manifest path")?.to_owned();
        let output = git_excerpt(
            repo,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--unified=1",
                base,
                head,
                "--",
                &path,
            ],
        )
        .ok();
        let reason = match &output {
            Some((_, false)) => None,
            Some((_, true)) => Some("diff_excerpt_truncated"),
            None => Some("diff_excerpt_unavailable"),
        };
        row["covered"] = json!(reason.is_none());
        row["uncovered_reason"] = json!(reason);
        excerpts.push(json!({"path":path,"excerpt":output.as_ref().map(|v| &v.0),"truncated":output.as_ref().is_none_or(|v| v.1)}));
    }
    let complete = paths.iter().all(|row| row["covered"] == true);
    Ok(
        json!({"base":base,"head":head,"paths":paths,"excerpts":excerpts,"path_count":paths.len(),"excerpted_paths":excerpts.len(),"complete":complete}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observed_head_uses_the_tasks_project_workspace() {
        let data = tempfile::tempdir().unwrap();
        let db = crate::store::Store::open(data.path()).unwrap();
        db.conn
            .execute(
                "INSERT INTO projects VALUES('other','other','Other',4,'native',0)",
                [],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO tasks VALUES('task','objective','.','running','{}','{}',0)",
                [],
            )
            .unwrap();
        db.conn
            .execute("INSERT INTO task_projects VALUES('task','other',NULL)", [])
            .unwrap();
        let path = workspace(&db, "task").unwrap();
        std::fs::create_dir_all(&path).unwrap();
        run(&path, &["init", "-q"]);
        run(&path, &["config", "user.name", "Reviewer"]);
        run(&path, &["config", "user.email", "reviewer@example.test"]);
        std::fs::write(path.join("project.txt"), "review me\n").unwrap();
        run(&path, &["add", "."]);
        run(&path, &["commit", "-qm", "project head"]);
        assert_eq!(
            observed_head(&db, "task"),
            Some(git_text(&path, &["rev-parse", "HEAD"], 128).unwrap())
        );
        assert!(!data.path().join("workspaces/task/integrated").exists());
    }

    fn run(repo: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .current_dir(repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    #[test]
    fn manifest_covers_renames_binary_and_bounded_excerpts() {
        let repo = tempfile::tempdir().unwrap();
        run(repo.path(), &["init", "-q"]);
        run(repo.path(), &["config", "user.name", "Reviewer"]);
        run(
            repo.path(),
            &["config", "user.email", "reviewer@example.test"],
        );
        std::fs::write(repo.path().join("before name.txt"), "unchanged\n").unwrap();
        std::fs::write(repo.path().join("image.bin"), b"a\0b").unwrap();
        std::fs::write(repo.path().join("large.txt"), "old\n").unwrap();
        run(repo.path(), &["add", "."]);
        run(repo.path(), &["commit", "-qm", "base"]);
        let base = git_text(repo.path(), &["rev-parse", "HEAD"], 128).unwrap();
        std::fs::rename(
            repo.path().join("before name.txt"),
            repo.path().join("after name.txt"),
        )
        .unwrap();
        std::fs::write(repo.path().join("image.bin"), b"c\0d").unwrap();
        std::fs::write(repo.path().join("large.txt"), "line\n".repeat(500)).unwrap();
        run(repo.path(), &["add", "-A"]);
        run(repo.path(), &["commit", "-qm", "change"]);
        let head = git_text(repo.path(), &["rev-parse", "HEAD"], 128).unwrap();
        let result = manifest(repo.path(), &base, &head).unwrap();
        assert_eq!(result["path_count"], 3);
        assert_eq!(result["complete"], false);
        let paths = result["paths"].as_array().unwrap();
        assert!(paths.iter().any(|row| row["path"] == "after name.txt"
            && row["previous_path"] == "before name.txt"
            && row["status"].as_str().unwrap().starts_with('R')));
        assert!(paths.iter().any(|row| row["path"] == "image.bin"
            && row["binary"] == true
            && row["uncovered_reason"] == "binary_path"));
        assert!(
            result["excerpts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["path"] == "large.txt"
                    && row["truncated"] == true
                    && row["excerpt"].as_str().unwrap().len() <= MAX_EXCERPT_BYTES)
        );
        assert!(
            paths.iter().any(|row| row["path"] == "large.txt"
                && row["uncovered_reason"] == "diff_excerpt_truncated")
        );
    }

    #[test]
    fn more_than_eight_changed_paths_record_omitted_excerpts() {
        let repo = tempfile::tempdir().unwrap();
        run(repo.path(), &["init", "-q"]);
        run(repo.path(), &["config", "user.name", "Reviewer"]);
        run(
            repo.path(),
            &["config", "user.email", "reviewer@example.test"],
        );
        std::fs::write(repo.path().join("base.txt"), "base\n").unwrap();
        run(repo.path(), &["add", "."]);
        run(repo.path(), &["commit", "-qm", "base"]);
        let base = git_text(repo.path(), &["rev-parse", "HEAD"], 128).unwrap();
        for index in 0..9 {
            std::fs::write(repo.path().join(format!("file-{index}.txt")), "small\n").unwrap();
        }
        run(repo.path(), &["add", "."]);
        run(repo.path(), &["commit", "-qm", "nine"]);
        let head = git_text(repo.path(), &["rev-parse", "HEAD"], 128).unwrap();
        let result = manifest(repo.path(), &base, &head).unwrap();
        assert_eq!(result["path_count"], 9);
        assert_eq!(result["complete"], false);
        assert_eq!(
            result["paths"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["uncovered_reason"] == "diff_excerpt_omitted")
                .count(),
            1
        );
    }
}
