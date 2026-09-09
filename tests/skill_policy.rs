use horde::{config::Settings, skill_policy as policy, skills, store::Store, template};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

fn fixture() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("repo")).unwrap();
    let db = Store::open(&dir.path().join("data")).unwrap();
    (dir, db)
}
fn inspect(db: &Store, repo: &Path) -> Value {
    policy::inspect(db, &json!({"repo":repo,"name":"horde-planning"})).unwrap()
}
fn proposal(db: &Store, repo: &Path, content: &str) -> Value {
    policy::propose(db,&json!({"repo":repo,"name":"horde-planning","content":content,"reason":"Prefer independent testable work for this project"})).unwrap()
}
fn apply(db: &Store, repo: &Path, p: &Value) -> Value {
    policy::apply(
        db,
        &json!({"repo":repo,"proposal_id":p["proposal_id"],"accepted":true}),
    )
    .unwrap()
}
fn content(text: &str) -> String {
    format!("---\nname: horde-planning\ndescription: Plan this project's work.\n---\n{text}\n")
}

#[test]
fn shipped_skills_are_discoverable_without_configuration() {
    let (dir, db) = fixture();
    let repo = dir.path().join("repo");
    let catalog = policy::inspect(&db, &json!({"repo":repo})).unwrap();
    assert_eq!(
        catalog["skills"].as_array().unwrap().len(),
        skills::builtins().unwrap().len()
    );
    for name in [
        "horde-setup",
        "horde-discovery",
        "horde-planning",
        "horde-delegation",
        "horde-review",
        "horde-model-selection",
    ] {
        let skill = policy::inspect(&db, &json!({"repo":repo,"name":name})).unwrap();
        assert_eq!(skill["source"], "builtin");
        assert_eq!(skill["revision"], 0);
        assert_eq!(skill["baseline_hash"], skill["effective_hash"]);
        assert!(!skill["content"].as_str().unwrap().is_empty());
    }
    assert_eq!(std::fs::read_dir(repo).unwrap().count(), 0);
}

#[test]
fn proposals_require_acceptance_and_compare_effective_revision_before_apply() {
    let (dir, db) = fixture();
    let repo = dir.path().join("repo");
    let before = inspect(&db, &repo);
    let first = proposal(
        &db,
        &repo,
        &content("Use the approved local thinking pool."),
    );
    let stale = proposal(
        &db,
        &repo,
        &content("Use the approved remote delivery pool."),
    );
    assert_eq!(inspect(&db, &repo), before);
    assert!(
        first["diff"]
            .as_str()
            .unwrap()
            .contains("+Use the approved local thinking pool.")
    );
    assert!(
        policy::apply(
            &db,
            &json!({"repo":repo,"proposal_id":first["proposal_id"]})
        )
        .is_err()
    );
    apply(&db, &repo, &first);
    let effective = inspect(&db, &repo);
    assert_eq!(effective["revision"], 1);
    assert_ne!(effective["effective_hash"], before["effective_hash"]);
    assert_eq!(effective["baseline_hash"], before["baseline_hash"]);
    assert!(
        policy::apply(
            &db,
            &json!({"repo":repo,"proposal_id":stale["proposal_id"],"accepted":true})
        )
        .is_err()
    );
    let repeat = apply(&db, &repo, &first);
    assert_eq!(repeat["revision"], 1);
    let history = policy::history(&db, &json!({"repo":repo,"name":"horde-planning"})).unwrap();
    assert_eq!(history["revisions"].as_array().unwrap().len(), 1);
}

#[test]
fn rollback_is_reviewed_and_project_overrides_do_not_cross_project_boundaries() {
    let (dir, db) = fixture();
    let repo = dir.path().join("repo");
    let other = dir.path().join("other");
    std::fs::create_dir(&other).unwrap();
    let baseline = inspect(&db, &repo);
    let p = proposal(&db, &repo, &content("Project-specific review order."));
    assert!(
        policy::apply(
            &db,
            &json!({"repo":other,"proposal_id":p["proposal_id"],"accepted":true})
        )
        .is_err()
    );
    apply(&db, &repo, &p);
    assert_eq!(
        inspect(&db, &other)["effective_hash"],
        baseline["effective_hash"]
    );
    let rollback = policy::rollback(
        &db,
        &json!({"repo":repo,"name":"horde-planning","revision":0}),
    )
    .unwrap();
    assert_ne!(
        inspect(&db, &repo)["effective_hash"],
        baseline["effective_hash"]
    );
    apply(&db, &repo, &rollback);
    assert_eq!(
        inspect(&db, &repo)["effective_hash"],
        baseline["effective_hash"]
    );
    assert_eq!(inspect(&db, &repo)["revision"], 2);
    let restore = policy::rollback(
        &db,
        &json!({"repo":repo,"name":"horde-planning","revision":1}),
    )
    .unwrap();
    apply(&db, &repo, &restore);
    assert_eq!(
        inspect(&db, &repo)["content"],
        content("Project-specific review order.")
    );
    let reopened = Store::open(&db.root).unwrap();
    assert_eq!(inspect(&reopened, &repo), inspect(&db, &repo));
    assert_eq!(std::fs::read_dir(repo).unwrap().count(), 0);
}

