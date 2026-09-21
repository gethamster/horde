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
fn guest_readiness_probe_rejects_every_missing_dependency() {
    use std::os::unix::fs::PermissionsExt;
    let config = lima::configuration(&profile(), "worker", "linux", "x86_64").unwrap();
    let probe = config["probes"][0]["script"]
        .as_str()
        .expect("guest readiness probe");
    let directory = tempfile::tempdir().unwrap();
    let bin = directory.path();
    let sudo = bin.join("sudo");
    std::fs::write(
        &sudo,
        r#"#!/bin/sh
set -eu
case "$*" in
  'docker compose version') component=compose ;;
  'systemctl is-active --quiet docker') component=service ;;
  'test -d /var/lib/horde') component=directory ;;
  'docker info') component=daemon ;;
  *) exit 99 ;;
esac
test "$FAIL" != "$component"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&sudo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let docker = bin.join("docker");
    std::fs::write(&docker, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
    for missing in ["", "compose", "service", "directory", "daemon", "cli"] {
        if missing == "cli" {
            std::fs::remove_file(&docker).unwrap();
        }
        let status = std::process::Command::new("/bin/sh")
            .args(["-c", probe])
            .env_clear()
            .env("PATH", bin)
            .env("FAIL", missing)
            .status()
            .unwrap();
        assert_eq!(status.success(), missing.is_empty(), "missing {missing}");
    }
}

