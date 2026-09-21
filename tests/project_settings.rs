use horde::{config::Settings, projects, store::Store};
use serde_json::json;

fn fixture() -> (tempfile::TempDir, Store, String) {
    let temp = tempfile::tempdir().unwrap();
    let db = Store::open(&temp.path().join("state")).unwrap();
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
