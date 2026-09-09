use std::{fs, path::Path};

use horde::repo_init::{Agent, install};

fn write(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn assert_installed_tree(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            assert_installed_tree(&entry.path(), &target);
        } else {
            assert!(
                target.is_file(),
                "missing bundled file {}",
                target.display()
            );
            assert!(
                !fs::symlink_metadata(&target)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(fs::read(entry.path()).unwrap(), fs::read(target).unwrap());
        }
    }
}

#[test]
fn both_agents_receive_every_canonical_horde_skill_file() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("skills");
    for (agent, destination) in [
        (Agent::Codex, ".agents/skills"),
        (Agent::Claude, ".claude/skills"),
    ] {
        let repo = tempfile::tempdir().unwrap();
        install(repo.path(), agent, None).unwrap();
        for entry in fs::read_dir(&source).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir()
                && entry.file_name().to_str().unwrap().starts_with("horde")
            {
                assert_installed_tree(
                    &entry.path(),
                    &repo.path().join(destination).join(entry.file_name()),
                );
            }
        }
    }
}

#[test]
fn codex_preserves_settings_and_instructions_and_reruns_cleanly() {
    let repo = tempfile::tempdir().unwrap();
    write(repo.path(), "AGENTS.md", "# Project\n\nKeep my rules.\n");
    write(
        repo.path(),
        ".codex/config.toml",
        "# My settings\nmodel = 'custom'\n[mcp_servers.other]\ncommand = 'other'\n",
    );
    install(repo.path(), Agent::Codex, None).unwrap();
    let instructions = fs::read_to_string(repo.path().join("AGENTS.md")).unwrap();
    assert!(instructions.starts_with("# Project\n\nKeep my rules.\n"));
    assert!(instructions.contains("HORDE_WORKER_TOKEN"));
    assert!(instructions.contains("small edits"));
    let config = fs::read_to_string(repo.path().join(".codex/config.toml")).unwrap();
    assert!(config.contains("# My settings"));
    let parsed: toml::Value = toml::from_str(&config).unwrap();
    assert_eq!(parsed["model"].as_str(), Some("custom"));
    assert_eq!(
        parsed["mcp_servers"]["other"]["command"].as_str(),
        Some("other")
    );
    assert_eq!(
        parsed["mcp_servers"]["horde"]["args"][0].as_str(),
        Some("mcp")
    );
    assert!(
        repo.path()
            .join(".agents/skills/horde/references/setup.md")
            .is_file()
    );
    assert!(
        repo.path()
            .join(".agents/skills/horde-worker/SKILL.md")
            .is_file()
    );
    assert!(
        repo.path()
            .join(".agents/skills/horde-templates/SKILL.md")
            .is_file()
    );
    install(repo.path(), Agent::Codex, None).unwrap();
    assert_eq!(
        fs::read_to_string(repo.path().join("AGENTS.md")).unwrap(),
        instructions
    );
    assert_eq!(
        fs::read_to_string(repo.path().join(".codex/config.toml")).unwrap(),
        config
    );
}

