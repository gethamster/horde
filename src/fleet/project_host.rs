//! Project-scoped host operations and physical capacity reservations.
use super::*;
/// Federation invokes this only after checking an authenticated management grant.
pub fn remote_command(db: &Store, peer: &str, args: &Value) -> Result<Option<Value>> {
    if args["action"] != "runtime_host_operation" {
        return Ok(None);
    }
    let project =
        crate::projects::resolve(db, args["project"].as_str().context("project required")?)?;
    ensure!(
        crate::projects::runtime_allowed(db, &project, peer)?,
        "caller is not granted to this project"
    );
    ensure!(
        crate::projects::runtime_allowed(db, &project, "local")?,
        "host is not granted to this project"
    );
    let action = args["operation"]["action"]
        .as_str()
        .context("host operation action required")?;
    ensure!(
        [
            "runtime_create",
            "runtime_start",
            "runtime_stop",
            "runtime_destroy",
            "runtime_reconcile"
        ]
        .contains(&action),
        "unsupported host lifecycle operation"
    );
    let mut input = args["operation"]["args"]
        .as_object()
        .context("host operation arguments required")?
        .clone();
    let request = input
        .get("request_id")
        .and_then(Value::as_str)
        .context("request_id required")?;
    let request = format!("{peer}:{request}");
    ensure!(request.len() <= 256, "host request ID too long");
    input.insert("request_id".into(), json!(request));
    input.insert("project".into(), json!(project));
    let input = Value::Object(input);
    let spec = if action == "runtime_create" {
        let profile = input["profile"].as_str().context("host profile required")?;
        load()?
            .profiles
            .get(profile)
            .cloned()
            .context("profile must be configured on host")?
    } else {
        let runtime = input["id"].as_str().context("host runtime ID required")?;
        let rows = db.rows("SELECT spec FROM managed_runtimes WHERE id=?", &[&runtime])?;
        serde_json::from_str(
            rows.first().context("host runtime missing")?["spec"]
                .as_str()
                .context("runtime spec")?,
        )?
    };
    ensure!(
        spec.host.is_none() && spec.provider == "lima",
        "host provisioning requires a local Lima profile"
    );
    ensure!(
        crate::projects::resolve(db, &spec.project)? == project,
        "host profile belongs to another project"
    );
    if action == "runtime_create" {
        let packet = args
            .get("bootstrap")
            .context("authenticated controller bootstrap required")?;
        let id = input["id"].as_str().context("runtime ID")?;
        let network: crate::network::NetworkConfig =
            serde_json::from_value(packet["network"].clone())?;
        ensure!(
            packet["id"] == id
                && network.runtime_id == id
                && network.controller_peer.as_deref() == Some(peer)
                && packet["project"]["id"] == project,
            "controller bootstrap identity mismatch"
        );
        persist_bootstrap(db, &project, id, packet)?;
    }
    dispatch(db, action, &input)
}

pub(super) async fn send_host_operation(
    db: &Store,
    op: &Value,
    profile: &Profile,
    host: &str,
) -> Result<Value> {
    ensure!(
        crate::projects::runtime_allowed(db, &profile.project, host)?,
        "host project grant revoked"
    );
    let args: Value = serde_json::from_str(op["args"].as_str().context("host operation args")?)?;
    let action = op["action"].as_str().context("host operation action")?;
    let config = crate::federation::config(db)?;
    let advertised = crate::federation::call(
        &config,
        host,
        "capabilities",
        json!({"project":profile.project}),
    )
    .await?;
    ensure!(
        advertised["features"]
            .as_array()
            .is_some_and(|features| features
                .iter()
                .any(|feature| feature == "lima_host_operations")),
        "host does not advertise project-aware Lima lifecycle support"
    );
    let mut payload = json!({"action":"runtime_host_operation","project":profile.project,"operation":{"action":action,"args":args}});
    if action == "runtime_create" {
        let id = op["runtime"].as_str().context("runtime ID")?;
        let path = bootstrap_path(db, &profile.project, id)?;
        let packet = if path.exists() {
            serde_json::from_slice(&std::fs::read(path)?)?
        } else {
            let packet = crate::enrollment::issue(db, id, profile)?
                .context("remote Lima requires controller enrollment")?;
            persist_bootstrap(db, &profile.project, id, &packet)?;
            packet
        };
        payload["bootstrap"] = packet;
    }
    let result = crate::federation::call(&config, host, "manage", payload).await;
    let result = match result {
        Ok(value) => value,
        Err(error) => {
            return Ok(json!({"host_managed":true,"state":"waiting","reason":error.to_string()}));
        }
    };
    if result["state"] == "succeeded" {
        let details: Value = result["result"]
            .as_str()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or(Value::Null);
        record_host_result(
            db,
            op["runtime"].as_str().context("runtime ID")?,
            action,
            &details,
        )?;
    }
    Ok(json!({"host_managed":true,"host":host,"state":result["state"],"receipt":result}))
}

