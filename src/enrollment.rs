//! Short-lived bootstrap packets provision unique mTLS identities, never provider login stores.
use crate::{
    network::{DirectPeer, NetworkConfig, Provider},
    store::{Store, now},
};
use anyhow::{Context, Result, ensure};
use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, Issuer, KeyPair};
use rusqlite::params;
use serde_json::{Value, json};
use std::path::Path;
pub fn issue(db: &Store, id: &str, profile: &crate::fleet::Profile) -> Result<Option<Value>> {
    let concurrency = profile.concurrency;
    let fleet = crate::fleet::load()?;
    let Some(keyfile) = fleet.issuer_key else {
        return Ok(None);
    };
    let address = fleet
        .controller_address
        .context("controller_address required with issuer_key")?;
    let tls_name = fleet
        .controller_tls_name
        .context("controller_tls_name required with issuer_key")?;
    let controller = crate::federation::config(db)?;
    use std::os::unix::fs::PermissionsExt;
    ensure!(
        std::fs::metadata(&keyfile)?.permissions().mode() & 0o077 == 0,
        "issuer key must be private"
    );
    let ca = std::fs::read_to_string(&controller.ca_cert)?;
    let issuer =
        Issuer::from_ca_cert_pem(&ca, KeyPair::from_pem(&std::fs::read_to_string(keyfile)?)?)?;
    let key = KeyPair::generate()?;
    let mut parameters = CertificateParams::new(vec![format!("{id}.task.internal")])?;
    parameters.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ClientAuth,
        ExtendedKeyUsagePurpose::ServerAuth,
    ];
    parameters.not_before = time::OffsetDateTime::now_utc() - time::Duration::minutes(5);
    parameters.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(30);
    let cert = parameters.signed_by(&key, &issuer)?;
    let fingerprint = crate::store::hash(cert.der());
    let token = crate::store::id();
    let parent_cert = pem::parse(std::fs::read(&controller.identity_cert)?)?;
    let parent_fingerprint = crate::store::hash(parent_cert.contents());
    let mut network = NetworkConfig {
        provider: Provider::Direct,
        runtime_id: id.into(),
        controller_peer: Some(controller.runtime_id.clone()),
        enrollment_token: Some(token.clone()),
        ..Default::default()
    };
    network
        .execution_clients
        .push(controller.runtime_id.clone());
    network
        .management_clients
        .push(controller.runtime_id.clone());
    network
        .allowed_clients
        .insert(parent_fingerprint, controller.runtime_id.clone());
    network
        .peers
        .insert(controller.runtime_id, DirectPeer { address, tls_name });
    let mut settings = crate::config::Settings::load_user()?;
    settings.concurrency = concurrency;
    settings
        .executors
        .retain(|role, _| profile.executor_roles.contains(role));
    settings.fallbacks.retain(|from, to| {
        settings.executors.contains_key(from) && settings.executors.contains_key(to)
    });
    let referenced: std::collections::BTreeSet<_> = settings
        .executors
        .values()
        .map(|e| e.provider().to_owned())
        .collect();
    settings
        .providers
        .retain(|slug, _| referenced.contains(slug));
    let mut credentials = std::collections::BTreeMap::new();
    for config in settings.resolved().values() {
        ensure!(
            config.auth_mode == "api" || config.kind == "tuara" || config.kind == "simulated",
            "subscription login must be performed explicitly on the remote runtime"
        );
        if config.kind != "simulated" {
            credentials.insert(
                config.api_key_env.clone(),
                crate::config::credential(&config.api_key_env)?,
            );
        }
    }
    db.conn.execute("INSERT INTO runtime_enrollments(runtime,fingerprint,token_hash,expires,state) VALUES(?,?,?,?,'pending')",params![id,fingerprint,crate::store::hash(token.as_bytes()),now()+900])?;
    Ok(Some(
        json!({"id":id,"network":network,"ca":ca,"certificate":cert.pem(),"key":key.serialize_pem(),"concurrency":concurrency,"settings":settings,"credentials":credentials}),
    ))
}
pub fn identity(db: &Store, fingerprint: &str) -> Result<Option<String>> {
    Ok(db.rows("SELECT runtime FROM runtime_enrollments WHERE fingerprint=? AND (state='active' OR (state='pending' AND expires>?))",&[&fingerprint,&now()])?.first().and_then(|v|v["runtime"].as_str()).map(str::to_owned))
}
pub fn activate(db: &Store, id: &str, token: Option<&str>) -> Result<bool> {
    let rows = db.rows(
        "SELECT * FROM runtime_enrollments WHERE runtime=? AND state!='revoked'",
        &[&id],
    )?;
    let Some(row) = rows.first() else {
        return Ok(false);
    };
    if row["state"] == "active" {
        return Ok(true);
    }
    ensure!(
        row["expires"].as_i64().unwrap_or(0) > now(),
        "enrollment expired"
    );
    ensure!(
        token.is_some_and(|t| row["token_hash"] == crate::store::hash(t.as_bytes())),
        "invalid enrollment token"
    );
    db.conn.execute(
        "UPDATE runtime_enrollments SET state='active',token_hash='' WHERE runtime=?",
        [id],
    )?;
    db.conn
        .execute("UPDATE managed_runtimes SET state='ready' WHERE id=?", [id])?;
    Ok(true)
}
fn write_same_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if path.exists() {
        let metadata = std::fs::symlink_metadata(path)?;
        ensure!(
            metadata.is_file()
                && metadata.permissions().mode() & 0o077 == 0
                && std::fs::read(path)? == bytes,
            "bootstrap cannot replace existing private configuration"
        );
        return Ok(());
    }
    crate::secrets::write_private(path, bytes)
}
pub fn bootstrap(root: &Path) -> Result<()> {
    let Ok(raw) = crate::branding::var("HORDE_BOOTSTRAP_JSON") else {
        return Ok(());
    };
    apply_bootstrap(root, &raw)
}
pub fn apply_bootstrap(root: &Path, raw: &str) -> Result<()> {
    ensure!(raw.len() <= 128 * 1024, "bootstrap packet too large");
    let packet: Value = serde_json::from_str(raw)?;
    let db = Store::open(root)?;
    let id = packet["id"]
        .as_str()
        .context("bootstrap identity required")?;
    if let Some(existing) = crate::management::value(&db, "bootstrap_identity")? {
        ensure!(
            existing == id,
            "bootstrap identity cannot replace an existing runtime"
        );
        return Ok(());
    }
    let mut network: NetworkConfig = serde_json::from_value(packet["network"].clone())?;
    ensure!(network.runtime_id == id, "bootstrap identity mismatch");
    let tls = root.join("tls");
    std::fs::create_dir_all(&tls)?;
    for (field, file) in [
        ("ca", "ca.pem"),
        ("certificate", "runtime.pem"),
        ("key", "runtime.key"),
    ] {
        write_same_private(
            &tls.join(file),
            packet[field]
                .as_str()
                .context("bootstrap certificate missing")?
                .as_bytes(),
        )?;
    }
    network.ca_cert = tls.join("ca.pem");
    network.identity_cert = tls.join("runtime.pem");
    network.identity_key = tls.join("runtime.key");
    network.validate()?;
    crate::federation::configure(root, &network)?;
    write_same_private(
        &root.join("managed-network.toml"),
        toml::to_string(&network)?.as_bytes(),
    )?;
    let n = packet["concurrency"]
        .as_u64()
        .context("concurrency required")?;
    ensure!((1..=64).contains(&n), "invalid bootstrap concurrency");
    crate::management::set(&db, "concurrency", &n.to_string())?;
    if let Some(settings) = packet.get("settings") {
        let settings: crate::config::Settings = serde_json::from_value(settings.clone())?;
        let config = crate::config::Settings::user_path();
        std::fs::create_dir_all(config.parent().context("configuration directory")?)?;
        write_same_private(&config, toml::to_string(&settings)?.as_bytes())?;
        let credentials: std::collections::BTreeMap<String, String> =
            serde_json::from_value(packet["credentials"].clone())?;
        let mut env = String::new();
        for (name, value) in credentials {
            ensure!(
                !name.contains(['=', '\n', '\r']) && !value.contains(['\n', '\r']),
                "invalid credential environment value"
            );
            env.push_str(&format!("{name}='{value}'\n"));
        }
        write_same_private(&config.with_file_name("credentials.env"), env.as_bytes())?;
    }
    crate::management::set(&db, "bootstrap_identity", id)?;
    Ok(())
}
