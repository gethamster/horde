use horde::{config::Settings, projects, store::Store, template};
use serde_json::json;
use std::collections::BTreeMap;

fn repository(path: &std::path::Path) {
    std::fs::create_dir_all(path).unwrap();
    for args in [
        ["init", "-b", "main"].as_slice(),
        &["config", "user.name", "Test"],
        &["config", "user.email", "test@localhost"],
        &["commit", "--allow-empty", "-m", "initial"],
    ] {
        horde::git::run(path, args).unwrap();
    }
}
fn create(db: &Store, slug: &str) -> String {
    projects::dispatch(db, "project_create", &json!({"slug":slug}))
        .unwrap()
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn project_tenant_binding_is_operator_owned_and_immutable_after_work() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let first = projects::dispatch(
        &db,
        "project_create",
        &json!({"slug":"first","tenant_id":"shared-tenant"}),
    )
    .unwrap()
    .unwrap();
    let second = projects::dispatch(
        &db,
        "project_create",
        &json!({"slug":"second","tenant_id":"shared-tenant"}),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        projects::tenant(&db, first["id"].as_str().unwrap()).unwrap(),
        "shared-tenant"
    );
    assert_eq!(
        projects::tenant(&db, second["id"].as_str().unwrap()).unwrap(),
        "shared-tenant"
    );
    assert!(
        projects::dispatch(
            &db,
            "project_update",
            &json!({"project":first["id"],"tenant_id":"other"})
        )
        .is_err()
    );
    assert!(
        db.conn
            .execute(
                "UPDATE project_tenants SET tenant_id='other' WHERE project=?",
                [first["id"].as_str().unwrap()]
            )
            .is_err()
    );
    let legacy = create(&db, "legacy");
    assert_eq!(projects::tenant(&db, &legacy).unwrap(), legacy);
}

#[test]
fn schema_six_migration_defaults_legacy_run_and_tenant_identity() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    repository(&repo);
    let db = Store::open(&dir.path().join("data")).unwrap();
    let project = create(&db, "legacy-project");
    projects::dispatch(
        &db,
        "project_repo_add",
        &json!({"project":project,"path":repo}),
    )
    .unwrap();
    let plan = template::compile(
        "simulated",
        &template::load_templates(&repo).unwrap(),
        BTreeMap::from([("task".into(), "legacy".into())]),
    )
    .unwrap();
    let task = db
        .submit_project(&project, "legacy", &repo, &Settings::default(), &plan)
        .unwrap();
    db.conn
        .execute_batch(
            "DROP TABLE run_bindings; DROP TABLE project_tenants; PRAGMA user_version=6;",
        )
        .unwrap();
    drop(db);
    let reopened = Store::open(&dir.path().join("data")).unwrap();
    assert_eq!(projects::tenant(&reopened, &project).unwrap(), project);
    let binding = horde::run::run_context(&reopened, &task).unwrap();
    assert_eq!(binding["tenant_id"], project);
    assert_eq!(binding["project"], project);
    assert!(binding["thread_id"].is_null());
    assert!(
        std::fs::read_dir(dir.path().join("data"))
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().starts_with("pre-runs-"))
    );
}
#[test]
fn repositories_and_worktrees_have_one_owner_and_tasks_are_immutable() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(&dir.path().join("data")).unwrap();
    let repo = dir.path().join("repo");
    repository(&repo);
    let horde = create(&db, "horde");
    projects::dispatch(
        &db,
        "project_runtime_grant",
        &json!({"project":horde,"runtime":"local"}),
    )
    .unwrap();
    let hamster = create(&db, "hamster");
    projects::dispatch(
        &db,
        "project_repo_add",
        &json!({"project":horde,"path":repo}),
    )
    .unwrap();
    assert_eq!(projects::infer(&db, &repo).unwrap(), Some(horde.clone()));
    let worktree = dir.path().join("worktree");
    horde::git::run(
        &repo,
        &["worktree", "add", "-b", "work", worktree.to_str().unwrap()],
    )
    .unwrap();
    assert_eq!(
        projects::infer(&db, &worktree).unwrap(),
        Some(horde.clone())
    );
    assert!(
        projects::dispatch(
            &db,
            "project_repo_add",
            &json!({"project":hamster,"path":worktree})
        )
        .is_err()
    );
    let plan = template::compile(
        "simulated",
        &template::load_templates(&repo).unwrap(),
        BTreeMap::from([("task".into(), "test".into())]),
    )
    .unwrap();
    let task = db
        .submit("test", &repo, &Settings::default(), &plan)
        .unwrap();
    assert_eq!(projects::task_project(&db, &task).unwrap(), horde);
    assert!(projects::authorize_task(&db, &hamster, &task).is_err());
    assert!(projects::bind_task(&db, &task, &hamster, &repo).is_err());
    assert!(
        db.conn
            .execute(
                "UPDATE task_projects SET project=? WHERE task=?",
                [&hamster, &task]
            )
            .is_err()
    );
    let child = horde::delegation::delegate(
        &db,
        &task,
        &json!({"id":"child","objective":"small work","template":"simulated"}),
    )
    .unwrap();
    assert_eq!(
        projects::task_project(&db, child["id"].as_str().unwrap()).unwrap(),
        horde
    );
}
#[test]
fn projects_start_without_runtime_grants_and_validate_settings() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let id = create(&db, "hamster");
    assert!(!projects::runtime_allowed(&db, &id, "local").unwrap());
    projects::dispatch(
        &db,
        "project_runtime_grant",
        &json!({"project":id,"runtime":"local"}),
    )
    .unwrap();
    assert!(projects::runtime_allowed(&db, &id, "local").unwrap());
    projects::dispatch(
        &db,
        "project_runtime_revoke",
        &json!({"project":id,"runtime":"local"}),
    )
    .unwrap();
    assert!(!projects::runtime_allowed(&db, &id, "local").unwrap());
    assert!(
        projects::dispatch(
            &db,
            "project_update",
            &json!({"project":id,"concurrency":0})
        )
        .is_err()
    );
    assert!(projects::dispatch(&db, "project_create", &json!({"slug":"../bad"})).is_err());
}
#[test]
fn schema_five_migration_backs_up_before_changes() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    db.conn.pragma_update(None, "user_version", 5).unwrap();
    drop(db);
    let db = Store::open(dir.path()).unwrap();
    assert_eq!(
        db.conn
            .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        horde::store::SCHEMA_VERSION
    );
    let backups: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|p| p.file_name().to_string_lossy().starts_with("pre-projects-"))
        .collect();
    assert_eq!(backups.len(), 1);
    let backup = rusqlite::Connection::open(backups[0].path()).unwrap();
    assert_eq!(
        backup
            .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        5
    );
    drop(db);
    Store::open(dir.path()).unwrap();
    assert_eq!(
        std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|p| p.file_name().to_string_lossy().starts_with("pre-projects-"))
            .count(),
        1
    );
}

