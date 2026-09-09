use horde::{
    config::Settings,
    executor::Invocation,
    skills,
    store::Store,
    template::{self, Step},
};
use serde_json::json;
use std::{collections::BTreeMap, path::Path};
fn repository(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@localhost"],
        vec!["commit", "--allow-empty", "-m", "initial"],
    ] {
        horde::git::run(path, &args).unwrap();
    }
}
fn skill(path: &Path, version: &str) {
    std::fs::create_dir_all(path.join("references")).unwrap();
    std::fs::create_dir_all(path.join("scripts")).unwrap();
    std::fs::write(path.join("SKILL.md"), format!("---\nname: writer\ndescription: Write reports\n---\nUse version {version}. Read references/rules.md. Run scripts/check.sh only when allowed.\n")).unwrap();
    std::fs::write(path.join("references/rules.md"), "Use short sentences.").unwrap();
    std::fs::write(
        path.join("scripts/check.sh"),
        "#!/bin/sh\nprintf 'skill-check-ok\\n'\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        path.join("scripts/check.sh"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
}
fn plan() -> template::Plan {
    template::compile(
        "simulated",
        &template::load_templates(Path::new("absent")).unwrap(),
        BTreeMap::from([("task".into(), "write".into())]),
    )
    .unwrap()
}
#[test]
fn selected_skill_is_discovered_once_and_progressive_reads_preserve_its_pin() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    repository(&repo);
    let source = dir.path().join("source-skill");
    skill(&source, "one");
    let settings = Settings {
        skills: BTreeMap::from([("writer".into(), source.clone())]),
        ..Default::default()
    };
    let root = dir.path().join("data");
    let db = Store::open(&root).unwrap();
    let mut plan = plan();
    plan.steps[0].skills = vec!["writer".into()];
    let task = db.submit("write", &repo, &settings, &plan).unwrap();
    let first = skills::catalog(&db, &task).unwrap();
    let row = db.steps(&task).unwrap()[0].clone();
    let step: Step = Store::step(&row).unwrap();
    let worker = db.register(&task, row["id"].as_str()).unwrap();
    let invocation = Invocation {
        db: &db,
        task: &task,
        step: row["id"].as_str().unwrap(),
        attempt: "attempt",
        worker: worker["id"].as_str().unwrap(),
        token: worker["token"].as_str().unwrap(),
        workspace: &repo,
        spec: &step,
        settings: &settings,
        context: json!({}),
    };
    let prompt = invocation.prompt().unwrap();
    let original_body = std::fs::read_to_string(source.join("SKILL.md")).unwrap();
    let original_hash = skills::packet(&db, &task).unwrap()["writer"].hash.clone();
    assert!(!prompt.contains(&original_body));
    assert!(!prompt.contains("Use version one"));
    assert!(prompt.contains("Selected skill writer"));
    assert!(prompt.contains(&original_hash));
    assert_eq!(invocation.prompt().unwrap(), prompt);
    assert_eq!(
        db.rows(
            "SELECT * FROM events WHERE task=? AND kind='skill.selected'",
            &[&task]
        )
        .unwrap()
        .len(),
        1
    );
    assert!(
        db.rows(
            "SELECT * FROM events WHERE task=? AND kind='skill.read'",
            &[&task]
        )
        .unwrap()
        .is_empty()
    );
    let instructions = horde::protocol::dispatch(
        &db,
        "read_skill",
        json!({"name":"writer"}),
        worker["token"].as_str(),
    )
    .unwrap();
    assert_eq!(instructions["content"], original_body);
    assert_eq!(instructions["hash"], original_hash);
    assert!(prompt.contains(instructions["base_directory"].as_str().unwrap()));
    let resource = horde::protocol::dispatch(
        &db,
        "read_skill",
        json!({"name":"writer", "path":"references/rules.md"}),
        worker["token"].as_str(),
    )
    .unwrap();
    assert_eq!(resource["content"], "Use short sentences.");
    let script = Path::new(resource["base_directory"].as_str().unwrap()).join("scripts/check.sh");
    let output = std::process::Command::new(script).output().unwrap();
    assert_eq!(output.stdout, b"skill-check-ok\n");
    assert!(
        horde::git::run(&repo, &["status", "--porcelain"])
            .unwrap()
            .is_empty()
    );
    skill(&source, "two");
    let reopened = Store::open(&root).unwrap();
    assert_eq!(skills::catalog(&reopened, &task).unwrap(), first);
    let retry = skills::prompt(&reopened, &task, "retry", &step).unwrap();
    assert!(!retry.contains("Use version one"));
    assert!(!retry.contains("Use version two"));
    assert!(retry.contains(&original_hash));
    let pinned = skills::read(&reopened, &task, &json!({"name":"writer"})).unwrap();
    assert_eq!(pinned["content"], original_body);
    assert_eq!(pinned["hash"], original_hash);
    let new_task = reopened
        .submit("new version", &repo, &settings, &plan)
        .unwrap();
    assert_ne!(
        skills::packet(&reopened, &new_task).unwrap()["writer"].hash,
        skills::packet(&reopened, &task).unwrap()["writer"].hash
    );
    let fresh = skills::read(&reopened, &new_task, &json!({"name":"writer"})).unwrap();
    assert_eq!(
        fresh["content"],
        std::fs::read_to_string(source.join("SKILL.md")).unwrap()
    );
    assert_ne!(fresh["hash"], original_hash);
    std::fs::remove_dir_all(source).unwrap();
    let child = horde::delegation::delegate(
        &reopened,
        &task,
        &json!({"id":"child", "objective":"continue", "template":"simulated", "skills":["writer"]}),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let writer_catalog = json!(
        first
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["name"] == "writer")
            .collect::<Vec<_>>()
    );
    assert_eq!(skills::catalog(&reopened, &child).unwrap(), writer_catalog);
    let grandchild = horde::delegation::delegate(
        &reopened,
        &child,
        &json!({"id":"grandchild", "objective":"continue", "template":"simulated"}),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        skills::catalog(&reopened, &grandchild).unwrap(),
        writer_catalog
    );
    for descendant in [&child, &grandchild] {
        let read = skills::read(&reopened, descendant, &json!({"name":"writer"})).unwrap();
        assert_eq!(read["content"], original_body);
        assert_eq!(read["hash"], original_hash);
    }
    let narrowed = horde::delegation::delegate(
        &reopened,
        &task,
        &json!({"id":"none", "objective":"continue", "template":"simulated", "skills":[]}),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(skills::catalog(&reopened, &narrowed).unwrap(), json!([]));
    let agent_child = horde::delegation::delegate(&reopened, &task, &json!({"id":"agent-child", "objective":"write", "template":"local-implementation", "skills":["writer"]})).unwrap()["id"].as_str().unwrap().to_owned();
    let agent_steps: Vec<_> = reopened
        .steps(&agent_child)
        .unwrap()
        .iter()
        .map(Store::step)
        .collect::<anyhow::Result<Vec<_>>>()
        .unwrap()
        .into_iter()
        .filter(|s| s.kind == "agent")
        .collect();
    assert!(!agent_steps.is_empty());
    for step in agent_steps {
        assert_eq!(step.skills, vec!["writer"]);
        let prompt = skills::prompt(
            &reopened,
            &agent_child,
            &format!("attempt-{}", step.id),
            &step,
        )
        .unwrap();
        assert!(!prompt.contains("Use version one"));
        assert!(prompt.contains(&original_hash));
        let read = skills::read(&reopened, &agent_child, &json!({"name":"writer"})).unwrap();
        assert_eq!(read["content"], original_body);
        assert_eq!(read["hash"], original_hash);
    }
    let before = reopened.steps(&task).unwrap();
    assert!(
        horde::protocol::dispatch(
            &reopened,
            "add_steps",
            json!({"task":task,"steps":[{"id":"bad-skill","skills":["unavailable"]}]}),
            None
        )
        .is_err()
    );
    assert_eq!(reopened.steps(&task).unwrap(), before);

    assert!(horde::delegation::delegate(&reopened, &narrowed, &json!({"id":"escape", "objective":"continue", "template":"simulated", "skills":["writer"]})).is_err());
}
#[test]
fn corrupt_missing_and_escaping_skill_bundles_fail_before_task_submission() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("skill");
    skill(&source, "one");
    let configured = BTreeMap::from([("writer".into(), source.clone())]);
    let packet = skills::capture(dir.path(), &configured).unwrap();
    let mut corrupt = packet.clone();
    corrupt
        .get_mut("writer")
        .unwrap()
        .files
        .get_mut("SKILL.md")
        .unwrap()
        .hex = hex::encode("changed");
    assert!(
        skills::validate(&corrupt)
            .unwrap_err()
            .to_string()
            .contains("hash")
    );
    let mut escaping = packet;
    let bundle = escaping.get_mut("writer").unwrap();
    bundle.files.insert(
        "../escape".into(),
        skills::File {
            hex: hex::encode("outside"),
            executable: false,
        },
    );
    bundle.hash = horde::store::hash(&serde_json::to_vec(&bundle.files).unwrap());
    assert!(skills::validate(&escaping).is_err());
    std::os::unix::fs::symlink("/etc/passwd", source.join("link")).unwrap();
    assert!(skills::capture(dir.path(), &configured).is_err());
    let db = Store::open(&dir.path().join("db")).unwrap();
    let settings = Settings {
        skills: configured,
        ..Default::default()
    };
    assert!(db.submit("bad", dir.path(), &settings, &plan()).is_err());
    assert!(db.rows("SELECT id FROM tasks", &[]).unwrap().is_empty());
    let mut unknown = plan();
    unknown.steps[0].skills = vec!["missing".into()];
    assert!(
        db.submit("missing", dir.path(), &Settings::default(), &unknown)
            .unwrap_err()
            .to_string()
            .contains("steps[0].skills")
    );
}
#[test]
fn resources_are_paged_and_materialized_edits_are_detected() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("skill");
    skill(&source, "one");
    let settings = Settings {
        skills: BTreeMap::from([("writer".into(), source)]),
        ..Default::default()
    };
    let db = Store::open(&dir.path().join("db")).unwrap();
    let task = db.submit("write", dir.path(), &settings, &plan()).unwrap();
    let page = skills::read(
        &db,
        &task,
        &json!({"name":"writer", "path":"references/rules.md", "limit":4}),
    )
    .unwrap();
    assert_eq!(page["content"], "Use ");
    assert_eq!(page["next_offset"], 4);
    assert!(skills::read(&db, &task, &json!({"name":"writer", "path":"../outside"})).is_err());
    let path = Path::new(page["base_directory"].as_str().unwrap()).join("SKILL.md");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&path, "changed").unwrap();
    assert!(
        skills::read(&db, &task, &json!({"name":"writer"}))
            .unwrap_err()
            .to_string()
            .contains("materialized skill changed")
    );
}
