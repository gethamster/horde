use horde::{
    config::Settings,
    skills,
    store::Store,
    template::{self, Step},
};
use serde_json::json;
use std::{collections::BTreeMap, path::Path};

fn fixture() -> (tempfile::TempDir, Store, String) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@localhost"],
        vec!["commit", "--allow-empty", "-m", "initial"],
    ] {
        horde::git::run(&repo, &args).unwrap();
    }
    let db = Store::open(&dir.path().join("data")).unwrap();
    let plan = template::compile(
        "simulated",
        &template::load_templates(Path::new("absent")).unwrap(),
        BTreeMap::from([("task".into(), "work".into())]),
    )
    .unwrap();
    let task = db
        .submit("work", &repo, &Settings::default(), &plan)
        .unwrap();
    (dir, db, task)
}

fn assert_loaded(db: &Store, task: &str, role: &str, attempt: &str, explicit: Vec<String>) {
    let spec: Step =
        serde_json::from_value(json!({"id":"work","kind":"agent","role":role,"skills":explicit}))
            .unwrap();
    let packet = skills::packet(db, task).unwrap();
    let bundle = &packet["horde-model-selection"];
    let body = String::from_utf8(hex::decode(&bundle.files["SKILL.md"].hex).unwrap()).unwrap();
    let prompt = skills::prompt(db, task, attempt, &spec).unwrap();
    assert_eq!(
        prompt.matches(&body).count(),
        1,
        "full pinned instructions must appear once"
    );
    assert_eq!(skills::prompt(db, task, attempt, &spec).unwrap(), prompt);
    let loaded = db.rows("SELECT hash FROM attempt_skills WHERE task=? AND attempt=? AND name='horde-model-selection'", &[&task,&attempt]).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0]["hash"], bundle.hash);
}

#[test]
fn model_guidance_is_injected_into_each_agent_role_and_retry_without_step_configuration() {
    let (_dir, db, task) = fixture();
    assert!(
        skills::packet(&db, &task)
            .unwrap()
            .contains_key("horde-model-selection")
    );
    for role in ["planner", "worker", "reviewer", "custom-role"] {
        assert_loaded(&db, &task, role, role, vec![]);
        assert_loaded(
            &db,
            &task,
            role,
            &format!("{role}-retry"),
            vec!["horde-model-selection".into()],
        );
    }
}