#[test]
fn cross_repository_children_require_registration_in_same_project() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(&dir.path().join("data")).unwrap();
    let primary = dir.path().join("primary");
    let other = dir.path().join("other");
    let foreign = dir.path().join("foreign");
    for path in [&primary, &other, &foreign] {
        repository(path);
    }
    let project = create(&db, "horde");
    projects::dispatch(
        &db,
        "project_runtime_grant",
        &json!({"project":project,"runtime":"local"}),
    )
    .unwrap();
    let hamster = create(&db, "hamster");
    for (project, path) in [
        (&project, &primary),
        (&project, &other),
        (&hamster, &foreign),
    ] {
        projects::dispatch(
            &db,
            "project_repo_add",
            &json!({"project":project,"path":path}),
        )
        .unwrap();
    }
    let plan = template::compile(
        "simulated",
        &template::load_templates(&primary).unwrap(),
        BTreeMap::from([("task".into(), "test".into())]),
    )
    .unwrap();
    let task = db
        .submit_project(&project, "test", &primary, &Settings::default(), &plan)
        .unwrap();
    let child=horde::delegation::delegate(&db,&task,&json!({"id":"other","objective":"other repository work","template":"simulated","repo":other})).unwrap();
    assert_eq!(
        projects::task_project(&db, child["id"].as_str().unwrap()).unwrap(),
        project
    );
    let rows = db
        .rows(
            "SELECT repository FROM task_projects WHERE task IN (?,?) ORDER BY task",
            &[&task, &child["id"].as_str().unwrap()],
        )
        .unwrap();
    assert_ne!(rows[0]["repository"], rows[1]["repository"]);
    assert!(horde::delegation::delegate(&db,&task,&json!({"id":"foreign","objective":"cross-project work","template":"simulated","repo":foreign})).is_err());
}

