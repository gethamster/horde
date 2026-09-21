use horde::{fleet::Profile, lima};
use serde_json::json;

fn profile() -> Profile {
    serde_json::from_value(json!({
        "provider":"lima", "project":"hamster", "image":"https://example.com/linux.img",
        "lima_image_digest":format!("sha256:{}", "a".repeat(64)),
        "lima_horde_binary":"/opt/horde/horde-linux", "lima_user":"horde-hamster", "lima_home":"/var/lib/horde-lima/hamster", "lima_egress":["203.0.113.9/32"]
    })).unwrap()
}

#[test]
#[ignore = "requires an installed limactl; validates real Lima schema without creating a VM"]
fn installed_lima_accepts_generated_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lima.yaml");
    let config = lima::configuration(
        &profile(),
        "schema-check",
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
    .unwrap();
    std::fs::write(&path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    let result = std::process::Command::new("limactl")
        .arg("validate")
        .arg(path)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn requires_project_bound_resources_and_host_enforcement() {
    let p = profile();
    p.validate().unwrap();
    for field in ["project", "lima_user", "lima_home", "lima_image_digest"] {
        let mut value = serde_json::to_value(&p).unwrap();
        value[field] = json!("");
        let invalid: Profile = serde_json::from_value(value).unwrap();
        assert!(invalid.validate().is_err(), "{field}");
    }
    let mut p = p;
    p.project = "../hamster".into();
    assert!(p.validate().is_err());
}

#[test]
fn guest_config_has_no_host_mounts_forwarding_or_docker_socket() {
    let config = lima::configuration(&profile(), "worker", "macos", "aarch64").unwrap();
    assert_eq!(config["vmType"], "vz");
    assert_eq!(config["mounts"], json!([]));
    assert_eq!(config["ssh"]["forwardAgent"], false);
    assert_eq!(config["portForwards"][0]["ignore"], true);
    assert_eq!(config["containerd"]["system"], false);
    assert_eq!(config["containerd"]["user"], false);
    assert_eq!(config["hostResolver"]["enabled"], false);
    assert_eq!(config["networks"][0]["lima"], "user-v2");
    assert!(!config.to_string().contains("docker.sock"));
    assert_eq!(
        lima::configuration(&profile(), "worker", "linux", "x86_64").unwrap()["vmType"],
        "qemu"
    );
    assert!(lima::configuration(&profile(), "worker", "windows", "x86_64").is_err());
}

#[test]
fn firewall_is_allowlist_only_and_rejects_injection() {
    let p = profile();
    let rules = lima::firewall_rules(&p, 501, "linux").unwrap();
    assert!(rules.contains("203.0.113.9/32"));
    assert!(rules.contains("drop"));
    assert!(rules.contains("skuid 501"));
    let pf = lima::firewall_rules(&p, 501, "macos").unwrap();
    assert!(pf.contains("block drop out quick"));
    let mut p = p;
    p.lima_user = "hamster; echo bad".into();
    assert!(lima::firewall_rules(&p, 501, "linux").is_err());
}

#[test]
fn ownership_is_immutable_and_projects_have_separate_instance_roots() {
    let temp = tempfile::tempdir().unwrap();
    let p = profile();
    let path = lima::prepare(temp.path(), &p, "worker", "linux", "x86_64").unwrap();
    assert_eq!(
        path,
        lima::prepare(temp.path(), &p, "worker", "linux", "x86_64").unwrap()
    );
    let mut changed = p.clone();
    changed.memory_mb += 1;
    assert!(lima::prepare(temp.path(), &changed, "worker", "linux", "x86_64").is_err());
    changed.project = "horde".into();
    assert_ne!(
        path,
        lima::prepare(temp.path(), &changed, "worker", "linux", "x86_64").unwrap()
    );
}

#[tokio::test]
async fn unowned_or_mismatched_guests_never_reach_host_commands() {
    let temp = tempfile::tempdir().unwrap();
    let db = horde::store::Store::open(temp.path()).unwrap();
    for (resource, expected) in [
        ("horde-someone-else", "ownership mismatch"),
        ("horde-worker", "no durable ownership"),
    ] {
        let error = lima::lifecycle(&db, &profile(), "worker", resource, "runtime_destroy")
            .await
            .unwrap_err();
        assert!(error.to_string().contains(expected));
    }
    assert!(
        lima::provision(&db, &profile(), "worker", None)
            .await
            .unwrap_err()
            .to_string()
            .contains("authenticated")
    );
}

#[test]
fn stopped_and_uncertain_vms_keep_host_cpu_reservations() {
    let temp = tempfile::tempdir().unwrap();
    let db = horde::store::Store::open(temp.path()).unwrap();
    for (id, state) in [
        ("stopped", "stopped"),
        ("uncertain", "requested"),
        ("destroyed", "removed"),
    ] {
        db.conn.execute("INSERT INTO managed_runtimes(id,profile,spec,state,created) VALUES(?,'test',?,?,0)", rusqlite::params![id, serde_json::to_string(&profile()).unwrap(),state]).unwrap();
    }
    assert_eq!(horde::fleet::reserved_local_cpus(&db).unwrap(), 4);
    db.conn
        .execute(
            "UPDATE managed_runtimes SET state='removed' WHERE id='uncertain'",
            [],
        )
        .unwrap();
    assert_eq!(horde::fleet::reserved_local_cpus(&db).unwrap(), 2);
}

#[test]
fn runtime_list_and_inspection_respect_project_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let db = horde::store::Store::open(temp.path()).unwrap();
    let project = horde::projects::dispatch(&db, "project_create", &json!({"slug":"hamster"}))
        .unwrap()
        .unwrap();
    let id = project["id"].as_str().unwrap();
    let p = Profile {
        project: id.into(),
        ..profile()
    };
    db.conn.execute("INSERT INTO managed_runtimes(id,profile,spec,state,created) VALUES('worker','test',?,'ready',0)",[serde_json::to_string(&p).unwrap()]).unwrap();
    db.conn
        .execute(
            "INSERT INTO project_runtime_grants VALUES(?,'worker',0)",
            [id],
        )
        .unwrap();
    assert_eq!(
        horde::fleet::dispatch(&db, "runtime_list", &json!({"project":"default"}))
            .unwrap()
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        horde::fleet::dispatch(&db, "runtime_list", &json!({"project":id}))
            .unwrap()
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        horde::fleet::dispatch(
            &db,
            "runtime_inspect",
            &json!({"project":"default","id":"worker"})
        )
        .is_err()
    );
    assert_eq!(
        horde::fleet::dispatch(
            &db,
            "runtime_list",
            &json!({"project":"default","all_projects":true})
        )
        .unwrap()
        .unwrap()
        .as_array()
        .unwrap()
        .len(),
        1
    );
}

#[test]
fn remote_host_management_requires_project_grants_before_profile_access() {
    let temp = tempfile::tempdir().unwrap();
    let db = horde::store::Store::open(temp.path()).unwrap();
    let project = horde::projects::dispatch(&db, "project_create", &json!({"slug":"hamster"}))
        .unwrap()
        .unwrap();
    let result = horde::fleet::remote_command(
        &db,
        "controller",
        &json!({"action":"runtime_host_operation","project":project["id"],"operation":{"action":"runtime_create","args":{"id":"worker","profile":"unavailable","request_id":"one"}}}),
    );
    assert!(result.unwrap_err().to_string().contains("not granted"));
    assert!(
        db.rows("SELECT * FROM runtime_operations", &[])
            .unwrap()
            .is_empty()
    );
}
