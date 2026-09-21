use horde::execution_selection;
use serde_json::{Value, json};

fn inventory() -> Value {
    json!({"project":"hamster","runtimes":[{"runtime":"linux-worker","local":false,"fresh":true,"ready":true,"platform":{"os":"linux","arch":"aarch64","docker":true,"isolation":"lima"},"protocol":{"features":["projects","execution_selection"]},"capacity":{"available":2},"capabilities":[{"id":"test","provider":"fake","kind":"simulated","model":null,"configuration_hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}]})
}
fn selection() -> Value {
    json!({"selected":{"runtime":"linux-worker","capability":"test"},"requirements":{"os":"macos","isolation":"lima"}})
}
#[test]
fn linux_guest_cannot_satisfy_macos_task() {
    assert!(execution_selection::prepare_inventory(&inventory(), &selection(), None).is_err());
}
#[test]
fn project_selection_rejects_peer_without_project_protocol() {
    let mut runtimes = inventory();
    runtimes["runtimes"][0]["protocol"]["features"] = json!(["execution_selection"]);
    let input = json!({"selected":{"runtime":"linux-worker","capability":"test"}});
    assert!(execution_selection::prepare_inventory(&runtimes, &input, None).is_err());
}
#[test]
fn project_and_requirements_are_immutable_selection_context() {
    let mut input = selection();
    input["requirements"]["os"] = json!("linux");
    let policy = execution_selection::prepare_inventory(&inventory(), &input, None).unwrap();
    assert_eq!(policy["project"], "hamster");
    assert_eq!(policy["requirements"], input["requirements"]);
    let mut other = inventory();
    other["project"] = json!("horde");
    assert!(execution_selection::prepare_inventory(&other, &input, Some(&policy)).is_err());
    input["requirements"] = json!({"isolation":"native"});
    assert!(execution_selection::prepare_inventory(&inventory(), &input, Some(&policy)).is_err());
}

#[test]
fn remote_accept_rejects_an_ungranted_project_before_creating_repository() {
    let dir = tempfile::tempdir().unwrap();
    let db = horde::store::Store::open(dir.path()).unwrap();
    let project = horde::projects::dispatch(&db, "project_create", &json!({"slug":"hamster"}))
        .unwrap()
        .unwrap();
    let network = horde::network::NetworkConfig {
        runtime_id: "worker".into(),
        execution_clients: vec!["controller".into()],
        ..Default::default()
    };
    let packet = json!({"method":"accept","args":{"project":project["id"],"task":"owner-task"}});
    let error =
        horde::federation::handle_control(dir.path().into(), network, "controller", &packet)
            .unwrap_err();
    assert!(error.to_string().contains("not granted"), "{error}");
    assert!(db.rows("SELECT id FROM tasks", &[]).unwrap().is_empty());
    assert!(!dir.path().join("projects").exists());
}

#[test]
fn project_inventory_hides_ungranted_runtimes_and_ambient_capabilities() {
    let dir = tempfile::tempdir().unwrap();
    let db = horde::store::Store::open(dir.path()).unwrap();
    let project = horde::projects::dispatch(&db, "project_create", &json!({"slug":"hamster"}))
        .unwrap()
        .unwrap();
    let id = project["id"].as_str().unwrap();
    let inventory = horde::capabilities::inventory_project(&db, id).unwrap();
    assert_eq!(inventory["project"], id);
    assert_eq!(inventory["runtimes"], json!([]));
    horde::projects::dispatch(
        &db,
        "project_runtime_grant",
        &json!({"project":id,"runtime":"local"}),
    )
    .unwrap();
    let inventory = horde::capabilities::inventory_project(&db, id).unwrap();
    assert_eq!(inventory["runtimes"].as_array().unwrap().len(), 1);
    assert_eq!(
        inventory["runtimes"][0]["platform"]["os"],
        std::env::consts::OS
    );
}

#[test]
fn remote_task_ids_cannot_cross_project_authorization() {
    let dir = tempfile::tempdir().unwrap();
    let db = horde::store::Store::open(dir.path()).unwrap();
    db.conn
        .execute(
            "INSERT INTO tasks VALUES('victim','work','repo','running','{}','{}',0)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO task_projects VALUES('victim','default',NULL)",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO remote_origins VALUES('victim','controller','owner')",
            [],
        )
        .unwrap();
    let project = horde::projects::dispatch(&db, "project_create", &json!({"slug":"hamster"}))
        .unwrap()
        .unwrap();
    let network = horde::network::NetworkConfig::default();
    let error = horde::federation::handle_control(
        dir.path().into(),
        network,
        "controller",
        &json!({"method":"status","args":{"project":project["id"],"task":"victim"}}),
    )
    .unwrap_err();
    assert!(error.to_string().contains("another project"), "{error}");
}

#[test]
fn automatic_placement_enforces_isolation_capacity_and_workflow_stickiness() {
    let dir = tempfile::tempdir().unwrap();
    let db = horde::store::Store::open(&dir.path().join("data")).unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@example.test"],
        vec!["commit", "--allow-empty", "-m", "init"],
    ] {
        horde::git::run(&repo, &args).unwrap();
    }
    let project = horde::projects::dispatch(
        &db,
        "project_create",
        &json!({"slug":"hamster","isolation":"vm"}),
    )
    .unwrap()
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    horde::projects::dispatch(
        &db,
        "project_runtime_grant",
        &json!({"project":project,"runtime":"guest"}),
    )
    .unwrap();
    let network = horde::network::NetworkConfig {
        runtime_id: "controller".into(),
        delegate_peers: vec!["guest".into()],
        ..Default::default()
    };
    horde::federation::configure(&db.root, &network).unwrap();
    db.conn.execute("INSERT INTO managed_runtimes(id,profile,spec,state,created) VALUES('guest','test','{}','ready',0)",[]).unwrap();
    let mut report = horde::capabilities::local_project(&db, &project).unwrap();
    report["runtime"] = json!("guest");
    report["local"] = json!(false);
    report["platform"]["os"] = json!("linux");
    report["platform"]["isolation"] = json!("lima");
    horde::capabilities::observe(&db, "guest", &report).unwrap();
    horde::capabilities::observe_project(&db, "guest", &project, &report).unwrap();
    let settings = horde::config::Settings::load_project_user(&db, &project).unwrap();
    let plan = horde::template::Plan {
        warnings: vec![],
        steps: vec![],
        pins: Default::default(),
        outputs: Default::default(),
    };
    let task = db
        .submit_project(&project, "work", &repo, &settings, &plan)
        .unwrap();
    assert!(horde::federation::route_queued(&db, &task).unwrap());
    assert_eq!(db.task(&task).unwrap()["status"], "remote");
    assert_eq!(
        db.rows("SELECT peer,state FROM remote_links WHERE task=?", &[&task])
            .unwrap()[0],
        json!({"peer":"guest","state":"pending"})
    );
    assert!(db.rows("SELECT id FROM attempts", &[]).unwrap().is_empty());

    // A native guest can never satisfy this VM project's fallback path.
    report["platform"]["isolation"] = json!("native");
    horde::capabilities::observe(&db, "guest", &report).unwrap();
    horde::capabilities::observe_project(&db, "guest", &project, &report).unwrap();
    let held = db
        .submit_project(&project, "needs VM", &repo, &settings, &plan)
        .unwrap();
    assert!(horde::federation::route_queued(&db, &held).unwrap());
    assert!(
        db.rows("SELECT task FROM remote_links WHERE task=?", &[&held])
            .unwrap()
            .is_empty()
    );

    // Native projects spread untouched work when local capacity is exhausted.
    horde::projects::dispatch(
        &db,
        "project_update",
        &json!({"project":project,"isolation":"native"}),
    )
    .unwrap();
    horde::projects::dispatch(
        &db,
        "project_runtime_grant",
        &json!({"project":project,"runtime":"local"}),
    )
    .unwrap();
    horde::management::set(&db, "concurrency", "1").unwrap();
    let busy = db
        .submit_project(&project, "already running", &repo, &settings, &plan)
        .unwrap();
    db.conn.execute("INSERT INTO steps(id,task,name,spec,state) VALUES('busy-step',?,'busy','{}','running')",[&busy]).unwrap();
    db.conn.execute("INSERT INTO attempts(id,step,state,started) VALUES('busy-attempt','busy-step','running',0)",[]).unwrap();
    let waiting = db
        .submit_project(&project, "new work", &repo, &settings, &plan)
        .unwrap();
    assert!(horde::federation::route_queued(&db, &waiting).unwrap());
    assert_eq!(
        db.rows("SELECT peer FROM remote_links WHERE task=?", &[&waiting])
            .unwrap()[0]["peer"],
        "guest"
    );
    assert!(!horde::federation::route_queued(&db, &busy).unwrap());
    db.conn
        .execute(
            "UPDATE attempts SET state='uncertain' WHERE id='busy-attempt'",
            [],
        )
        .unwrap();
    assert!(!horde::federation::route_queued(&db, &busy).unwrap());
    assert!(
        db.rows("SELECT task FROM remote_links WHERE task=?", &[&busy])
            .unwrap()
            .is_empty()
    );
}