#[test]
fn explicit_project_rejects_repository_owned_elsewhere_without_task_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(&dir.path().join("data")).unwrap();
    let repo = dir.path().join("repo");
    repository(&repo);
    let a = create(&db, "a");
    let b = create(&db, "b");
    projects::register_repository(&db, &a, &repo).unwrap();
    let plan = template::compile(
        "simulated",
        &template::load_templates(&repo).unwrap(),
        BTreeMap::from([("task".into(), "test".into())]),
    )
    .unwrap();
    assert!(
        db.submit_project(&b, "bad", &repo, &Settings::default(), &plan)
            .is_err()
    );
    assert!(db.rows("SELECT id FROM tasks", &[]).unwrap().is_empty());
}

#[test]
fn project_configuration_is_validated_before_atomic_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(&dir.path().join("data")).unwrap();
    let project = create(&db, "horde");
    let source = dir.path().join("settings.toml");
    std::fs::write(&source, "concurrency = 2\n").unwrap();
    projects::dispatch(
        &db,
        "project_configure",
        &json!({"project":project,"file":source}),
    )
    .unwrap();
    let destination = projects::storage_root(&db, &project)
        .unwrap()
        .join("config.toml");
    assert_eq!(
        std::fs::read_to_string(&destination).unwrap(),
        "concurrency = 2\n"
    );
    std::fs::write(&source, "concurrency = 0\n").unwrap();
    assert!(
        projects::dispatch(
            &db,
            "project_configure",
            &json!({"project":project,"file":source})
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(&destination).unwrap(),
        "concurrency = 2\n"
    );
    assert!(
        projects::dispatch(
            &db,
            "project_configure",
            &json!({"project":"default","file":source})
        )
        .is_err()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(destination).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn legacy_tasks_keep_ids_paths_and_uncertain_attempts_during_migration() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let db = Store::open(&data).unwrap();
    let repo = dir.path().join("repo");
    repository(&repo);
    let plan = template::compile(
        "simulated",
        &template::load_templates(&repo).unwrap(),
        BTreeMap::from([("task".into(), "legacy".into())]),
    )
    .unwrap();
    let task = db
        .submit("legacy", &repo, &Settings::default(), &plan)
        .unwrap();
    let step = db
        .rows("SELECT id FROM steps WHERE task=? LIMIT 1", &[&task])
        .unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    db.conn.execute("INSERT INTO attempts(id,step,state,started) VALUES('uncertain-attempt',?,'uncertain',1)",[step]).unwrap();
    db.conn
        .execute("UPDATE tasks SET status='blocked' WHERE id=?", [&task])
        .unwrap();
    db.conn.execute("DELETE FROM task_projects", []).unwrap();
    db.conn
        .execute("DELETE FROM project_repositories", [])
        .unwrap();
    db.conn.pragma_update(None, "user_version", 5).unwrap();
    drop(db);
    let db = Store::open(&data).unwrap();
    assert_eq!(projects::task_project(&db, &task).unwrap(), "default");
    assert_eq!(
        projects::infer(&db, &repo).unwrap().as_deref(),
        Some("default")
    );
    assert_eq!(db.task(&task).unwrap()["repo"], repo.to_str().unwrap());
    assert_eq!(db.task(&task).unwrap()["status"], "blocked");
    assert_eq!(
        db.rows(
            "SELECT state FROM attempts WHERE id='uncertain-attempt'",
            &[]
        )
        .unwrap()[0]["state"],
        "uncertain"
    );
}

#[test]
fn local_runtime_aliases_share_grants_and_explicit_revocation() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let project = create(&db, "horde");
    horde::federation::configure(
        &db.root,
        &horde::network::NetworkConfig {
            runtime_id: "controller".into(),
            ..Default::default()
        },
    )
    .unwrap();
    projects::dispatch(
        &db,
        "project_runtime_grant",
        &json!({"project":project,"runtime":"local"}),
    )
    .unwrap();
    assert!(projects::runtime_allowed(&db, &project, "controller").unwrap());
    projects::dispatch(
        &db,
        "project_runtime_revoke",
        &json!({"project":project,"runtime":"controller"}),
    )
    .unwrap();
    assert!(!projects::runtime_allowed(&db, &project, "local").unwrap());
    projects::dispatch(
        &db,
        "project_runtime_grant",
        &json!({"project":project,"runtime":"local"}),
    )
    .unwrap();
    assert!(projects::runtime_allowed(&db, &project, "controller").unwrap());
    assert!(projects::runtime_allowed(&db, "default", "legacy-runtime").unwrap());
    projects::dispatch(
        &db,
        "project_runtime_revoke",
        &json!({"project":"default","runtime":"legacy-runtime"}),
    )
    .unwrap();
    assert!(!projects::runtime_allowed(&db, "default", "legacy-runtime").unwrap());
}