#[test]
fn claude_preserves_other_servers_and_uses_absolute_data_directory() {
    let repo = tempfile::tempdir().unwrap();
    write(
        repo.path(),
        ".mcp.json",
        r#"{"other":42,"mcpServers":{"other":{"command":"other"}}}"#,
    );
    install(repo.path(), Agent::Claude, Some(Path::new("state"))).unwrap();
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(repo.path().join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(config["other"], 42);
    assert_eq!(config["mcpServers"]["other"]["command"], "other");
    let args = config["mcpServers"]["horde"]["args"].as_array().unwrap();
    assert_eq!(args[0], "--data-dir");
    assert!(Path::new(args[1].as_str().unwrap()).is_absolute());
    assert_eq!(args[2], "mcp");
    assert!(
        repo.path()
            .join(".claude/skills/horde/references/setup.md")
            .is_file()
    );
    assert!(repo.path().join("CLAUDE.md").is_file());
}

#[test]
fn conflicts_are_detected_before_any_files_are_written() {
    for (path, content) in [
        ("AGENTS.md", "<!-- BEGIN HORDE DELEGATION -->\nunclosed"),
        (
            "AGENTS.md",
            "<!-- END HORDE DELEGATION -->\n<!-- BEGIN HORDE DELEGATION -->",
        ),
        (
            "AGENTS.md",
            "<!-- BEGIN HORDE DELEGATION --><!-- END HORDE DELEGATION --><!-- BEGIN HORDE DELEGATION --><!-- END HORDE DELEGATION -->",
        ),
        (
            ".codex/config.toml",
            "[mcp_servers.horde]\ncommand = 'custom'\nargs = []\n",
        ),
        (".codex/config.toml", "[broken"),
        (".agents/skills/horde/SKILL.md", "my custom skill"),
        (
            ".agents/skills/horde-planning/horde.toml",
            "my custom planning policy",
        ),
    ] {
        let repo = tempfile::tempdir().unwrap();
        write(repo.path(), path, content);
        assert!(install(repo.path(), Agent::Codex, None).is_err(), "{path}");
        assert_eq!(fs::read_to_string(repo.path().join(path)).unwrap(), content);
        assert!(
            !repo
                .path()
                .join(".agents/skills/horde-worker/SKILL.md")
                .exists()
        );
        assert!(!repo.path().join(".codex/.horde-init.json").exists());
    }
}

#[test]
fn local_skill_edits_are_preserved_on_rerun() {
    let repo = tempfile::tempdir().unwrap();
    install(repo.path(), Agent::Codex, None).unwrap();
    write(
        repo.path(),
        ".agents/skills/horde/references/setup.md",
        "local edits",
    );
    let before = fs::read(repo.path().join("AGENTS.md")).unwrap();
    assert!(install(repo.path(), Agent::Codex, None).is_err());
    assert_eq!(
        fs::read_to_string(repo.path().join(".agents/skills/horde/references/setup.md")).unwrap(),
        "local edits"
    );
    assert_eq!(fs::read(repo.path().join("AGENTS.md")).unwrap(), before);
}

#[test]
fn upgrades_replace_only_manifest_owned_skill_content_and_the_managed_block() {
    use sha2::{Digest, Sha256};
    let repo = tempfile::tempdir().unwrap();
    install(repo.path(), Agent::Codex, None).unwrap();
    let path = ".agents/skills/horde/SKILL.md";
    let bundled = fs::read(repo.path().join(path)).unwrap();
    write(repo.path(), path, "previous bundled version");
    let manifest_path = repo.path().join(".codex/.horde-init.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["files"][path] = hex::encode(Sha256::digest(b"previous bundled version")).into();
    fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    write(
        repo.path(),
        "AGENTS.md",
        "Before\n<!-- BEGIN HORDE DELEGATION -->\nOld rule\n<!-- END HORDE DELEGATION -->\nAfter\n",
    );
    install(repo.path(), Agent::Codex, None).unwrap();
    assert_eq!(fs::read(repo.path().join(path)).unwrap(), bundled);
    let instructions = fs::read_to_string(repo.path().join("AGENTS.md")).unwrap();
    assert!(instructions.starts_with("Before\n"));
    assert!(instructions.ends_with("\nAfter\n"));
    assert!(!instructions.contains("Old rule"));
    assert_eq!(
        instructions
            .matches("<!-- BEGIN HORDE DELEGATION -->")
            .count(),
        1
    );
}

#[test]
fn existing_identical_skills_can_be_adopted_without_a_manifest() {
    let repo = tempfile::tempdir().unwrap();
    write(
        repo.path(),
        ".agents/skills/horde/SKILL.md",
        include_str!("../skills/horde/SKILL.md"),
    );
    install(repo.path(), Agent::Codex, None).unwrap();
    assert!(repo.path().join(".codex/.horde-init.json").is_file());
}

#[test]
fn malformed_manifest_and_directory_destinations_are_rejected() {
    for content in ["{", r#"{"version":2,"files":{}}"#] {
        let repo = tempfile::tempdir().unwrap();
        write(repo.path(), ".codex/.horde-init.json", content);
        assert!(install(repo.path(), Agent::Codex, None).is_err());
        assert!(!repo.path().join("AGENTS.md").exists());
    }
    let repo = tempfile::tempdir().unwrap();
    fs::create_dir(repo.path().join("AGENTS.md")).unwrap();
    assert!(install(repo.path(), Agent::Codex, None).is_err());
    assert!(!repo.path().join(".codex").exists());
}

#[test]
fn malformed_claude_configuration_is_preserved() {
    for content in [
        "{",
        "[]",
        r#"{"mcpServers":[]}"#,
        r#"{"mcpServers":{"horde":{"command":"custom"}}}"#,
    ] {
        let repo = tempfile::tempdir().unwrap();
        write(repo.path(), ".mcp.json", content);
        assert!(install(repo.path(), Agent::Claude, None).is_err());
        assert_eq!(
            fs::read_to_string(repo.path().join(".mcp.json")).unwrap(),
            content
        );
        assert!(!repo.path().join("CLAUDE.md").exists());
    }
}

#[cfg(unix)]
#[test]
fn symlink_destinations_and_parents_are_rejected_without_partial_writes() {
    use std::os::unix::fs::symlink;
    for relative in [
        ".agents",
        ".agents/skills/horde/references",
        "AGENTS.md",
        ".codex/config.toml",
    ] {
        let repo = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = repo.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(outside.path(), &path).unwrap();
        assert!(
            install(repo.path(), Agent::Codex, None).is_err(),
            "{relative}"
        );
        assert!(
            !repo
                .path()
                .join(".agents/skills/horde-worker/SKILL.md")
                .exists()
        );
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
