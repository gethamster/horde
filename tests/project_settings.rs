use horde::{config::Settings, projects, store::Store};
use serde_json::json;

fn fixture() -> (tempfile::TempDir, Store, String) {
    let temp = tempfile::tempdir().unwrap();
    // An unused configuration directory keeps the developer's own settings out.
    let db = Store::open_with_config_dir(&temp.path().join("state"), &temp.path().join("config"))
        .unwrap();
    let result = projects::dispatch(
        &db,
        "project_create",
        &json!({"name":"hamster","slug":"hamster"}),
    )
    .unwrap()
    .unwrap();
    let project = result["id"].as_str().unwrap().to_owned();
    (temp, db, project)
}

#[test]
fn repository_cannot_replace_project_credentials_or_enable_delivery() {
    let (temp, db, project) = fixture();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(repo.join(".horde")).unwrap();
    std::fs::write(
        repo.join(".horde/horde.toml"),
        "[providers.default]\napi_key_env='OTHER_PROJECT_KEY'\n",
    )
    .unwrap();
    assert!(
        Settings::load_project(&db, &project, &repo)
            .unwrap_err()
            .to_string()
            .contains("provider")
    );
    std::fs::write(repo.join(".horde/horde.toml"), "[delivery]\nenabled=true\n").unwrap();
    assert!(
        Settings::load_project(&db, &project, &repo)
            .unwrap_err()
            .to_string()
            .contains("delivery")
    );
    std::fs::write(
        repo.join(".horde/horde.toml"),
        "[decision]\nmode='disabled'\n",
    )
    .unwrap();
    assert!(
        Settings::load_project(&db, &project, &repo)
            .unwrap_err()
            .to_string()
            .contains("decision")
    );
    std::fs::write(
        repo.join(".horde/horde.toml"),
        "[automatic_delivery]\nenabled=false\n",
    )
    .unwrap();
    assert!(
        Settings::load_project(&db, &project, &repo)
            .unwrap_err()
            .to_string()
            .contains("automatic delivery")
    );
}

#[test]
fn repository_may_narrow_commands_and_workflow_limits() {
    let (temp, db, project) = fixture();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(repo.join(".horde")).unwrap();
    std::fs::write(
        repo.join(".horde/horde.toml"),
        "concurrency=1\nallow_commands=false\n",
    )
    .unwrap();
    let settings = Settings::load_project(&db, &project, &repo).unwrap();
    assert_eq!(settings.concurrency, 1);
    assert!(!settings.allow_commands);
}

#[test]
fn repository_cannot_load_skills_from_another_checkout() {
    let (temp, db, project) = fixture();
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(repo.join(".horde")).unwrap();
    std::fs::write(
        repo.join(".horde/horde.toml"),
        "[skills]\nprivate='../other-project'\n",
    )
    .unwrap();
    assert!(Settings::load_project(&db, &project, &repo).is_err());
}

#[test]
fn default_project_also_rejects_repository_credential_overrides() {
    let (temp, db, _) = fixture();
    let repo = temp.path().join("legacy");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(
        repo.join(".horde.toml"),
        "[providers.default]\napi_key_env='FOREIGN_KEY'\n",
    )
    .unwrap();
    assert!(Settings::load_project(&db, "default", &repo).is_err());
    std::fs::write(
        repo.join(".horde.toml"),
        "[automatic_delivery]\nenabled=true\ngh_program='/tmp/untrusted-gh'\n",
    )
    .unwrap();
    assert!(
        Settings::load_project(&db, "default", &repo)
            .unwrap_err()
            .to_string()
            .contains("automatic delivery")
    );
}

#[test]
fn default_project_reads_user_settings_from_the_stores_configuration_directory() {
    let (temp, db, _) = fixture();
    let config = temp.path().join("config");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("config.toml"), "concurrency=3\n").unwrap();
    assert_eq!(db.user_config_dir(), config);
    assert_eq!(
        Settings::load_project_user(&db, "default")
            .unwrap()
            .concurrency,
        3
    );
    let repo = temp.path().join("legacy");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join(".horde.toml"), "concurrency=2\n").unwrap();
    assert_eq!(
        Settings::load_project(&db, "default", &repo)
            .unwrap()
            .concurrency,
        2
    );
}

#[test]
fn project_skill_inspection_uses_its_own_configuration() {
    let (temp, db, project) = fixture();
    let repo = temp.path().join("repo");
    let skill = repo.join("custom");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "---\nname: project-private\ndescription: Project instructions.\n---\nOnly hamster knows this.\n").unwrap();
    let root = projects::storage_root(&db, &project).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("config.toml"),
        "[skills]\nproject-private='custom'\n",
    )
    .unwrap();
    projects::register_repository(&db, &project, &repo).unwrap();
    let result = horde::skill_policy::inspect(
        &db,
        &json!({"repo":repo,"project":project,"name":"project-private"}),
    )
    .unwrap();
    assert!(result["content"].as_str().unwrap().contains("Only hamster"));
}

#[test]
fn repository_cannot_enable_executor_network_access() {
    for project in [None, Some(())] {
        let (temp, db, created) = fixture();
        let project = project.map_or(projects::DEFAULT_PROJECT.to_owned(), |_| created);
        let config_dir = if project == projects::DEFAULT_PROJECT {
            db.user_config_dir()
        } else {
            projects::storage_root(&db, &project).unwrap()
        };
        std::fs::create_dir_all(&config_dir).unwrap();
        // The operator opens the network for the reviewer, through a provider
        // that no other role uses.
        std::fs::write(
            config_dir.join("config.toml"),
            "[providers.online]\nkind='codex'\nnetwork=true\n[executors.reviewer]\nprovider='online'\n",
        )
        .unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(repo.join(".horde")).unwrap();
        let approved = Settings::load_project(&db, &project, &repo).unwrap();
        assert!(approved.executor("reviewer").unwrap().network);
        assert!(!approved.executor("codex").unwrap().network);

        for (file, patch) in [
            (".horde.toml", "[executors.codex]\nnetwork=true\n"),
            (".horde/horde.toml", "[executors.codex]\nnetwork=true\n"),
            // Moving a role onto the operator's networked provider is also an expansion.
            (
                ".horde/horde.toml",
                "[executors.worker]\nprovider='online'\n",
            ),
        ] {
            let _ = std::fs::remove_file(repo.join(".horde.toml"));
            let _ = std::fs::remove_file(repo.join(".horde/horde.toml"));
            std::fs::write(repo.join(file), patch).unwrap();
            let error = Settings::load_project(&db, &project, &repo)
                .unwrap_err()
                .to_string();
            assert!(error.contains("cannot enable network access"), "{error}");
        }

        // Narrowing an operator grant, or restating it, is allowed.
        let _ = std::fs::remove_file(repo.join(".horde.toml"));
        std::fs::write(
            repo.join(".horde/horde.toml"),
            "[executors.reviewer]\nnetwork=false\n",
        )
        .unwrap();
        let narrowed = Settings::load_project(&db, &project, &repo).unwrap();
        assert!(!narrowed.executor("reviewer").unwrap().network);
        std::fs::write(
            repo.join(".horde/horde.toml"),
            "[executors.reviewer]\nnetwork=true\n",
        )
        .unwrap();
        assert!(
            Settings::load_project(&db, &project, &repo)
                .unwrap()
                .executor("reviewer")
                .unwrap()
                .network
        );
    }
}
