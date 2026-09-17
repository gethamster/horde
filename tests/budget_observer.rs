//! The workspace observer runs on its own schedule, and a loaded host stretches
//! that schedule without bound. This binary slows the observer's own `git`
//! calls so one sample takes longer than the budget leaves for it, and checks
//! that a command still writing to its workspace is never ruled idle.
//!
//! The command runs through the executor alone: its idle tail is then the 0.4 s
//! after its last write, which the budget covers on any host, rather than a
//! scope validation whose own git spawns a loaded host can stretch past it.
//!
//! It is its own binary because the slow `git` is installed through `PATH`,
//! which is process-wide; nothing else may run in this process.
use horde::{budget, config::Settings, executor, git, store::Store, template};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

/// Put a `git` first on `PATH` that delays only the two calls no other code path
/// shares with the observer, so the observer's cycle outgrows the budget's slack
/// while the fixture keeps its real speed.
fn slow_observer_git(dir: &Path) {
    let real = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path).find_map(|p| {
                let candidate = p.join("git");
                candidate.is_file().then_some(candidate)
            })
        })
        .expect("git on PATH");
    let shim = dir.join("git");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\ncase \"$*\" in *rev-parse*|*--no-ext-diff*) sleep 0.3;; esac\nexec {} \"$@\"\n",
            real.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(dir.to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    // SAFETY: this binary holds a single test, and nothing has spawned a thread yet.
    unsafe { std::env::set_var("PATH", path) };
}

fn fixture(dir: &Path) -> (Store, String, String, String) {
    let repo = dir.join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "Test"],
        vec!["commit", "--allow-empty", "-m", "initial"],
    ] {
        git::run(&repo, &args).unwrap();
    }
    let db = Store::open(&dir.join("data")).unwrap();
    let plan = template::compile(
        "simulated",
        &template::load_templates(Path::new("absent")).unwrap(),
        BTreeMap::from([("task".into(), "budget".into())]),
    )
    .unwrap();
    let task = db
        .submit("budget", &repo, &Settings::default(), &plan)
        .unwrap();
    let step = db.steps(&task).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let worker = db.register(&task, Some(&step)).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    git::allocate(&db, &task, &worker).unwrap();
    db.claim(&task, &worker, &[".".into()]).unwrap();
    db.conn
        .execute("UPDATE steps SET state='running' WHERE id=?", [&step])
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO attempts(id,step,worker,state,started) VALUES('attempt',?,?,'running',?)",
            rusqlite::params![step, worker, horde::store::now()],
        )
        .unwrap();
    (db, task, step, worker)
}

#[tokio::test(flavor = "current_thread")]
async fn a_slow_observer_never_rules_a_writing_command_idle() {
    let dir = tempfile::tempdir().unwrap();
    slow_observer_git(dir.path());
    tokio::task::LocalSet::new()
        .run_until(async {
            let (db, task, step, worker) = fixture(dir.path());
            let workspace = db.worker(&worker).unwrap()["workspace"]
                .as_str()
                .unwrap()
                .to_owned();
            let argv: Vec<String> = ["sh", "-c", "sleep 0.4; echo one > a; sleep 0.4; echo two > a; sleep 0.4; echo three > a; sleep 0.4"]
                .map(String::from)
                .to_vec();
            let result: anyhow::Result<Value> = budget::supervise(
                &db,
                &task,
                &step,
                "attempt",
                &worker,
                Some(1),
                async {
                    executor::run_command(&argv, Path::new(&workspace), 30, Some((&db, "attempt"))).await?;
                    Ok(json!({"accepted":true}))
                },
            )
            .await;
            assert!(result.is_ok(), "{result:?}");
            let observed = db
                .rows(
                    "SELECT data FROM events WHERE task=? AND kind='step.progress'",
                    &[&task],
                )
                .unwrap();
            assert!(!observed.is_empty(), "no workspace change was recorded");
        })
        .await;
}