#[test]
fn project_slugs_cannot_shadow_immutable_project_ids() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let project = create(&db, "horde");
    assert!(projects::dispatch(&db, "project_create", &json!({"slug":project})).is_err());
    assert_eq!(projects::resolve(&db, &project).unwrap(), project);
}

#[test]
fn migration_keeps_legacy_child_repository_lineage_for_integration() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let db = Store::open(&data).unwrap();
    let repo = dir.path().join("repo");
    repository(&repo);
    let plan = template::compile(
        "simulated",
        &template::load_templates(&repo).unwrap(),
        BTreeMap::from([("task".into(), "legacy".into())]),
    )
    .unwrap();
    let parent = db
        .submit("legacy", &repo, &Settings::default(), &plan)
        .unwrap();
    let child = horde::delegation::delegate(
        &db,
        &parent,
        &json!({"id":"child","objective":"legacy child","template":"simulated"}),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let child_repo = db.task(&child).unwrap()["repo"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(child_repo, repo.to_str().unwrap());
    db.conn.execute("DELETE FROM task_projects", []).unwrap();
    db.conn
        .execute("DELETE FROM project_repositories", [])
        .unwrap();
    db.conn.pragma_update(None, "user_version", 5).unwrap();
    drop(db);
    let db = Store::open(&data).unwrap();
    let same:bool=db.conn.query_row("SELECT a.repository=b.repository FROM task_projects a JOIN task_projects b ON b.task=? WHERE a.task=?",[&child,&parent],|r|r.get(0)).unwrap();
    assert!(same);
    assert_eq!(db.task(&child).unwrap()["repo"], child_repo);
    assert_eq!(
        projects::infer(&db, std::path::Path::new(&child_repo))
            .unwrap()
            .as_deref(),
        Some("default")
    );
}

#[test]
fn dedicated_runtime_denies_default_and_other_projects_even_with_legacy_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let owner = create(&db, "hamster");
    let other = create(&db, "horde");
    projects::dispatch(
        &db,
        "project_runtime_grant",
        &json!({"project":owner,"runtime":"guest","dedicated":true}),
    )
    .unwrap();
    assert!(projects::runtime_allowed(&db, &owner, "guest").unwrap());
    assert!(!projects::runtime_allowed(&db, "default", "guest").unwrap());
    assert!(!projects::runtime_allowed(&db, &other, "guest").unwrap());
    assert!(
        projects::dispatch(
            &db,
            "project_runtime_grant",
            &json!({"project":other,"runtime":"guest"})
        )
        .is_err()
    );
    assert!(
        projects::dispatch(
            &db,
            "project_runtime_grant",
            &json!({"project":"default","runtime":"guest"})
        )
        .is_err()
    );
}

#[test]
fn newly_managed_runtime_does_not_inherit_default_access() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    db.conn.execute("INSERT INTO managed_runtimes(id,profile,spec,state,created) VALUES('new-guest','profile','{}','provisioned',1)",[]).unwrap();
    assert!(!projects::runtime_allowed(&db, "default", "new-guest").unwrap());
    projects::dispatch(
        &db,
        "project_runtime_grant",
        &json!({"project":"default","runtime":"new-guest"}),
    )
    .unwrap();
    assert!(projects::runtime_allowed(&db, "default", "new-guest").unwrap());
}

#[test]
fn migration_preserves_existing_managed_runtime_default_grant() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    db.conn.execute("INSERT INTO managed_runtimes(id,profile,spec,state,created) VALUES('legacy-guest','profile','{}','provisioned',1)",[]).unwrap();
    db.conn.pragma_update(None, "user_version", 5).unwrap();
    drop(db);
    let db = Store::open(dir.path()).unwrap();
    assert!(projects::runtime_allowed(&db, "default", "legacy-guest").unwrap());
}

#[test]
fn local_dedication_rejects_alias_regrant_and_preserves_owner_on_revoke() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let owner = create(&db, "hamster");
    horde::federation::configure(
        &db.root,
        &horde::network::NetworkConfig {
            runtime_id: "controller".into(),
            ..Default::default()
        },
    )
    .unwrap();
    projects::bind_runtime(&db, &owner, "controller").unwrap();
    assert!(!projects::runtime_allowed(&db, "default", "local").unwrap());
    assert!(
        projects::dispatch(
            &db,
            "project_runtime_grant",
            &json!({"project":"default","runtime":"local"})
        )
        .is_err()
    );
    projects::dispatch(
        &db,
        "project_runtime_revoke",
        &json!({"project":owner,"runtime":"local"}),
    )
    .unwrap();
    assert!(!projects::runtime_allowed(&db, &owner, "controller").unwrap());
    assert!(projects::bind_runtime(&db, "default", "local").is_err());
}

