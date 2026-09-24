use horde::{config::Settings, git, storage, store::Store, template};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

struct Fixture {
    _dir: tempfile::TempDir,
    db: Store,
    task: String,
    worker: String,
    repo: PathBuf,
    workspace: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.com"],
        ] {
            git::run(&repo, &args).unwrap();
        }
        std::fs::write(repo.join("tracked"), "keep").unwrap();
        git::run(&repo, &["add", "tracked"]).unwrap();
        git::run(&repo, &["commit", "-m", "initial"]).unwrap();
        let db = Store::open_with_config_dir(&dir.path().join("data"), &dir.path().join("config"))
            .unwrap();
        let plan = template::compile(
            "simulated",
            &template::load_templates(Path::new("absent")).unwrap(),
            BTreeMap::from([("task".into(), "cleanup".into())]),
        )
        .unwrap();
        let task = db
            .submit("cleanup", &repo, &Settings::default(), &plan)
            .unwrap();
        let worker = db.register(&task, None).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let workspace = git::allocate(&db, &task, &worker).unwrap();
        db.conn
            .execute("UPDATE tasks SET status='succeeded',created=1", [])
            .unwrap();
        db.conn
            .execute("UPDATE steps SET state='succeeded'", [])
            .unwrap();
        db.conn.execute("UPDATE workers SET updated=1", []).unwrap();
        db.conn.execute("UPDATE events SET created=1", []).unwrap();
        Self {
            _dir: dir,
            db,
            task,
            worker,
            repo,
            workspace,
        }
    }

    fn cleanup(&self, dry_run: bool) -> Value {
        storage::cleanup(&self.db, 60, 16, dry_run).unwrap()
    }

    fn assert_preserved(&self) {
        let result = self.cleanup(false);
        assert!(self.workspace.exists(), "{result}");
        assert_eq!(result["removed"].as_array().unwrap().len(), 0, "{result}");
    }
}

#[test]
fn clean_worker_checkout_is_removed_but_branch_history_and_integrated_checkout_survive() {
    let f = Fixture::new();
    let registered = f.db.worker(&f.worker).unwrap()["workspace"].clone();
    let branch = format!("refs/heads/workers/{}/{}", f.task, f.worker);
    let head = git::run(&f.workspace, &["rev-parse", "HEAD"]).unwrap();
    let result = f.cleanup(false);
    assert_eq!(result["removed"].as_array().unwrap().len(), 1, "{result}");
    assert!(!f.workspace.exists());
    assert_eq!(git::run(&f.repo, &["rev-parse", &branch]).unwrap(), head);
    assert!(f.workspace.parent().unwrap().join("integrated").is_dir());
    assert_eq!(f.db.worker(&f.worker).unwrap()["workspace"], registered);
    assert!(
        horde::management::value(&f.db, &format!("storage.cleaned.{}", f.worker))
            .unwrap()
            .is_some()
    );
    assert_eq!(f.cleanup(false)["removed"].as_array().unwrap().len(), 0);
}

#[test]
fn dry_run_and_retention_do_not_write_or_remove_workspaces() {
    let f = Fixture::new();
    let result = f.cleanup(true);
    assert_eq!(
        result["candidates"].as_array().unwrap().len(),
        1,
        "{result}"
    );
    assert!(f.workspace.exists());
    assert!(
        horde::management::value(&f.db, &format!("storage.cleaned.{}", f.worker))
            .unwrap()
            .is_none()
    );
    f.db.conn
        .execute("UPDATE workers SET updated=?", [horde::store::now()])
        .unwrap();
    f.assert_preserved();
}

#[test]
fn dirty_untracked_and_ignored_files_are_preserved() {
    for mode in ["dirty", "untracked", "ignored"] {
        let f = Fixture::new();
        match mode {
            "dirty" => std::fs::write(f.workspace.join("tracked"), "uncommitted").unwrap(),
            "untracked" => std::fs::write(f.workspace.join("new"), "untracked").unwrap(),
            _ => {
                let exclude =
                    git::run(&f.workspace, &["rev-parse", "--git-path", "info/exclude"]).unwrap();
                std::fs::write(f.workspace.join(exclude), "ignored\n").unwrap();
                std::fs::write(f.workspace.join("ignored"), "retain this too").unwrap();
            }
        }
        f.assert_preserved();
    }
}