#[test]
fn descendants_keep_pinned_model_guidance_and_explicit_catalog_narrowing_is_respected() {
    let (dir, db, task) = fixture();
    let child = horde::delegation::delegate(
        &db,
        &task,
        &json!({"id":"child","objective":"child","template":"simulated"}),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let grandchild = horde::delegation::delegate(
        &db,
        &child,
        &json!({"id":"grandchild","objective":"grandchild","template":"simulated"}),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let original = skills::packet(&db, &task).unwrap();
    let proposed = horde::skill_policy::propose(&db,&json!({"repo":dir.path().join("repo"),"name":"horde-model-selection","content":"Use this newly accepted project model policy."})).unwrap();
    horde::skill_policy::apply(&db,&json!({"repo":dir.path().join("repo"),"proposal_id":proposed["proposal_id"],"accepted":true})).unwrap();
    let reopened = Store::open(&db.root).unwrap();
    for id in [&task, &child, &grandchild] {
        assert_eq!(
            skills::packet(&reopened, id).unwrap()["horde-model-selection"].hash,
            original["horde-model-selection"].hash
        );
        assert_loaded(&reopened, id, "worker", "after-policy-edit", vec![]);
    }
    let narrowed = horde::delegation::delegate(
        &db,
        &task,
        &json!({"id":"narrowed","objective":"narrowed","template":"simulated","skills":[]}),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let step: Step =
        serde_json::from_value(json!({"id":"work","kind":"agent","role":"planner"})).unwrap();
    assert!(
        skills::prompt(&db, &narrowed, "no-injection", &step)
            .unwrap()
            .is_empty()
    );
    assert!(
        db.rows("SELECT * FROM attempt_skills WHERE task=?", &[&narrowed])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn automatic_guidance_does_not_apply_to_command_steps() {
    let (_dir, db, task) = fixture();
    let step: Step =
        serde_json::from_value(json!({"id":"work","kind":"command","role":"planner"})).unwrap();
    skills::prompt(&db, &task, "command", &step).unwrap();
    assert!(
        db.rows("SELECT * FROM attempt_skills WHERE task=?", &[&task])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn directory_skills_can_be_added_edited_removed_and_installed_without_rebuilding() {
    use horde::skill_catalog;
    let (dir, db, old_task) = fixture();
    let source = dir.path().join("pack");
    let skill = source.join("new-guidance");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "First file policy").unwrap();
    std::fs::write(skill.join("horde.toml"), "[injection]\nagent=true\n").unwrap();
    let first = skill_catalog::load_from(&source).unwrap();
    let installed = skill_catalog::install(&db.root, &first).unwrap();
    assert_eq!(
        skill_catalog::summary(&skill_catalog::load_for(&db.root).unwrap()).unwrap(),
        installed
    );
    let plan = template::compile(
        "simulated",
        &template::load_templates(Path::new("absent")).unwrap(),
        BTreeMap::from([("task".into(), "work".into())]),
    )
    .unwrap();
    let task = db
        .submit(
            "new work",
            &dir.path().join("repo"),
            &Settings::default(),
            &plan,
        )
        .unwrap();
    let step: Step =
        serde_json::from_value(json!({"id":"work","kind":"agent","role":"invented-role"})).unwrap();
    assert!(
        skills::prompt(&db, &task, "first", &step)
            .unwrap()
            .contains("First file policy")
    );
    std::fs::write(skill.join("SKILL.md"), "Second file policy").unwrap();
    let second = skill_catalog::load_from(&source).unwrap();
    assert_ne!(first["new-guidance"].hash, second["new-guidance"].hash);
    skill_catalog::install(&db.root, &second).unwrap();
    assert!(
        skills::prompt(&db, &task, "retry", &step)
            .unwrap()
            .contains("First file policy")
    );
    assert!(
        skills::packet(&db, &old_task)
            .unwrap()
            .contains_key("horde-model-selection")
    );
    std::fs::remove_dir_all(&skill).unwrap();
    let empty = skill_catalog::load_from(&source).unwrap();
    assert!(empty.is_empty());
    skill_catalog::install(&db.root, &empty).unwrap();
    assert!(skill_catalog::load_for(&db.root).unwrap().is_empty());
}

#[test]
fn injection_metadata_is_pinned_generic_and_preserves_explicit_order() {
    let (dir, db, _) = fixture();
    let source = dir.path().join("pack");
    for (name, metadata) in [
        ("z-explicit", ""),
        ("a-explicit", ""),
        ("role-guidance", "[injection]\nroles=['invented-role']\n"),
        (
            "only-empty",
            "[injection]\nagent=true\nwhen_no_explicit_skills=true\n",
        ),
    ] {
        let path = source.join(name);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("SKILL.md"), format!("Body for {name}")).unwrap();
        std::fs::write(path.join("horde.toml"), metadata).unwrap();
    }
    let packet = horde::skill_catalog::load_from(&source).unwrap();
    horde::skill_catalog::install(&db.root, &packet).unwrap();
    let plan = template::compile(
        "simulated",
        &template::load_templates(Path::new("absent")).unwrap(),
        BTreeMap::from([("task".into(), "work".into())]),
    )
    .unwrap();
    let task = db
        .submit(
            "work",
            &dir.path().join("repo"),
            &Settings::default(),
            &plan,
        )
        .unwrap();
    let step:Step=serde_json::from_value(json!({"id":"work","kind":"agent","role":"invented-role","skills":["z-explicit","a-explicit","z-explicit"]})).unwrap();
    let prompt = skills::prompt(&db, &task, "custom", &step).unwrap();
    assert!(
        prompt.find("Body for z-explicit").unwrap() < prompt.find("Body for a-explicit").unwrap()
    );
    assert_eq!(prompt.matches("Body for z-explicit").count(), 1);
    assert!(prompt.contains("Body for role-guidance"));
    assert!(!prompt.contains("Body for only-empty"));
    std::fs::write(
        source.join("role-guidance/horde.toml"),
        "[injection]\nunknown=true\n",
    )
    .unwrap();
    assert!(horde::skill_catalog::load_from(&source).is_err());
}

#[test]
fn invalid_catalogs_and_corrupt_installed_packs_fail_without_fallback() {
    use horde::skill_catalog;
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("pack");
    std::fs::create_dir(&source).unwrap();
    let skill = source.join("a");
    std::fs::create_dir(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "Instructions").unwrap();
    let packet = skill_catalog::load_from(&source).unwrap();
    let root = dir.path().join("runtime");
    std::fs::create_dir(&root).unwrap();
    let installed = skill_catalog::install(&root, &packet).unwrap();
    assert_eq!(skill_catalog::install(&root, &packet).unwrap(), installed);
    let version = root
        .join("skill-packs/versions")
        .join(installed["hash"].as_str().unwrap());
    let stored = version.join("a/SKILL.md");
    std::fs::remove_file(&stored).unwrap();
    std::fs::write(stored, "Tampered").unwrap();
    assert!(skill_catalog::load_for(&root).is_err());
    assert!(skill_catalog::install(&root, &packet).is_err());
    std::fs::remove_file(root.join("skill-packs/CURRENT")).unwrap();
    std::fs::write(root.join("skill-packs/CURRENT"), "../../outside").unwrap();
    assert!(skill_catalog::load_for(&root).is_err());
    std::os::unix::fs::symlink(&skill, source.join("alias")).unwrap();
    assert!(skill_catalog::load_from(&source).is_err());
    assert!(skill_catalog::load_from(&source.join("alias")).is_err());
}

#[test]
fn automatic_metadata_obeys_the_combined_instruction_budget() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..5 {
        let skill = dir.path().join(format!("guidance-{index}"));
        std::fs::create_dir(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "x".repeat(65536)).unwrap();
        std::fs::write(skill.join("horde.toml"), "[injection]\nagent=true\n").unwrap();
    }
    let packet = horde::skill_catalog::load_from(dir.path()).unwrap();
    let step: Step =
        serde_json::from_value(json!({"id":"work","kind":"agent","role":"worker"})).unwrap();
    assert!(
        skills::validate_steps(&packet, &[step])
            .unwrap_err()
            .to_string()
            .contains("256 KiB")
    );
}