#[test]
fn schema_five_upgrade_refuses_active_daemon_before_backup_or_schema_changes() {
    use fs2::FileExt;
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    db.conn
        .execute_batch("DROP TABLE runtime_project_bindings; PRAGMA user_version=5;")
        .unwrap();
    drop(db);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.path().join("daemon.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    let result = Store::open(dir.path());
    assert!(
        result.is_err(),
        "upgrade must reject an existing daemon lock"
    );
    assert!(result.err().unwrap().to_string().contains("stop"));
    let conn = rusqlite::Connection::open(dir.path().join("state.sqlite3")).unwrap();
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        5
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='runtime_project_bindings'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert!(
        !std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().starts_with("pre-projects-"))
    );
    drop(conn);
    drop(lock);
    // Other tests in this binary spawn processes too. A child forked while `lock` was
    // open holds a copy of it, and with it the flock, until that child execs. Wait for
    // the lock to come free, then unlock the probe explicitly so that a copy forked
    // from the probe cannot hold it in turn.
    let probe = std::fs::File::open(dir.path().join("daemon.lock")).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while let Err(error) = probe.try_lock_exclusive() {
        assert!(
            error.kind() == std::io::ErrorKind::WouldBlock && std::time::Instant::now() < deadline,
            "dropping the daemon lock must release it: {error}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    probe.unlock().unwrap();
    drop(probe);
    Store::open(dir.path()).unwrap();
}

#[test]
fn current_schema_reopen_is_read_only_while_another_connection_writes() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    db.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = std::time::Instant::now();
    let reopened = Store::open(dir.path()).unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(
        reopened
            .conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    db.conn.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn cached_local_identity_survives_corrupt_config_and_keeps_dedication() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let owner = create(&db, "hamster");
    horde::federation::configure(
        &db.root,
        &horde::network::NetworkConfig {
            runtime_id: "guest".into(),
            ..Default::default()
        },
    )
    .unwrap();
    projects::bind_runtime(&db, &owner, "guest").unwrap();
    std::fs::write(db.root.join("network-runtime.toml"), "invalid = [").unwrap();
    assert!(projects::runtime_allowed(&db, &owner, "guest").unwrap());
    assert!(!projects::runtime_allowed(&db, "default", "local").unwrap());
    assert!(!projects::runtime_allowed(&db, "default", "guest").unwrap());
    drop(db);
    let db = Store::open(dir.path()).unwrap();
    assert!(!projects::runtime_allowed(&db, "default", "guest").unwrap());
}

#[test]
fn runtime_revocation_retains_remote_reservations_and_requests_credential_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(&dir.path().join("data")).unwrap();
    let repo = dir.path().join("repo");
    repository(&repo);
    let plan = template::compile(
        "simulated",
        &template::load_templates(&repo).unwrap(),
        BTreeMap::from([("task".into(), "test".into())]),
    )
    .unwrap();
    let task = db
        .submit("test", &repo, &Settings::default(), &plan)
        .unwrap();
    let account = horde::accounts::dispatch(
        &db,
        "account_create",
        &json!({"project":"default","name":"subscription","provider":"codex","auth_mode":"login"}),
    )
    .unwrap()
    .unwrap();
    db.conn.execute("INSERT INTO project_remote_reservations VALUES('reservation','default',?,'guest','active',1)",[&task]).unwrap();
    db.conn.execute("INSERT INTO account_remote_reservations VALUES('reservation','default',?,'guest',?,'attempt','worker',?,1,'active',1)",rusqlite::params![account["id"].as_str().unwrap(),task,account["profile"].as_str().unwrap()]).unwrap();
    db.conn.execute("INSERT INTO credential_deliveries VALUES('reservation','default',?,'guest',1,'replacement_pending',1)",[account["id"].as_str().unwrap()]).unwrap();
    projects::dispatch(
        &db,
        "project_runtime_revoke",
        &json!({"project":"default","runtime":"guest"}),
    )
    .unwrap();
    for table in ["project_remote_reservations", "account_remote_reservations"] {
        assert_eq!(
            db.rows(
                &format!("SELECT state FROM {table} WHERE request_id='reservation'"),
                &[]
            )
            .unwrap()[0]["state"],
            "revoked"
        );
    }
    assert_eq!(
        db.rows(
            "SELECT state FROM credential_deliveries WHERE request_id='reservation'",
            &[]
        )
        .unwrap()[0]["state"],
        "revocation_pending"
    );
}
