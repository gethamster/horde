//! Safe execution inventory. Configuration and filesystem evidence never prove provider login.
use crate::{
    config::{ExecutorConfig, Settings},
    management,
    store::{Store, now},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    os::unix::fs::PermissionsExt,
    path::Path,
};

const MAX_INVENTORY: usize = 64 * 1024;
const FRESH_SECONDS: i64 = 30;
const PREFIX: &str = "runtime_capabilities:";

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Evidence {
    Available,
    Missing,
    Unknown,
    NotRequired,
    CredentialPresent,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Availability {
    executable: Evidence,
    authentication: Evidence,
    verified: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Capability {
    id: String,
    configuration_hash: String,
    executor: String,
    provider: String,
    model: Option<String>,
    kind: String,
    available: Option<bool>,
    availability: Availability,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Capacity {
    concurrency: usize,
    active: usize,
    available: usize,
    draining: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Protocol {
    version: u32,
    features: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeInventory {
    runtime: String,
    name: Option<String>,
    local: bool,
    fresh: bool,
    ready: bool,
    observed_at: i64,
    version: String,
    protocol: Protocol,
    capacity: Capacity,
    capabilities: Vec<Capability>,
}

fn label(value: &str, maximum: usize) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= maximum && !value.chars().any(char::is_control),
        "invalid capability label"
    );
    Ok(())
}
fn available(kind: &str, executable: Evidence, authentication: Evidence) -> Option<bool> {
    if executable == Evidence::Missing || authentication == Evidence::Missing {
        return Some(false);
    }
    if kind == "simulated"
        && executable == Evidence::NotRequired
        && authentication == Evidence::NotRequired
    {
        Some(true)
    } else {
        None
    }
}
fn validate(record: &RuntimeInventory) -> Result<()> {
    ensure!(
        serde_json::to_vec(record)?.len() <= MAX_INVENTORY,
        "capability report too large"
    );
    label(&record.runtime, 256)?;
    label(&record.version, 64)?;
    if let Some(name) = &record.name {
        crate::runtime_directory::validate_name(name)?;
    }
    ensure!(
        record.capabilities.len() <= 128 && record.protocol.features.len() <= 32,
        "capability inventory too large"
    );
    ensure!(
        (1..=64).contains(&record.capacity.concurrency)
            && record.capacity.active <= 1024
            && record.capacity.available <= record.capacity.concurrency,
        "invalid capability capacity"
    );
    let free = if record.capacity.draining {
        0
    } else {
        record
            .capacity
            .concurrency
            .saturating_sub(record.capacity.active)
    };
    ensure!(
        record.capacity.available == free,
        "inconsistent capability capacity"
    );
    for feature in &record.protocol.features {
        label(feature, 64)?;
    }
    let mut seen = BTreeSet::new();
    for capability in &record.capabilities {
        ensure!(
            capability.configuration_hash.len() == 64
                && capability
                    .configuration_hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "invalid capability configuration hash"
        );
        for value in [
            &capability.id,
            &capability.executor,
            &capability.provider,
            &capability.kind,
        ] {
            label(value, 128)?;
        }
        if let Some(model) = &capability.model {
            label(model, 256)?;
        }
        ensure!(
            capability.id == capability.executor && seen.insert(&capability.id),
            "capability IDs must be unique executor names"
        );
        ensure!(
            ["codex", "claude", "grok", "tuara", "simulated"].contains(&capability.kind.as_str()),
            "unsupported executor capability kind"
        );
        ensure!(
            capability.availability.executable != Evidence::CredentialPresent
                && capability.availability.authentication != Evidence::Available,
            "invalid capability evidence category"
        );
        ensure!(
            capability.availability.authentication != Evidence::NotRequired
                || capability.kind == "simulated",
            "provider authentication cannot be waived"
        );
        ensure!(
            capability.availability.executable != Evidence::NotRequired
                || ["tuara", "simulated"].contains(&capability.kind.as_str()),
            "external executor requires executable evidence"
        );
        ensure!(
            !capability.availability.verified,
            "capability inventory cannot claim verified provider authentication"
        );
        ensure!(
            capability.available
                == available(
                    &capability.kind,
                    capability.availability.executable,
                    capability.availability.authentication
                ),
            "inconsistent capability availability evidence"
        );
    }
    Ok(())
}
fn executable(program: &str) -> Evidence {
    fn check(path: &Path) -> Evidence {
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 => {
                Evidence::Available
            }
            Ok(_) => Evidence::Missing,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Evidence::Missing,
            Err(_) => Evidence::Unknown,
        }
    }
    if Path::new(program).is_absolute() {
        return check(Path::new(program));
    }
    if program.contains('/') {
        // Relative programs resolve in a task repository, which inventory does not know.
        return Evidence::Unknown;
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return Evidence::Unknown;
    };
    let candidates = std::env::split_paths(&paths)
        .map(|path| check(&path.join(program)))
        .collect::<Vec<_>>();
    if candidates.contains(&Evidence::Available) {
        Evidence::Available
    } else if candidates.contains(&Evidence::Unknown) {
        Evidence::Unknown
    } else {
        Evidence::Missing
    }
}
fn credential_evidence(name: &str) -> Evidence {
    if let Some(value) = std::env::var_os(name) {
        return if value.is_empty() {
            Evidence::Missing
        } else {
            Evidence::CredentialPresent
        };
    }
    if !Settings::credentials_path().exists() {
        return Evidence::Missing;
    }
    match crate::config::credential(name) {
        Ok(value) if !value.trim().is_empty() => Evidence::CredentialPresent,
        Ok(_) => Evidence::Missing,
        Err(_) => Evidence::Unknown,
    }
}
/// Fingerprint effective settings without reading or including credential values.
pub fn configuration_hash(config: &ExecutorConfig) -> Result<String> {
    Ok(crate::store::hash(&serde_json::to_vec(config)?))
}

fn capability(
    id: &str,
    provider: &str,
    config: &ExecutorConfig,
    credential: &impl Fn(&str) -> Evidence,
) -> Result<Capability> {
    let executable = match config.kind.as_str() {
        "tuara" | "simulated" => Evidence::NotRequired,
        "codex" | "claude" => executable(config.program.as_deref().unwrap_or(&config.kind)),
        _ => Evidence::Unknown,
    };
    let authentication = if config.kind == "simulated" {
        Evidence::NotRequired
    } else if config.auth_mode == "api" || config.kind == "tuara" {
        credential(&config.api_key_env)
    } else {
        Evidence::Unknown
    };
    Ok(Capability {
        id: id.into(),
        configuration_hash: configuration_hash(config)?,
        executor: id.into(),
        provider: provider.into(),
        model: config.model.clone(),
        kind: config.kind.clone(),
        available: available(&config.kind, executable, authentication),
        availability: Availability {
            executable,
            authentication,
            verified: false,
        },
    })
}
fn local_from(
    db: &Store,
    settings: &Settings,
    credential: impl Fn(&str) -> Evidence,
) -> Result<RuntimeInventory> {
    let path = db.root.join("network-runtime.toml");
    let runtime = if path.exists() {
        crate::network::NetworkConfig::load(Some(&path))?.runtime_id
    } else {
        "local".into()
    };
    let concurrency = management::limit(db)?;
    let active: usize = db.conn.query_row(
        "SELECT COUNT(*) FROM attempts WHERE state='running'",
        [],
        |row| row.get(0),
    )?;
    let draining = management::draining(db)?;
    let capabilities = settings
        .executors
        .iter()
        .map(|(id, executor)| {
            let resolved = settings
                .executor(id)
                .context("capability provider is not configured")?;
            capability(id, executor.provider(), &resolved, &credential)
        })
        .collect::<Result<Vec<_>>>()?;
    let record = RuntimeInventory {
        runtime,
        name: Some(crate::runtime_directory::local_name(db)?),
        local: true,
        fresh: true,
        ready: true,
        observed_at: now(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol: Protocol {
            version: 1,
            features: vec![
                "heartbeat_ack".into(),
                "capability_inventory".into(),
                "execution_selection".into(),
                "runtime_skills_update".into(),
            ],
        },
        capacity: Capacity {
            concurrency,
            active,
            available: if draining {
                0
            } else {
                concurrency.saturating_sub(active)
            },
            draining,
        },
        capabilities,
    };
    validate(&record)?;
    Ok(record)
}

pub fn local(db: &Store) -> Result<Value> {
    Ok(serde_json::to_value(local_from(
        db,
        &Settings::load_user()?,
        credential_evidence,
    )?)?)
}

/// Accept only a bounded, whitelisted report from this authenticated runtime.
pub fn observe(db: &Store, runtime: &str, packet: &Value) -> Result<()> {
    ensure!(
        packet.to_string().len() <= MAX_INVENTORY,
        "capability report too large"
    );
    // Public inventory augments the stable wire record with pack status. Accept
    // that same record for observations while keeping its wire schema unchanged.
    let mut wire = packet.clone();
    if let Some(fields) = wire.as_object_mut() {
        if let Some(pack) = fields.remove("skill_pack") {
            ensure!(
                pack.is_null() || safe_pack_report(&pack).is_some(),
                "invalid skill pack report"
            );
        }
        if let Some(error) = fields
            .remove("skill_pack_error")
            .filter(|value| !value.is_null())
        {
            label(error.as_str().context("invalid skill pack error")?, 1024)?;
        }
    }
    let report: RuntimeInventory =
        serde_json::from_value(wire).context("invalid worker capability report")?;
    validate(&report)?;
    ensure!(
        report.runtime == runtime,
        "capability report identity mismatch"
    );
    let record = RuntimeInventory {
        local: false,
        observed_at: now(),
        fresh: true,
        ..report
    };
    management::set(
        db,
        &format!("{PREFIX}{runtime}"),
        &serde_json::to_string(&record)?,
    )
}

fn unknown(row: &Value) -> Result<RuntimeInventory> {
    let runtime = row["id"]
        .as_str()
        .context("runtime identity missing")?
        .to_owned();
    let status: Value = row["runtime_status"]
        .as_str()
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or(Value::Null);
    let concurrency = status["concurrency"].as_u64().unwrap_or(0).min(64) as usize;
    let active = status["active"].as_u64().unwrap_or(0).min(1024) as usize;
    let draining = status["draining"].as_bool().unwrap_or(false);
    Ok(RuntimeInventory {
        runtime,
        name: row["name"].as_str().map(str::to_owned),
        local: false,
        fresh: false,
        ready: false,
        observed_at: row["last_seen"].as_i64().unwrap_or(0),
        version: row["version"].as_str().unwrap_or("unknown").into(),
        protocol: Protocol {
            version: 0,
            features: vec![],
        },
        capacity: Capacity {
            concurrency,
            active,
            available: 0,
            draining,
        },
        capabilities: vec![],
    })
}
fn inventory_from(db: &Store, local: RuntimeInventory) -> Result<Value> {
    let listed = crate::fleet::dispatch(db, "runtime_list", &json!({}))?
        .context("runtime directory unavailable")?;
    let mut records = listed
        .as_array()
        .context("runtime list unavailable")?
        .iter()
        .map(|row| {
            let record = unknown(row)?;
            Ok((record.runtime.clone(), record))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    for row in db.rows(
        "SELECT key,value FROM runtime_settings WHERE key GLOB 'runtime_capabilities:*'",
        &[],
    )? {
        let key = row["key"].as_str().context("capability storage key")?;
        let record: RuntimeInventory =
            serde_json::from_str(row["value"].as_str().context("capability record")?)?;
        validate(&record)?;
        ensure!(
            key.strip_prefix(PREFIX) == Some(&record.runtime),
            "stored capability identity mismatch"
        );
        records.insert(record.runtime.clone(), record);
    }
    let mut local_value = serde_json::to_value(&local)?;
    let status = management::status(db)?;
    local_value["skill_pack"] = status["skill_pack"].clone();
    if let Some(error) = status
        .get("skill_pack_error")
        .filter(|value| !value.is_null())
    {
        local_value["skill_pack_error"] = error.clone();
    }
    let mut runtimes = vec![local_value];
    for (runtime, record) in records {
        if runtime == local.runtime
            || management::value(db, &format!("runtime_removed:{runtime}"))?.as_deref()
                == Some("true")
        {
            continue;
        }
        let removed: bool = db.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM managed_runtimes WHERE id=? AND state='removed')",
            [&runtime],
            |row| row.get(0),
        )?;
        if removed {
            continue;
        }
        let fresh = (now() - FRESH_SECONDS..=now() + 1).contains(&record.observed_at);
        let ready = fresh
            && record.protocol.version == 1
            && crate::runtime_directory::resolve(db, &runtime).is_ok();
        let current = RuntimeInventory {
            name: crate::runtime_directory::display_name(db, &runtime)?.or(record.name.clone()),
            local: false,
            fresh,
            ready,
            ..record
        };
        let mut value = serde_json::to_value(current)?;
        let presence = db.rows(
            "SELECT status FROM runtime_presence WHERE runtime=?",
            &[&runtime],
        )?;
        value["skill_pack"] = presence
            .first()
            .and_then(|row| row["status"].as_str())
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .and_then(|status| safe_pack_report(&status["skill_pack"]))
            .unwrap_or(Value::Null);
        runtimes.push(value);
    }
    Ok(json!({"observed_at":now(),"runtimes":runtimes}))
}

// Presence is authenticated but its fields are still worker-supplied. Keep only
// bounded content identifiers in the agent-facing inventory.
fn safe_pack_report(value: &Value) -> Option<Value> {
    let hash = value["hash"].as_str()?;
    let names = value["skills"].as_array()?;
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) || names.len() > 64 {
        return None;
    }
    let skills = names
        .iter()
        .map(|name| {
            let name = name.as_str()?;
            if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
                return None;
            }
            Some(name)
        })
        .collect::<Option<Vec<_>>>()?;
    Some(json!({"hash":hash,"skills":skills}))
}

pub fn inventory(db: &Store) -> Result<Value> {
    inventory_from(
        db,
        local_from(db, &Settings::load_user()?, credential_evidence)?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Executor, Provider, Settings};
    use std::os::unix::fs::PermissionsExt;

    fn settings(program: &str) -> Settings {
        Settings {
            providers: [
                (
                    "subscription".into(),
                    Provider {
                        kind: "codex".into(),
                        auth_mode: "login".into(),
                        program: Some(program.into()),
                        model: Some("model-a".into()),
                        base_url: "https://secret-endpoint.test".into(),
                        api_key_env: "SECRET_KEY".into(),
                        ..Default::default()
                    },
                ),
                (
                    "fake".into(),
                    Provider {
                        kind: "simulated".into(),
                        ..Default::default()
                    },
                ),
            ]
            .into(),
            executors: [
                (
                    "coder".into(),
                    Executor {
                        provider: Some("subscription".into()),
                        ..Default::default()
                    },
                ),
                (
                    "tester".into(),
                    Executor {
                        provider: Some("fake".into()),
                        ..Default::default()
                    },
                ),
            ]
            .into(),
            ..Default::default()
        }
    }
    fn store(root: &std::path::Path) -> crate::store::Store {
        let db = crate::store::Store::open(root).unwrap();
        crate::management::set(&db, "concurrency", "3").unwrap();
        crate::runtime_directory::set_local_name(&db, "test-host").unwrap();
        db
    }
    #[test]
    fn inventory_distinguishes_configuration_from_verified_auth_and_omits_secrets() {
        let root = tempfile::tempdir().unwrap();
        let db = store(root.path());
        let program = root.path().join("fake-codex");
        std::fs::write(&program, b"#!/bin/sh\nexit 99\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let inventory = local_from(&db, &settings(program.to_str().unwrap()), |_| {
            Evidence::CredentialPresent
        })
        .unwrap();
        let json = serde_json::to_value(&inventory).unwrap();
        assert_eq!(json["capabilities"][0]["id"], "coder");
        assert_eq!(
            json["capabilities"][0]["availability"]["executable"],
            "available"
        );
        assert_eq!(
            json["capabilities"][0]["availability"]["authentication"],
            "unknown"
        );
        assert!(json["capabilities"][0]["available"].is_null());
        assert_eq!(json["capabilities"][1]["available"], true);
        let raw = json.to_string();
        for secret in ["secret-endpoint", "SECRET_KEY", program.to_str().unwrap()] {
            assert!(!raw.contains(secret));
        }
        assert_eq!(json["capacity"]["available"], 3);
    }
    #[test]
    fn missing_program_and_missing_api_credential_are_known_unavailable() {
        let root = tempfile::tempdir().unwrap();
        let db = store(root.path());
        let base = settings("/does/not/exist");
        let local = local_from(&db, &base, |_| Evidence::Missing).unwrap();
        assert_eq!(local.capabilities[0].available, Some(false));
        let configured = Settings {
            providers: [(
                "native".into(),
                Provider {
                    kind: "tuara".into(),
                    auth_mode: "api".into(),
                    api_key_env: "KEY".into(),
                    ..Default::default()
                },
            )]
            .into(),
            executors: [(
                "coder".into(),
                Executor {
                    provider: Some("native".into()),
                    ..Default::default()
                },
            )]
            .into(),
            ..base
        };
        let local = local_from(&db, &configured, |_| Evidence::Missing).unwrap();
        assert_eq!(local.capabilities[0].available, Some(false));
        let present = local_from(&db, &configured, |_| Evidence::CredentialPresent).unwrap();
        assert_eq!(present.capabilities[0].available, None);
    }
    #[test]
    fn remote_inventory_is_bound_to_authenticated_identity_and_receipt_time() {
        let root = tempfile::tempdir().unwrap();
        let db = store(root.path());
        let local = local_from(&db, &settings("/missing"), |_| Evidence::Unknown).unwrap();
        let remote = RuntimeInventory {
            runtime: "ts-worker".into(),
            local: false,
            name: Some("apollo".into()),
            observed_at: 1,
            ..local.clone()
        };
        let packet = serde_json::to_value(&remote).unwrap();
        assert!(observe(&db, "another-worker", &packet).is_err());
        db.conn.execute("INSERT INTO runtime_enrollments VALUES('ts-worker','fingerprint','',9999999999,'active')",[]).unwrap();
        observe(&db, "ts-worker", &packet).unwrap();
        let inventory = inventory_from(&db, local.clone()).unwrap();
        assert!(inventory["runtimes"][1]["observed_at"].as_i64().unwrap() > 1);
        assert_eq!(inventory["runtimes"][1]["fresh"], true);
        db.conn
            .execute("UPDATE runtime_enrollments SET state='revoked'", [])
            .unwrap();
        assert_eq!(
            inventory_from(&db, local).unwrap()["runtimes"][1]["ready"],
            false
        );
    }
    #[test]
    fn remote_capabilities_reject_extra_fields_duplicate_ids_and_expire() {
        let root = tempfile::tempdir().unwrap();
        let db = store(root.path());
        let local = local_from(&db, &settings("/missing"), |_| Evidence::Unknown).unwrap();
        let remote = RuntimeInventory {
            runtime: "ts-worker".into(),
            local: false,
            ..local.clone()
        };
        let mut packet = serde_json::to_value(&remote).unwrap();
        packet["credentials"] = serde_json::json!({"token":"never-store"});
        assert!(observe(&db, "ts-worker", &packet).is_err());
        let duplicate = RuntimeInventory {
            capabilities: vec![
                remote.capabilities[0].clone(),
                remote.capabilities[0].clone(),
            ],
            ..remote.clone()
        };
        assert!(observe(&db, "ts-worker", &serde_json::to_value(duplicate).unwrap()).is_err());
        let stale = RuntimeInventory {
            observed_at: crate::store::now() - 100,
            ..remote
        };
        crate::management::set(
            &db,
            "runtime_capabilities:ts-worker",
            &serde_json::to_string(&stale).unwrap(),
        )
        .unwrap();
        assert_eq!(
            inventory_from(&db, local).unwrap()["runtimes"][1]["fresh"],
            false
        );
    }
    #[test]
    fn configuration_hash_changes_on_effective_settings_but_not_credential_presence() {
        let root = tempfile::tempdir().unwrap();
        let db = store(root.path());
        let settings = settings("/missing");
        let first = local_from(&db, &settings, |_| Evidence::Missing).unwrap();
        let same = local_from(&db, &settings, |_| Evidence::CredentialPresent).unwrap();
        assert_eq!(
            first.capabilities[0].configuration_hash,
            same.capabilities[0].configuration_hash
        );
        let provider = Provider {
            base_url: "https://different.example".into(),
            ..settings.providers["subscription"].clone()
        };
        let next = Settings {
            providers: [
                ("subscription".into(), provider),
                ("fake".into(), settings.providers["fake"].clone()),
            ]
            .into(),
            ..settings
        };
        let changed = local_from(&db, &next, |_| Evidence::Missing).unwrap();
        assert_eq!(first.capabilities[0].id, changed.capabilities[0].id);
        assert_ne!(
            first.capabilities[0].configuration_hash,
            changed.capabilities[0].configuration_hash
        );
    }
}