#[test]
fn submitted_tasks_and_children_keep_effective_skill_pins_after_override_changes() {
    let (dir, db) = fixture();
    let repo = dir.path().join("repo");
    for args in [
        &["init", "-b", "main"][..],
        &["config", "user.name", "Test"],
        &["config", "user.email", "test@localhost"],
        &["commit", "--allow-empty", "-m", "initial"],
    ] {
        horde::git::run(&repo, args).unwrap();
    }
    let plan = template::compile(
        "simulated",
        &template::load_templates(Path::new("absent")).unwrap(),
        BTreeMap::from([("task".into(), "work".into())]),
    )
    .unwrap();
    let old = db
        .submit("before", &repo, &Settings::default(), &plan)
        .unwrap();
    let pinned = skills::packet(&db, &old).unwrap();
    assert!(pinned.contains_key("horde-planning"));
    let change = proposal(&db, &repo, &content("New project planning guidance."));
    apply(&db, &repo, &change);
    let new = db
        .submit("after", &repo, &Settings::default(), &plan)
        .unwrap();
    let next = skills::packet(&db, &new).unwrap();
    assert_ne!(pinned["horde-planning"].hash, next["horde-planning"].hash);
    assert_eq!(
        skills::packet(&db, &old).unwrap()["horde-planning"].hash,
        pinned["horde-planning"].hash
    );
    let child=horde::delegation::delegate(&db,&old,&json!({"id":"child","objective":"continue","template":"simulated","skills":["horde-planning"]})).unwrap()["id"].as_str().unwrap().to_owned();
    assert_eq!(
        skills::packet(&db, &child).unwrap()["horde-planning"].hash,
        pinned["horde-planning"].hash
    );
}

#[test]
fn malformed_skill_edits_and_unknown_names_never_create_overrides() {
    let (dir, db) = fixture();
    let repo = dir.path().join("repo");
    for (name, text) in [
        ("../escape", content("Wrong path")),
        ("missing", content("Missing baseline")),
        ("horde-planning", String::new()),
        ("horde-planning", "x".repeat(65537)),
    ] {
        assert!(policy::propose(&db, &json!({"repo":repo,"name":name,"content":text})).is_err());
    }
    assert!(policy::propose(&db,&json!({"repo":repo,"name":"horde-planning","content":content("Reviewed"),"expected_hash":"stale"})).is_err());
    assert_eq!(inspect(&db, &repo)["revision"], 0);
}

#[test]
fn configured_resources_are_reviewed_and_pinned_with_an_accepted_override() {
    let (dir, db) = fixture();
    let repo = dir.path().join("repo");
    let source = repo.join("custom");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(
        source.join("SKILL.md"),
        "Read reference.txt for project checks.",
    )
    .unwrap();
    std::fs::write(source.join("reference.txt"), "first reference").unwrap();
    std::fs::write(repo.join(".horde.toml"), "[skills]\ncustom = 'custom'\n").unwrap();
    let args = json!({"repo":repo,"name":"custom","content":"Use the project checks and report failures."});
    let stale = policy::propose(&db, &args).unwrap();
    std::fs::write(source.join("reference.txt"), "second reference").unwrap();
    assert!(
        policy::apply(
            &db,
            &json!({"repo":repo,"proposal_id":stale["proposal_id"],"accepted":true})
        )
        .is_err()
    );
    let accepted = policy::propose(&db, &args).unwrap();
    apply(&db, &repo, &accepted);
    std::fs::write(source.join("reference.txt"), "third reference").unwrap();
    let settings = Settings::load(&repo).unwrap();
    let pinned = skills::capture_effective(&db, &repo, &settings.skills).unwrap();
    assert_eq!(
        hex::decode(&pinned["custom"].files["reference.txt"].hex).unwrap(),
        b"second reference"
    );
    let reset = policy::rollback(&db, &json!({"repo":repo,"name":"custom","revision":0})).unwrap();
    assert!(
        reset["changed_files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["path"] == "reference.txt")
    );
    apply(&db, &repo, &reset);
    let restored = skills::capture_effective(&db, &repo, &settings.skills).unwrap();
    assert_eq!(
        hex::decode(&restored["custom"].files["reference.txt"].hex).unwrap(),
        b"third reference"
    );
}

#[test]
fn skill_policy_rejects_symlinked_scope_and_configured_bundles() {
    let (dir, db) = fixture();
    let repo = dir.path().join("repo");
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&repo, &alias).unwrap();
    assert!(policy::inspect(&db, &json!({"repo":alias})).is_err());
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("SKILL.md"), "Instructions").unwrap();
    std::os::unix::fs::symlink(&real, repo.join("source")).unwrap();
    std::fs::write(repo.join(".horde.toml"), "[skills]\ncustom='source'\n").unwrap();
    assert!(policy::inspect(&db, &json!({"repo":repo,"name":"custom"})).is_err());
    assert!(policy::inspect(&db, &json!({"repo":repo,"scope":"global"})).is_err());
}

