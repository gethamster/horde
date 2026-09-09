//! Join workers without replacing an existing local runtime or its user configuration.
use crate::{
    daemon_client,
    fleet_enrollment::worker,
    management,
    store::{Store, hash, now},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

fn select_root(root: &Path, explicit: bool, controller: &str) -> Result<(PathBuf, bool)> {
    if explicit
        || [
            "fleet-worker.json",
            "fleet-worker-pending.json",
            "fleet-worker.key",
            "fleet-worker.csr",
        ]
        .iter()
        .any(|file| root.join(file).exists())
    {
        return Ok((root.into(), false));
    }
    // Selecting a worker must not open or migrate another runtime's database.
    let occupied = daemon_client::running(root)
        || root.join("network-runtime.toml").exists()
        || root.join("managed-network.toml").exists()
        || root.join("state.sqlite3").exists()
        || crate::branding::config_dir().join("network.toml").exists();
    if !occupied {
        return Ok((root.into(), false));
    }
    let selected = root
        .join("fleet-workers")
        .join(&hash(controller.as_bytes())[..24]);
    std::fs::create_dir_all(&selected)?;
    ensure!(
        selected.canonicalize()?.starts_with(root.canonicalize()?),
        "worker directory must remain inside its selected data directory"
    );
    Ok((selected, true))
}

fn connected(root: &Path, controller: &str, name: &str) -> Result<bool> {
    let db = Store::open(root)?;
    let at = management::value(&db, "controller_connected_at")?
        .and_then(|value| value.parse::<i64>().ok());
    let pid = management::value(&db, "controller_connected_pid")?
        .and_then(|value| value.parse::<u64>().ok());
    let peer = management::value(&db, "controller_connected_peer")?;
    if peer.as_deref() != Some(controller)
        || management::value(&db, "controller_connected_name")?.as_deref() != Some(name)
        || !at.is_some_and(|at| (now() - 10..=now() + 1).contains(&at))
    {
        return Ok(false);
    }
    let Ok(status) = daemon_client::request(root, "runtime_status", json!({})) else {
        return Ok(false);
    };
    if pid.is_none() || status["pid"].as_u64() != pid {
        return Ok(false);
    }
    let usable = management::value(&db, "controller_connected_name_usable")?;
    ensure!(
        usable.as_deref() != Some("false"),
        "worker is connected, but name {name} conflicts with another worker or an existing controller alias; choose a unique --name or have the agent update the controller alias"
    );
    Ok(usable.as_deref() == Some("true"))
}

pub async fn join(
    default_root: &Path,
    explicit_root: bool,
    invitation_path: &Path,
    name: Option<&str>,
    no_start: bool,
) -> Result<Value> {
    crate::fleet_enrollment::admin()?;
    if let Some(name) = name {
        crate::runtime_directory::validate_name(name)?;
    }
    let invitation = worker::read_invitation(invitation_path)?;
    let (root, isolated) = select_root(default_root, explicit_root, &invitation.controller_id)?;
    let existing = worker::validate_join(&root, invitation_path)?;
    let certificate = if daemon_client::running(&root) {
        let certificate = existing.context(
            "this data directory belongs to a running runtime; choose another --data-dir",
        )?;
        ensure!(
            certificate.expires > now(),
            "worker certificate expired; restore its enrollment credential and let the running daemon recover, then retry join"
        );
        certificate
    } else if isolated {
        worker::join_isolated(&root, invitation_path).await?
    } else {
        worker::join(&root, invitation_path).await?
    };
    let db = Store::open(&root)?;
    if let Some(name) = name {
        crate::runtime_directory::set_local_name(&db, name)?;
    }
    let name = crate::runtime_directory::local_name(&db)?;
    if no_start {
        return Ok(
            json!({"runtime":certificate.runtime_id,"name":name,"enrolled":true,"connected":connected(&root, &invitation.controller_id, &name)?,"data_dir":root}),
        );
    }
    daemon_client::start(&root).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if connected(&root, &invitation.controller_id, &name)? {
            return Ok(
                json!({"runtime":certificate.runtime_id,"name":name,"enrolled":true,"running":true,"connected":true,"data_dir":root}),
            );
        }
        if tokio::time::Instant::now() >= deadline || !daemon_client::running(&root) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!(
        "worker enrolled in {} but could not connect to controller {}; check controller reachability and {}, and update the controller for named-worker support. Retry the same join command",
        root.display(),
        invitation.controller_address,
        root.join("daemon.log").display()
    )
}