#[test]
fn active_uncertain_claimed_and_unsuccessful_tasks_are_preserved() {
    for mode in [
        "running",
        "blocked",
        "failed",
        "cancelled",
        "uncertain",
        "claim",
        "notified",
    ] {
        let f = Fixture::new();
        match mode {
            "uncertain" => {
                let step = f.db.steps(&f.task).unwrap()[0]["id"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                f.db.conn.execute("INSERT INTO attempts(id,step,worker,state,started) VALUES('uncertain',?,?,'uncertain',1)", [&step, &f.worker]).unwrap();
            }
            "claim" => {
                f.db.conn
                    .execute(
                        "INSERT INTO claims VALUES(?,'tracked',?)",
                        [&f.task, &f.worker],
                    )
                    .unwrap();
            }
            "notified" => {
                f.db.conn
                    .execute("UPDATE workers SET status='notified'", [])
                    .unwrap();
            }
            status => {
                f.db.conn
                    .execute("UPDATE tasks SET status=?", [status])
                    .unwrap();
            }
        }
        f.assert_preserved();
    }
}

#[test]
fn unintegrated_commit_and_pending_integration_are_preserved() {
    let f = Fixture::new();
    std::fs::write(f.workspace.join("tracked"), "committed but not integrated").unwrap();
    git::run(&f.workspace, &["commit", "-am", "unintegrated"]).unwrap();
    f.assert_preserved();
    let f = Fixture::new();
    let head = git::run(&f.workspace, &["rev-parse", "HEAD"]).unwrap();
    f.db.conn
        .execute(
            "INSERT INTO integrations VALUES('pending',?,?,?,'queued',NULL,1)",
            [&f.task, &f.worker, &head],
        )
        .unwrap();
    f.assert_preserved();
}

#[test]
fn held_environment_and_remote_reservation_are_preserved() {
    for mode in ["environment", "reservation"] {
        let f = Fixture::new();
        if mode == "environment" {
            f.db.conn.execute("INSERT INTO app_environments(id,task,kind,state,spec,workspace,created,expires) VALUES('held',?,'process','held','{}',?,1,2)", [&f.task, f.workspace.to_str().unwrap()]).unwrap();
        } else {
            f.db.conn.execute("INSERT INTO project_remote_reservations VALUES('held','default',?,'remote','uncertain',1)", [&f.task]).unwrap();
        }
        f.assert_preserved();
    }
}

#[test]
fn custom_registered_workspace_is_preserved() {
    let f = Fixture::new();
    let custom = f._dir.path().join("custom");
    git::run(
        &f.repo,
        &[
            "worktree",
            "move",
            f.workspace.to_str().unwrap(),
            custom.to_str().unwrap(),
        ],
    )
    .unwrap();
    f.db.conn
        .execute(
            "UPDATE workers SET workspace=? WHERE id=?",
            [custom.to_str().unwrap(), &f.worker],
        )
        .unwrap();
    let result = f.cleanup(false);
    assert!(custom.exists());
    assert_eq!(result["removed"].as_array().unwrap().len(), 0, "{result}");
}

#[cfg(unix)]
#[test]
fn symlinked_task_directory_cannot_redirect_cleanup() {
    let f = Fixture::new();
    let task_dir = f.workspace.parent().unwrap();
    let moved = f._dir.path().join("moved");
    std::fs::rename(task_dir, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, task_dir).unwrap();
    f.assert_preserved();
    assert!(moved.join(&f.worker).exists());
}

#[test]
fn allocation_restores_a_cleaned_workspace_from_the_retained_branch() {
    let f = Fixture::new();
    let original = f.db.worker(&f.worker).unwrap();
    let result = f.cleanup(false);
    assert_eq!(result["removed"].as_array().unwrap().len(), 1, "{result}");
    let restored = git::allocate(&f.db, &f.task, &f.worker).unwrap();
    assert_eq!(
        restored,
        PathBuf::from(original["workspace"].as_str().unwrap())
    );
    assert_eq!(
        std::fs::read_to_string(restored.join("tracked")).unwrap(),
        "keep"
    );
    assert_eq!(f.db.worker(&f.worker).unwrap()["base"], original["base"]);
    assert!(
        horde::management::value(&f.db, &format!("storage.cleaned.{}", f.worker))
            .unwrap()
            .is_none()
    );
}

#[test]
fn allocation_does_not_recreate_an_unmarked_missing_workspace() {
    let f = Fixture::new();
    git::run(
        &f.repo,
        &["worktree", "remove", f.workspace.to_str().unwrap()],
    )
    .unwrap();
    assert!(git::allocate(&f.db, &f.task, &f.worker).is_err());
    assert!(!f.workspace.exists());
}

#[test]
fn skipped_old_registrations_do_not_starve_later_workspaces() {
    let f = Fixture::new();
    for index in 0..256 {
        f.db.conn.execute("INSERT INTO workers(id,task,status,token_hash,workspace,updated) VALUES(?,?,'idle','unused',?,1)", rusqlite::params![format!("!skipped-{index:04}"), f.task, format!("/unowned/{index}")]).unwrap();
    }
    assert_eq!(f.cleanup(false)["removed"].as_array().unwrap().len(), 0);
    assert!(f.workspace.exists());
    let next = f.cleanup(false);
    assert_eq!(next["removed"].as_array().unwrap().len(), 1, "{next}");
    assert!(!f.workspace.exists());
}

#[test]
fn tracked_edits_hidden_by_git_index_flags_are_preserved() {
    for flag in ["--assume-unchanged", "--skip-worktree"] {
        let f = Fixture::new();
        git::run(&f.workspace, &["update-index", flag, "tracked"]).unwrap();
        std::fs::write(f.workspace.join("tracked"), "valuable hidden edit").unwrap();
        assert!(
            git::run(&f.workspace, &["status", "--porcelain"])
                .unwrap()
                .is_empty()
        );
        f.assert_preserved();
        assert_eq!(
            std::fs::read_to_string(f.workspace.join("tracked")).unwrap(),
            "valuable hidden edit"
        );
    }
}

#[test]
fn failed_cleanup_intent_does_not_pin_an_intact_workspace_to_an_old_head() {
    let f = Fixture::new();
    let report = f.cleanup(true);
    let mut marker = report["candidates"][0].clone();
    marker["state"] = serde_json::json!("removing");
    horde::management::set(
        &f.db,
        &format!("storage.cleaned.{}", f.worker),
        &marker.to_string(),
    )
    .unwrap();
    std::fs::write(f.workspace.join("tracked"), "new legitimate commit").unwrap();
    git::run(&f.workspace, &["commit", "-am", "after failed cleanup"]).unwrap();
    let head = git::run(&f.workspace, &["rev-parse", "HEAD"]).unwrap();
    git::allocate(&f.db, &f.task, &f.worker).unwrap();
    assert_eq!(
        git::run(&f.workspace, &["rev-parse", "HEAD"]).unwrap(),
        head
    );
    assert!(
        horde::management::value(&f.db, &format!("storage.cleaned.{}", f.worker))
            .unwrap()
            .is_none()
    );
}
