use horde::{protocol, store::Store};
use serde_json::json;

#[test]
fn coding_agent_can_discover_and_plan_without_task_or_project_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let inventory = protocol::dispatch(&db, "runtime_capabilities", json!({}), None).unwrap();
    assert!(
        inventory["runtimes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["local"] == true)
    );
    let skills =
        protocol::dispatch(&db, "skill_inspect", json!({"repo":dir.path()}), None).unwrap();
    assert!(skills.to_string().contains("horde-planning"));
    for name in [
        "runtime_capabilities",
        "plan_execution",
        "agent_setup",
        "skill_inspect",
        "skill_propose",
        "skill_apply",
        "skill_history",
        "skill_rollback",
    ] {
        assert!(
            protocol::OPERATIONS.iter().any(|op| op.0 == name),
            "missing agent tool {name}"
        );
    }
}

#[test]
fn internal_workers_get_discovery_but_cannot_change_setup_or_saved_skills() {
    assert!(protocol::worker_allowed("runtime_capabilities"));
    assert!(protocol::worker_allowed("plan_execution"));
    for name in [
        "agent_setup",
        "skill_propose",
        "skill_apply",
        "skill_rollback",
    ] {
        assert!(!protocol::worker_allowed(name));
    }
    let schema = protocol::admin_schema("submit_task");
    assert_eq!(schema["properties"]["execution"]["type"], "object");
    assert_eq!(
        protocol::schema("delegate_task")["properties"]["execution"]["type"],
        "object"
    );
}

#[test]
fn lost_submission_reply_reuses_receipt_even_after_repository_disappears() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let db = Store::open(&dir.path().join("data")).unwrap();
    let input = json!({"request_id":"delivery-1","repo":repo,"objective":"reviewable result","template":"simulated"});
    let first = protocol::dispatch(&db, "submit_task", input.clone(), None).unwrap();
    std::fs::remove_dir_all(&repo).unwrap();
    let retry = protocol::dispatch(&db, "submit_task", input.clone(), None).unwrap();
    assert_eq!(first, retry);
    assert_eq!(db.rows("SELECT id FROM tasks", &[]).unwrap().len(), 1);
    let mut changed = input;
    changed["objective"] = json!("different work");
    assert!(
        protocol::dispatch(&db, "submit_task", changed, None)
            .unwrap_err()
            .to_string()
            .contains("different assignment")
    );
}

#[test]
fn execution_policy_database_cannot_be_opened_by_schema_three_runtimes() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let schema: i64 = db
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert!(
        schema > 3,
        "older daemons must reject rather than ignore execution policies"
    );
    drop(db);
    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(
        reopened
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        schema
    );
}

#[test]
fn caller_installs_file_skills_and_sees_the_exact_catalog_version() {
    let dir = tempfile::tempdir().unwrap();
    let pack = dir.path().join("pack");
    std::fs::create_dir_all(pack.join("delivery")).unwrap();
    std::fs::write(pack.join("delivery/SKILL.md"), "Check the delivery result.").unwrap();
    let db = Store::open(&dir.path().join("data")).unwrap();
    let installed =
        protocol::dispatch(&db, "skill_pack_install", json!({"path":pack}), None).unwrap();
    assert_eq!(installed["skills"], json!(["delivery"]));
    let listed = protocol::dispatch(&db, "skill_pack_list", json!({}), None).unwrap();
    assert_eq!(installed, listed);
    let inventory = protocol::dispatch(&db, "runtime_capabilities", json!({}), None).unwrap();
    assert_eq!(inventory["runtimes"][0]["skill_pack"], installed);
    let mut observed = inventory["runtimes"][0].clone();
    observed["skill_pack_error"] = serde_json::Value::Null;
    horde::capabilities::observe(&db, observed["runtime"].as_str().unwrap(), &observed).unwrap();
    for name in [
        "skill_pack_install",
        "runtime_skills_update",
        "runtime_update",
    ] {
        assert!(!protocol::worker_allowed(name));
        assert!(
            protocol::dispatch(&db, name, json!({}), Some("worker-token"))
                .unwrap_err()
                .to_string()
                .contains("unavailable to worker")
        );
    }
    for name in [
        "skill_pack_install",
        "skill_pack_list",
        "runtime_skills_update",
    ] {
        assert!(protocol::OPERATIONS.iter().any(|op| op.0 == name));
    }
}
