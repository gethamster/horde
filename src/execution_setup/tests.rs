use super::*;

fn setup() -> (tempfile::TempDir, Store, String, Layout) {
    let temp = tempfile::tempdir().unwrap();
    let db = Store::open(&temp.path().join("data")).unwrap();
    let project = crate::projects::dispatch(&db, "project_create", &json!({"slug":"example"}))
        .unwrap()
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let layout = Layout {
        home: temp.path().join("home"),
        workspace: temp.path().join("workspace"),
        rustup: temp.path().join("rustup"),
        target: temp.path().join("target"),
        executable: PathBuf::from("/usr/local/bin/horde"),
    };
    (temp, db, project, layout)
}
fn request(db: &Store, project: &str) -> Request {
    validate(db, &json!({"scope":"local","projects":[{"id":project,"slug":"example","git_proxy_token":"test-secret-value-at-least-32-bytes"}]})).unwrap()
}

#[test]
fn configure_is_repeatable_preserves_settings_and_never_returns_tokens() {
    let (_temp, db, project, layout) = setup();
    std::fs::create_dir_all(layout.home.join(".cargo")).unwrap();
    std::fs::write(
        layout.home.join(".gitconfig"),
        "[user]\n name = Existing User\n",
    )
    .unwrap();
    std::fs::write(layout.home.join(".cargo/config.toml"), "[net]\nretry = 5\n").unwrap();
    let first = configure_request(&db, request(&db, &project), layout.clone()).unwrap();
    let second = configure_request(&db, request(&db, &project), layout.clone()).unwrap();
    assert_eq!(first, second);
    assert!(!first.to_string().contains("test-secret"));
    let git = std::fs::read_to_string(layout.home.join(".gitconfig")).unwrap();
    assert!(git.contains("Existing User"));
    assert_eq!(git.matches("git-credential").count(), 1);
    assert!(!git.contains("python"));
    assert!(!git.contains("test-secret"));
    let cargo = std::fs::read_to_string(layout.home.join(".cargo/config.toml")).unwrap();
    assert!(cargo.contains("retry = 5"));
    let persisted = std::fs::read_to_string(db.root.join("private/execution.json")).unwrap();
    assert!(!persisted.contains("test-secret"));
    use std::os::unix::fs::PermissionsExt;
    let token = db.root.join("private/git").join(&project).join("token");
    assert_eq!(
        std::fs::metadata(token).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn helper_restricts_credentials_to_exact_protocol_host_and_project() {
    let (_temp, db, project, layout) = setup();
    configure_request(&db, request(&db, &project), layout).unwrap();
    let valid = "protocol=http\nhost=deliver-bridge:8090\npath=git/example\n\n";
    let response = credential_helper(&db.root, &project, "get", valid).unwrap();
    assert!(response.contains("password=test-secret-value-at-least-32-bytes"));
    for invalid in [
        valid.replace("http", "https"),
        valid.replace(":8090", ":8091"),
        valid.replace("git/example", "git/other"),
        valid.replace("git/example", "git/example/child"),
        format!("protocol=https\n{valid}"),
        valid.replace("git/example", "git/%65xample"),
    ] {
        assert_eq!(
            credential_helper(&db.root, &project, "get", &invalid).unwrap(),
            ""
        );
    }
}

#[test]
fn newly_granted_account_gets_profile_and_existing_codex_roots_survive() {
    let (_temp, db, project, layout) = setup();
    configure_request(&db, request(&db, &project), layout.clone()).unwrap();
    let account = crate::accounts::dispatch(
        &db,
        "account_create",
        &json!({"project":project,"name":"new","provider":"codex","auth_mode":"login"}),
    )
    .unwrap()
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let profile = crate::accounts::profile_directory(&db.root, &project, &account).unwrap();
    std::fs::create_dir_all(profile.join("codex")).unwrap();
    std::fs::write(
        profile.join("codex/config.toml"),
        "model = 'kept'\n[sandbox_workspace_write]\nwritable_roots = [\n '/existing',\n]\n",
    )
    .unwrap();
    let executor = crate::config::ExecutorConfig {
        kind: "codex".into(),
        auth_mode: "login".into(),
        account: Some(account.clone()),
        ..Default::default()
    };
    crate::account_auth::command(&db, &project, &account, &executor).unwrap();
    ensure_account(&db, &project, &account).unwrap();
    assert_eq!(
        std::fs::read_link(profile.join("home/.rustup")).unwrap(),
        layout.rustup
    );
    let codex = std::fs::read_to_string(profile.join("codex/config.toml")).unwrap();
    let config: toml::Value = toml::from_str(&codex).unwrap();
    let roots = config["sandbox_workspace_write"]["writable_roots"]
        .as_array()
        .unwrap();
    assert_eq!(roots.len(), 5);
    assert_eq!(roots[0].as_str(), Some("/existing"));
    assert_eq!(config["model"].as_str(), Some("kept"));
}

#[test]
fn restoration_uses_persisted_layout_and_preserves_populated_directories() {
    let (_temp, db, project, layout) = setup();
    configure_request(&db, request(&db, &project), layout.clone()).unwrap();
    std::fs::remove_file(layout.home.join(".rustup")).unwrap();
    std::fs::create_dir(layout.home.join(".rustup")).unwrap();
    std::fs::write(layout.home.join(".rustup/keep"), "user data").unwrap();
    assert!(restore(&db).is_err());
    assert_eq!(
        std::fs::read_to_string(layout.home.join(".rustup/keep")).unwrap(),
        "user data"
    );
}

#[test]
fn invalid_codex_roots_fail_without_overwriting_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    let original = "[sandbox_workspace_write]\nwritable_roots = [42]\n";
    std::fs::write(&path, original).unwrap();
    assert!(files::codex(&path, &[PathBuf::from("/valid")]).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
}

#[test]
fn credential_helper_refuses_symlinked_token_and_project_directories() {
    let (_temp, db, project, layout) = setup();
    configure_request(&db, request(&db, &project), layout).unwrap();
    let folder = db.root.join("private/git").join(&project);
    let saved = db.root.join("saved-token");
    std::fs::rename(folder.join("token"), &saved).unwrap();
    std::os::unix::fs::symlink(&saved, folder.join("token")).unwrap();
    let input = "protocol=http\nhost=deliver-bridge:8090\npath=git/example\n\n";
    assert!(credential_helper(&db.root, &project, "get", input).is_err());
    std::fs::remove_file(folder.join("token")).unwrap();
    std::fs::rename(&saved, folder.join("token")).unwrap();
    let moved = db.root.join("moved-project");
    std::fs::rename(&folder, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &folder).unwrap();
    assert!(credential_helper(&db.root, &project, "get", input).is_err());
}

#[test]
fn daemon_bootstrap_observes_first_sidecar_profile_without_restart() {
    let (_temp, db, project, layout) = setup();
    restore_with_bootstrap(&db, Some(layout.clone())).unwrap();
    assert!(!db.root.join("private/execution.json").exists());
    assert_eq!(
        std::fs::read_link(layout.home.join(".gitconfig")).unwrap(),
        db.root.join("private/git/config")
    );
    // The sidecar has a separate home filesystem but the same /data volume.
    let sidecar = Layout {
        home: layout.home.with_file_name("sidecar-home"),
        ..layout.clone()
    };
    configure_request(&db, request(&db, &project), sidecar).unwrap();
    let controller_git = std::fs::read_to_string(layout.home.join(".gitconfig")).unwrap();
    assert!(controller_git.contains("git-credential"));
    assert!(controller_git.contains(&project));
}
