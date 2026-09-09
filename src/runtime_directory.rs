//! Human names are labels; authenticated runtime IDs remain the source of authority.
use crate::{
    management,
    store::{Store, now},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

pub fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 63
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !name.starts_with('-')
            && !name.ends_with('-'),
        "runtime name must be 1–63 lowercase letters, digits, or interior hyphens"
    );
    ensure!(
        !["local", "all", "auto"].contains(&name)
            && !["ts-", "fleet-"]
                .iter()
                .any(|prefix| name.starts_with(prefix)),
        "runtime name is reserved for identity or selection"
    );
    Ok(())
}

fn ids(db: &Store) -> Result<Vec<String>> {
    db.rows("SELECT id FROM managed_runtimes UNION SELECT runtime FROM runtime_enrollments UNION SELECT runtime FROM fleet_enrollment_members UNION SELECT runtime FROM runtime_presence", &[])?.into_iter().map(|row| row["id"].as_str().map(str::to_owned).context("runtime identity missing")).collect()
}

fn known(db: &Store, id: &str) -> Result<()> {
    ensure!(
        ids(db)?.iter().any(|candidate| candidate == id),
        "unknown runtime {id}; run horde runtime list"
    );
    Ok(())
}

fn usable(db: &Store, id: &str) -> Result<()> {
    let inactive: bool = db.conn.query_row("SELECT EXISTS(SELECT 1 FROM runtime_enrollments WHERE runtime=?1 AND state='revoked' UNION ALL SELECT 1 FROM managed_runtimes WHERE id=?1 AND state='removed' UNION ALL SELECT 1 FROM fleet_enrollment_members WHERE runtime=?1 AND state='revoked')",[id],|r| r.get(0))?;
    ensure!(
        !inactive,
        "runtime {id} is revoked or removed; enroll it again before submitting work"
    );
    Ok(())
}

pub fn display_name(db: &Store, id: &str) -> Result<Option<String>> {
    if let Some(name) = management::value(db, &format!("runtime_alias:{id}"))? {
        return Ok(Some(name));
    }
    management::value(db, &format!("runtime_reported_name:{id}"))
}

/// Called only for the identity authenticated by the incoming control stream.
pub fn observe_name(db: &Store, id: &str, name: &str) -> Result<()> {
    validate_name(name)?;
    management::set(db, &format!("runtime_reported_name:{id}"), name)
}

/// Resolve a stable identity first, then an unambiguous controller or worker name.
pub fn resolve(db: &Store, selector: &str) -> Result<String> {
    let id = resolve_known(db, selector)?;
    usable(db, &id)?;
    Ok(id)
}

/// Administrative inspection and cleanup may still select revoked identities.
pub fn resolve_known(db: &Store, selector: &str) -> Result<String> {
    let known = ids(db)?;
    if known.iter().any(|id| id == selector) {
        return Ok(selector.into());
    }
    let aliases = known
        .iter()
        .filter_map(
            |id| match management::value(db, &format!("runtime_alias:{id}")) {
                Ok(Some(name)) if name == selector => Some(Ok(id.clone())),
                Err(error) => Some(Err(error)),
                _ => None,
            },
        )
        .collect::<Result<Vec<_>>>()?;
    let matches = if aliases.is_empty() {
        known
            .iter()
            .filter_map(|id| match display_name(db, id) {
                Ok(Some(name)) if name == selector => Some(Ok(id.clone())),
                Err(error) => Some(Err(error)),
                _ => None,
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        aliases
    };
    ensure!(
        !matches.is_empty(),
        "unknown runtime {selector}; run horde runtime list"
    );
    ensure!(
        matches.len() == 1,
        "runtime name {selector} is ambiguous; use an exact ID from horde runtime list"
    );
    Ok(matches[0].clone())
}

pub fn rename(db: &Store, id: &str, name: &str) -> Result<Value> {
    validate_name(name)?;
    db.atomic(|| {
        known(db, id)?;
        for other in ids(db)? {
            ensure!(
                other == id
                    || (other != name && display_name(db, &other)?.as_deref() != Some(name)),
                "runtime name {name} is already in use; choose a unique name"
            );
        }
        management::set(db, &format!("runtime_alias:{id}"), name)?;
        management::event(db, "runtime.renamed", json!({"id":id,"name":name}))?;
        Ok(json!({"id":id,"name":name}))
    })
}

pub fn set_local_name(db: &Store, name: &str) -> Result<()> {
    validate_name(name)?;
    management::set(db, "runtime_name", name)
}

pub fn local_name(db: &Store) -> Result<String> {
    if let Some(name) = management::value(db, "runtime_name")? {
        validate_name(&name)?;
        return Ok(name);
    }
    let mut hostname = [0u8; 256];
    // gethostname writes at most the supplied buffer length; no pointer escapes.
    ensure!(
        unsafe { libc::gethostname(hostname.as_mut_ptr().cast(), hostname.len()) } == 0,
        "cannot read local hostname"
    );
    let end = hostname
        .iter()
        .position(|b| *b == 0)
        .unwrap_or(hostname.len());
    let host = String::from_utf8_lossy(&hostname[..end]);
    let label = host.split('.').next().unwrap_or("").to_ascii_lowercase();
    let label: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .take(63)
        .collect();
    let label = label.trim_matches('-');
    let name = if validate_name(label).is_ok() {
        label
    } else {
        "worker"
    };
    set_local_name(db, name)?;
    Ok(name.into())
}

/// Forget only local admission and listing state. Provider resources are never touched.
pub fn forget(db: &Store, id: &str) -> Result<Value> {
    db.atomic(|| {
        known(db,id)?;
        let busy: bool = db.conn.query_row("SELECT EXISTS(SELECT 1 FROM runtime_operations WHERE runtime=?1 AND state NOT IN ('succeeded','failed','cancelled','done') UNION ALL SELECT 1 FROM remote_links WHERE peer=?1 AND state!='done' UNION ALL SELECT 1 FROM runtime_presence WHERE runtime=?1 AND observed>?2)", rusqlite::params![id,now()-30],|r|r.get(0))?;
        ensure!(!busy,"runtime has active work, recent presence, or unfinished operations; stop or revoke it and reconcile operations before removing it");
        let config_path=db.root.join("network-runtime.toml");
        if config_path.exists() {
            let network=crate::network::NetworkConfig::load(Some(&config_path))?;
            ensure!(!crate::control::connected(&network,id)?,"runtime is connected; stop or revoke it before removing it");
        }
        db.conn.execute("UPDATE runtime_enrollments SET state='revoked',token_hash='' WHERE runtime=?",[id])?;
        db.conn.execute("UPDATE fleet_enrollment_members SET state='revoked' WHERE runtime=?",[id])?;
        db.conn.execute("UPDATE managed_runtimes SET state='removed' WHERE id=?",[id])?;
        management::set(db,&format!("runtime_removed:{id}"),"true")?;
        management::event(db,"runtime.removed",json!({"id":id,"name":display_name(db,id)?,"provider_resources_changed":false}))?;
        db.conn.execute("DELETE FROM runtime_settings WHERE key IN (?1,?2)",rusqlite::params![format!("runtime_alias:{id}"),format!("runtime_reported_name:{id}")])?;
        Ok(json!({"id":id,"removed":true,"provider_resources_changed":false}))
    })
}
