use horde::{config::Settings, git, projects, protocol, run, store::Store};
use rusqlite::params;
use serde_json::{Value, json};
use std::path::PathBuf;

struct Fixture {
    _dir: tempfile::TempDir,
    db: Store,
    repo: PathBuf,
    task: String,
    base: String,
    head: String,
}
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@localhost"],
    ] {
        git::run(&repo, &args).unwrap();
    }
    std::fs::write(repo.join("hello.txt"), "before\n").unwrap();
    git::run(&repo, &["add", "."]).unwrap();
    git::run(&repo, &["commit", "-m", "base"]).unwrap();
    let base = git::run(&repo, &["rev-parse", "HEAD"]).unwrap();
    let task = "changes-smoke".to_owned();
    git::run(&repo, &["checkout", "-b", &format!("horde/{task}")]).unwrap();
    std::fs::write(repo.join("hello.txt"), "after\n").unwrap();
    std::fs::write(repo.join("[literal].txt"), "literal file\n").unwrap();
    std::fs::write(repo.join("binary.dat"), [0, 255, 1]).unwrap();
    std::fs::write(repo.join("large.txt"), vec![b'x'; 70_000]).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("/etc/passwd", repo.join("outside-link")).unwrap();
    git::run(&repo, &["add", "."]).unwrap();
    git::run(&repo, &["commit", "-m", "changes"]).unwrap();
    let head = git::run(&repo, &["rev-parse", "HEAD"]).unwrap();
    let db = Store::open(&dir.path().join("data")).unwrap();
    let repository = projects::register_repository(&db, "default", &repo).unwrap();
    db.conn.execute("INSERT INTO tasks(id,objective,repo,status,settings,plan,created) VALUES(?,?,?,'succeeded',?,'{}',1)",
        params![task,"changes smoke",repo.to_str(),serde_json::to_string(&Settings::default()).unwrap()]).unwrap();
    db.conn
        .execute(
            "INSERT INTO task_projects(task,project,repository) VALUES(?,'default',?)",
            params![task, repository],
        )
        .unwrap();
    db.event(
        &task,
        "run.checkpoint_verified",
        json!({"commit_sha":head,"head_sha":head,"expected_main_head":base,"verified":true}),
    )
    .unwrap();
    Fixture {
        _dir: dir,
        db,
        repo,
        task,
        base,
        head,
    }
}
fn request(f: &Fixture) -> Value {
    json!({"task":f.task,"expected_base":f.base,"expected_head":f.head})
}

#[test]
fn changes_reads_exact_committed_blobs_and_bounds_large_or_nontext_sources() {
    let f = fixture();
    // Working tree text is deliberately different; responses must use blobs.
    std::fs::write(f.repo.join("hello.txt"), "uncommitted secret\n").unwrap();
    let result =
        protocol::dispatch_scoped(&f.db, "run_changes", request(&f), None, Some("default"))
            .unwrap();
    assert_eq!(result["base_sha"], f.base);
    assert_eq!(result["head_sha"], f.head);
    let files = result["files"].as_array().unwrap();
    let hello = files.iter().find(|v| v["path"] == "hello.txt").unwrap();
    assert_eq!(hello["previousText"], "before\n");
    assert_eq!(hello["content"], "after\n");
    assert!(hello["diff"].as_str().unwrap().contains("+after"));
    assert!(!result.to_string().contains("uncommitted secret"));
    assert_eq!(
        files.iter().find(|v| v["path"] == "[literal].txt").unwrap()["content"],
        "literal file\n"
    );
    assert!(files.iter().find(|v| v["path"] == "binary.dat").unwrap()["content"].is_null());
    assert!(
        files.iter().find(|v| v["path"] == "large.txt").unwrap()["truncated"]
            .as_bool()
            .unwrap()
    );
    assert!(result["truncated"].as_bool().unwrap());
    #[cfg(unix)]
    assert!(files.iter().find(|v| v["path"] == "outside-link").unwrap()["content"].is_null());
}

#[test]
fn changes_rejects_arbitrary_refs_stale_heads_and_cross_project_access() {
    let f = fixture();
    assert!(!protocol::worker_allowed("run_changes"));
    assert!(run::run_changes(&f.db, &f.task, "HEAD", &f.head).is_err());
    assert!(run::run_changes(&f.db, &f.task, &f.head, &f.head).is_err());
    let mut extra = request(&f);
    extra["repo"] = json!(f.repo);
    assert!(protocol::dispatch_scoped(&f.db, "run_changes", extra, None, Some("default")).is_err());
    let other = projects::dispatch(&f.db, "project_create", &json!({"slug":"other"}))
        .unwrap()
        .unwrap();
    assert!(
        protocol::dispatch_scoped(
            &f.db,
            "run_changes",
            request(&f),
            None,
            other["id"].as_str()
        )
        .is_err()
    );
    git::run(
        &f.repo,
        &["commit", "--allow-empty", "-m", "stale checkpoint"],
    )
    .unwrap();
    assert!(run::run_changes(&f.db, &f.task, &f.base, &f.head).is_err());
    let schema = protocol::admin_schema("run_changes");
    assert_eq!(schema["additionalProperties"], false);
    assert!(schema["properties"].get("repo").is_none());
    assert_eq!(
        schema["required"],
        json!(["task", "expected_base", "expected_head"])
    );
}