#[test]
fn guest_package_commands_bound_retries_and_both_transport_timeouts() {
    use std::os::unix::fs::PermissionsExt;
    let config = lima::configuration(&profile(), "worker", "linux", "x86_64").unwrap();
    let directory = tempfile::tempdir().unwrap();
    let bin = directory.path();
    let log = bin.join("apt.log");
    let apt = bin.join("apt-get");
    std::fs::write(
        &apt,
        r#"#!/bin/sh
set -eu
test "$1" = '-o'; test "$2" = 'Acquire::Retries=3'; shift 2
test "$1" = '-o'; test "$2" = 'Acquire::http::Timeout=20'; shift 2
test "$1" = '-o'; test "$2" = 'Acquire::https::Timeout=20'; shift 2
test "$1" = '-o'; test "$2" = 'APT::Update::Error-Mode=any'; shift 2
printf '%s\n' "$*" >> "$LOG"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&apt, std::fs::Permissions::from_mode(0o755)).unwrap();
    for name in ["usermod", "systemctl", "install"] {
        let command = bin.join(name);
        std::fs::write(&command, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let result = std::process::Command::new("/bin/sh")
        .args(["-c", config["provision"][0]["script"].as_str().unwrap()])
        .env_clear()
        .env("PATH", bin)
        .env("LOG", &log)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let commands = std::fs::read_to_string(log).unwrap();
    assert_eq!(commands.lines().count(), 2);
    assert!(commands.starts_with("update\n"));
    assert!(commands.contains("install -y docker.io docker-compose-v2 ca-certificates git"));
}

#[test]
fn guest_packages_retry_failed_processes_and_stop_at_three_attempts() {
    use std::os::unix::fs::PermissionsExt;
    let config = lima::configuration(&profile(), "worker", "linux", "x86_64").unwrap();
    for command in ["update", "install"] {
        for mode in ["once", "always"] {
            let directory = tempfile::tempdir().unwrap();
            let bin = directory.path();
            let count = bin.join("count");
            let sleeps = bin.join("sleeps");
            let post = bin.join("post");
            for (name, script) in [
                (
                    "apt-get",
                    r#"#!/bin/sh
set -eu
test "$1" = '-o'; test "$2" = 'Acquire::Retries=3'; shift 2
test "$1" = '-o'; test "$2" = 'Acquire::http::Timeout=20'; shift 2
test "$1" = '-o'; test "$2" = 'Acquire::https::Timeout=20'; shift 2
test "$1" = '-o'; test "$2" = 'APT::Update::Error-Mode=any'; shift 2
if test "$1" = "$FAIL_COMMAND"; then
  count=0
  if test -f "$COUNT"; then read -r count < "$COUNT"; fi
  count=$((count + 1)); printf '%s\n' "$count" > "$COUNT"
  if test "$MODE" = always || test "$count" -eq 1; then exit 42; fi
fi
"#,
                ),
                (
                    "sleep",
                    "#!/bin/sh\ntest \"$1\" = 5 || exit 99\nprintf 'sleep\\n' >> \"$SLEEPS\"\n",
                ),
                ("usermod", "#!/bin/sh\nprintf 'ready\\n' > \"$POST\"\n"),
                ("systemctl", "#!/bin/sh\nexit 0\n"),
                ("install", "#!/bin/sh\nexit 0\n"),
            ] {
                let path = bin.join(name);
                std::fs::write(&path, script).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let result = std::process::Command::new("/bin/sh")
                .args(["-c", config["provision"][0]["script"].as_str().unwrap()])
                .env_clear()
                .env("PATH", bin)
                .env("FAIL_COMMAND", command)
                .env("MODE", mode)
                .env("COUNT", &count)
                .env("SLEEPS", &sleeps)
                .env("POST", &post)
                .output()
                .unwrap();
            assert_eq!(
                result.status.success(),
                mode == "once",
                "{command} {mode}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert_eq!(
                std::fs::read_to_string(count).unwrap().trim(),
                if mode == "once" { "2" } else { "3" }
            );
            assert_eq!(
                std::fs::read_to_string(sleeps).unwrap().lines().count(),
                if mode == "once" { 1 } else { 2 }
            );
            assert_eq!(
                post.exists(),
                mode == "once",
                "failed package installation must stop provisioning"
            );
        }
    }
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

#[test]
fn compact_guest_names_preserve_full_ownership_and_reject_collisions() {
    let temp = tempfile::tempdir().unwrap();
    let p = profile();
    let id = "550e8400-e29b-41d4-a716-446655440000";
    let name = lima::resource_name(&p, id).unwrap();
    assert_eq!(name.len(), 12);
    assert!(name.starts_with('h'));
    assert_ne!(name, lima::resource_name(&p, "other").unwrap());
    let owned = lima::prepare(temp.path(), &p, id, "linux", "x86_64").unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(owned.join("ownership.json")).unwrap()).unwrap();
    assert_eq!(manifest["runtime"], id);
    assert_eq!(manifest["resource"], name);
    assert_eq!(lima::owned_resource(temp.path(), &p, id).unwrap(), name);
    // A conflicting full identity must fail even if it occupies the same short-name claim.
    let claims = temp.path().join("lima-resource-claims");
    let scope = std::fs::read_dir(&claims)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let claim = scope.join(format!("{name}.json"));
    let mut conflicting: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&claim).unwrap()).unwrap();
    conflicting["runtime"] = json!("different-full-runtime-id");
    std::fs::write(&claim, conflicting.to_string()).unwrap();
    assert!(
        lima::prepare(temp.path(), &p, id, "linux", "x86_64")
            .unwrap_err()
            .to_string()
            .contains("collision")
    );
}

#[test]
fn socket_path_check_includes_canonical_home_and_ssh_temporary_suffix() {
    let home =
        std::path::Path::new("/private/var/lib/horde-lima/550e8400-e29b-41d4-a716-446655440000");
    lima::validate_socket_path(home, "h123456789ab", "macos").unwrap();
    assert!(
        lima::validate_socket_path(home, "horde-live-c6fa3a2c6079", "macos")
            .unwrap_err()
            .to_string()
            .contains("socket path")
    );
    let temp = tempfile::tempdir().unwrap();
    let long = temp.path().join("a".repeat(90));
    std::fs::create_dir(&long).unwrap();
    let alias = temp.path().join("short");
    std::os::unix::fs::symlink(&long, &alias).unwrap();
    assert!(lima::validate_socket_path(&alias, "h123456789ab", "macos").is_err());
}

#[test]
fn legacy_guest_intent_keeps_original_resource_name() {
    let temp = tempfile::tempdir().unwrap();
    let p = profile();
    let owned = temp.path().join("projects/hamster/lima/worker");
    std::fs::create_dir_all(&owned).unwrap();
    let old = json!({"project":p.project,"runtime":"worker","profile":p,"config":lima::configuration(&p,"worker","linux","x86_64").unwrap()});
    std::fs::write(owned.join("ownership.json"), old.to_string()).unwrap();
    assert_eq!(
        lima::owned_resource(temp.path(), &p, "worker").unwrap(),
        "horde-worker"
    );
    lima::prepare(temp.path(), &p, "worker", "linux", "x86_64").unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(owned.join("ownership.json")).unwrap()
        )
        .unwrap(),
        old
    );
}

#[test]
fn existing_guest_intent_retains_recorded_configuration_after_upgrade() {
    let temp = tempfile::tempdir().unwrap();
    let p = profile();
    let owned = lima::prepare(temp.path(), &p, "worker", "linux", "x86_64").unwrap();
    let path = owned.join("ownership.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["config"].as_object_mut().unwrap().remove("probes");
    manifest["config"]["provision"][0]["script"] = json!("#!/bin/sh\necho previous version\n");
    std::fs::write(&path, manifest.to_string()).unwrap();
    assert_eq!(
        lima::prepare(temp.path(), &p, "worker", "linux", "x86_64").unwrap(),
        owned
    );
    assert_eq!(
        lima::stored_configuration(&owned).unwrap(),
        manifest["config"]
    );
    let changed = Profile {
        cpus: p.cpus + 1,
        ..p
    };
    assert!(lima::prepare(temp.path(), &changed, "worker", "linux", "x86_64").is_err());
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