pub(super) fn record_host_result(
    db: &Store,
    id: &str,
    action: &str,
    details: &Value,
) -> Result<()> {
    let guest = details["guest"]
        .as_array()
        .and_then(|guests| guests.first())
        .unwrap_or(&details["guest"]);
    let resource = details["resource"]
        .as_str()
        .or_else(|| guest["name"].as_str());
    if let Some(resource) = resource {
        let rows = db.rows(
            "SELECT spec,resource FROM managed_runtimes WHERE id=?",
            &[&id],
        )?;
        let row = rows.first().context("managed runtime missing")?;
        let profile: Profile = serde_json::from_str(row["spec"].as_str().context("runtime spec")?)?;
        ensure!(
            (resource == crate::lima::resource_name(&profile, id)?
                || resource == format!("horde-{id}"))
                && row["resource"]
                    .as_str()
                    .is_none_or(|previous| previous == resource),
            "remote guest resource ownership mismatch"
        );
    }
    let state = match action {
        "runtime_destroy" => "removed",
        "runtime_stop" => "stopped",
        "runtime_reconcile" => match details["state"].as_str() {
            Some("stopped") => "stopped",
            Some("provisioned") => "provisioned",
            _ => "uncertain",
        },
        _ => "provisioned",
    };
    db.atomic(|| {
        if action == "runtime_reconcile" {
            db.conn.execute("UPDATE managed_runtimes SET error=NULL WHERE id=?", [id])?;
        }
        if action == "runtime_destroy" {
            db.conn.execute("UPDATE runtime_enrollments SET state='revoked',token_hash='' WHERE runtime=?", [id])?;
        }
        if let Some(resource) = resource {
            db.conn.execute("UPDATE managed_runtimes SET resource=? WHERE id=?", params![resource,id])?;
        }
        db.conn.execute("UPDATE managed_runtimes SET state=CASE WHEN state='ready' AND ?='runtime_create' THEN state ELSE ? END WHERE id=?",params![action,state,id])?;
        Ok(())
    })
}

pub(super) fn runtime_visible(db: &Store, project: &str, runtime: &str) -> Result<bool> {
    if !crate::projects::runtime_allowed(db, project, runtime)? {
        return Ok(false);
    }
    if let Some(row) = db
        .rows("SELECT spec FROM managed_runtimes WHERE id=?", &[&runtime])?
        .first()
    {
        let profile: Profile = serde_json::from_str(row["spec"].as_str().context("runtime spec")?)?;
        return Ok(crate::projects::resolve(db, &profile.project)? == project);
    }
    Ok(true)
}

pub(super) fn bootstrap_path(db: &Store, project: &str, runtime: &str) -> Result<PathBuf> {
    crate::accounts::identifier(project)?;
    identifier(runtime)?;
    Ok(db
        .root
        .join("projects")
        .join(project)
        .join("runtime-bootstrap")
        .join(format!("{runtime}.json")))
}
pub(super) fn persist_bootstrap(
    db: &Store,
    project: &str,
    runtime: &str,
    packet: &Value,
) -> Result<()> {
    let path = bootstrap_path(db, project, runtime)?;
    std::fs::create_dir_all(path.parent().context("bootstrap directory")?)?;
    if path.exists() {
        ensure!(
            std::fs::symlink_metadata(&path)?.is_file(),
            "bootstrap must be a regular file"
        );
        let prior: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        ensure!(
            prior == *packet,
            "bootstrap retry changed immutable identity"
        );
    } else {
        crate::secrets::write_private(&path, packet.to_string().as_bytes())?;
    }
    Ok(())
}

/// Stopped and uncertain guests retain their reservation until explicit destroy.
pub fn reserved_local_cpus(db: &Store) -> Result<usize> {
    Ok(local_vm_reservations(db)?.0)
}
fn local_vm_reservations(db: &Store) -> Result<(usize, u64)> {
    let mut cpus = 0usize;
    let mut memory = 0u64;
    for row in db.rows(
        "SELECT spec FROM managed_runtimes WHERE state!='removed'",
        &[],
    )? {
        let profile: Profile = serde_json::from_str(row["spec"].as_str().context("runtime spec")?)?;
        if profile.provider == "lima" && profile.host.is_none() {
            cpus = cpus
                .checked_add(profile.cpus as usize)
                .context("Lima CPU reservations overflow")?;
            memory = memory
                .checked_add(u64::from(profile.memory_mb))
                .context("Lima memory reservations overflow")?;
        }
    }
    Ok((cpus, memory))
}
pub(super) fn admit_local_vm(db: &Store, profile: &Profile) -> Result<()> {
    let (cpus, memory) = local_vm_reservations(db)?;
    let physical_cpus = std::thread::available_parallelism()?.get();
    ensure!(
        cpus.checked_add(profile.cpus as usize)
            .is_some_and(|total| total <= physical_cpus),
        "insufficient unreserved host CPUs for Lima guest"
    );
    let physical_mb = if cfg!(target_os = "macos") {
        let output = std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", "hw.memsize"])
            .output()?;
        ensure!(
            output.status.success(),
            "cannot inspect host memory before reserving Lima capacity"
        );
        String::from_utf8(output.stdout)?.trim().parse::<u64>()? / 1024 / 1024
    } else {
        let text = std::fs::read_to_string("/proc/meminfo")?;
        text.lines()
            .find_map(|line| line.strip_prefix("MemTotal:"))
            .context("host memory unavailable")?
            .split_whitespace()
            .next()
            .context("host memory unavailable")?
            .parse::<u64>()?
            / 1024
    };
    ensure!(
        memory
            .checked_add(u64::from(profile.memory_mb))
            .is_some_and(|total| total <= physical_mb.saturating_sub(512)),
        "insufficient unreserved host memory for Lima guest (512 MiB retained for host)"
    );
    Ok(())
}