#[test]
fn skill_history_pages_through_applied_revisions() {
    let (dir, db) = fixture();
    let repo = dir.path().join("repo");
    for text in ["one", "two", "three"] {
        let p = proposal(&db, &repo, &content(text));
        apply(&db, &repo, &p);
    }
    let first =
        policy::history(&db, &json!({"repo":repo,"name":"horde-planning","limit":2})).unwrap();
    assert_eq!(first["revisions"].as_array().unwrap().len(), 2);
    assert_eq!(first["next_before_revision"], 2);
    let rest = policy::history(
        &db,
        &json!({"repo":repo,"name":"horde-planning","limit":2,"before_revision":2}),
    )
    .unwrap();
    assert_eq!(rest["revisions"].as_array().unwrap().len(), 1);
    assert_eq!(rest["revisions"][0]["revision"], 1);
    assert!(rest["next_before_revision"].is_null());
}

#[test]
fn planner_invocation_discovers_its_pinned_planning_skill_without_loading_it() {
    let (dir, db) = fixture();
    let repo = dir.path().join("repo");
    let guidance =
        content("For this project, split delivery at independently testable boundaries.");
    let p = proposal(&db, &repo, &guidance);
    apply(&db, &repo, &p);
    let settings = Settings::default();
    let plan = template::compile(
        "local-implementation",
        &template::load_templates(Path::new("absent")).unwrap(),
        BTreeMap::from([("task".into(), "plan work".into())]),
    )
    .unwrap();
    let task = db.submit("plan work", &repo, &settings, &plan).unwrap();
    let row = db
        .steps(&task)
        .unwrap()
        .into_iter()
        .find(|row| Store::step(row).unwrap().role == "planner")
        .unwrap();
    let step: template::Step = Store::step(&row).unwrap();
    assert!(step.skills.is_empty());
    let worker = db.register(&task, row["id"].as_str()).unwrap();
    let invocation = horde::executor::Invocation {
        db: &db,
        task: &task,
        step: row["id"].as_str().unwrap(),
        attempt: "planning-attempt",
        worker: worker["id"].as_str().unwrap(),
        token: worker["token"].as_str().unwrap(),
        workspace: &repo,
        spec: &step,
        settings: &settings,
        context: json!({}),
    };
    let initial = invocation.prompt().unwrap();
    assert!(!initial.contains(&guidance));
    assert!(initial.contains("Selected skill horde-planning"));
    let pinned_hash = skills::packet(&db, &task).unwrap()["horde-planning"]
        .hash
        .clone();
    assert!(initial.contains(&pinned_hash));
    let read = skills::read(&db, &task, &json!({"name":"horde-planning"})).unwrap();
    assert_eq!(read["content"], guidance);
    assert_eq!(read["hash"], pinned_hash);
    assert!(initial.contains(read["base_directory"].as_str().unwrap()));
    let next = proposal(&db, &repo, &content("Future project guidance."));
    apply(&db, &repo, &next);
    assert_eq!(invocation.prompt().unwrap(), initial);
    let after_edit = skills::read(&db, &task, &json!({"name":"horde-planning"})).unwrap();
    assert_eq!(after_edit["content"], guidance);
    assert_eq!(after_edit["hash"], pinned_hash);
    let selected = db
        .rows(
            "SELECT name,hash FROM attempt_skills WHERE task=? AND attempt='planning-attempt'",
            &[&task],
        )
        .unwrap();
    assert_eq!(selected.len(), 2);
    let planning = selected
        .iter()
        .find(|row| row["name"] == "horde-planning")
        .unwrap();
    assert_eq!(
        planning["hash"],
        skills::packet(&db, &task).unwrap()["horde-planning"].hash
    );
}

#[test]
fn shipped_skills_preserve_the_existing_capacity_for_32_configured_skills() {
    let (dir, db) = fixture();
    let repo = dir.path().join("repo");
    let configured: BTreeMap<_, _> = (0..32)
        .map(|index| {
            let name = format!("custom-{index}");
            let path = repo.join(&name);
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("SKILL.md"), "Project guidance.").unwrap();
            (name, path)
        })
        .collect();
    let packet = skills::capture_effective(&db, &repo, &configured).unwrap();
    assert_eq!(packet.len(), 32 + skills::builtins().unwrap().len());
    skills::validate(&packet).unwrap();
    let extra = configured
        .into_iter()
        .chain([("one-too-many".into(), repo.join("custom-0"))])
        .collect();
    assert!(skills::capture_effective(&db, &repo, &extra).is_err());
}
