use super::*;

pub(super) fn from_response(
    body: &Value,
    claims: &Value,
    client_id: &str,
    host: &str,
    previous: Option<&Registration>,
) -> Result<Registration> {
    let required = |name: &str| -> Result<String> {
        body[name]
            .as_str()
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .with_context(|| format!("ChatGPT token response missing {name}"))
    };
    let expires = body["expires_in"]
        .as_i64()
        .filter(|n| *n > 0 && *n <= 86400)
        .context("invalid ChatGPT token expiration")?;
    let scope = body["scope"]
        .as_str()
        .map(|s| s.split_whitespace().map(str::to_owned).collect())
        .or_else(|| previous.map(|r| r.scopes.clone()))
        .context("ChatGPT granted scopes missing")?;
    let registration = Registration {
        issuer: ISSUER.into(),
        subject: claims["sub"]
            .as_str()
            .context("validated subject missing")?
            .into(),
        email: claims["email"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| previous.and_then(|r| r.email.clone())),
        client_id: client_id.into(),
        ext_agent_host_id: host.into(),
        access_token: required("access_token")?,
        refresh_token: required("refresh_token")?,
        id_token: body["id_token"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| previous.map(|r| r.id_token.clone()))
            .context("ChatGPT ID token missing")?,
        token_type: required("token_type")?,
        scopes: scope,
        expires_at: now() + expires,
        earliest_refresh_at: body["earliest_refresh_at"].as_i64(),
    };
    registration.validate()?;
    Ok(registration)
}
pub async fn access_token(db: &Store, project: &str, account: &str) -> Result<String> {
    accounts::authorized(db, project, account)?;
    // The process-wide file lock covers re-read, rotating exchange and durable save.
    // Called on a dedicated execution thread; never hold this on the scheduler lane.
    let _guard = lock_async(db, account).await?;
    let (current, version) = loaded(db, project, account)?;
    ensure!(
        current.plan_enabled(),
        "ChatGPT identity signed in; plan usage permission is required"
    );
    if current.expires_at > now() + 60 {
        return Ok(current.access_token);
    }
    let owner_project: String = db.conn.query_row(
        "SELECT owner_project FROM accounts WHERE id=?",
        [account],
        |r| r.get(0),
    )?;
    owner(db, &owner_project, account)?;
    ensure!(
        current.earliest_refresh_at.is_none_or(|t| t <= now()),
        "ChatGPT refresh is not available yet"
    );
    let http = client()?;
    let body = token(
        &http,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &current.client_id),
            ("refresh_token", &current.refresh_token),
            ("resource", RESOURCE),
        ],
    )
    .await;
    let body = match body {
        Ok(body) => body,
        Err(error) => {
            if error.is::<SessionRevoked>() {
                clear_local(db, account)?;
            }
            return Err(error);
        }
    };
    let claims = if let Some(jwt) = body["id_token"].as_str() {
        identity(&http, jwt, &current.client_id, None, Some(&current.subject)).await?
    } else {
        json!({"sub":current.subject,"email":current.email})
    };
    let updated = from_response(
        &body,
        &claims,
        &current.client_id,
        &current.ext_agent_host_id,
        Some(&current),
    )?;
    if !updated.plan_enabled() {
        clear_local(db, account)?;
        anyhow::bail!("ChatGPT plan usage permission revoked");
    }
    db.atomic(|| {
        let new_version=save(db,&owner_project,account,&updated,version)?;
        for table in ["account_reservations","account_remote_reservations"] {
            db.conn.execute(&format!("UPDATE {table} SET credential_version=? WHERE account=? AND credential_version=? AND state='active'"),rusqlite::params![new_version,account,version])?;
        }
        db.conn.execute("UPDATE attempt_bindings SET credential_version=? WHERE account=? AND credential_version=? AND attempt IN (SELECT id FROM attempts WHERE state='running')",rusqlite::params![new_version,account,version])?;
        Ok(())
    })?;
    accounts::authorized(db, project, account)?;
    Ok(updated.access_token)
}
pub async fn import(db: &Store, project: &str, account: &str, value: Value) -> Result<Value> {
    crate::fleet_enrollment::admin()?;
    owner(db, project, account)?;
    let _guard = lock_async(db, account).await?;
    let mut incoming: Registration =
        serde_json::from_value(value).map_err(|_| anyhow::anyhow!("invalid ChatGPT import"))?;
    incoming.validate()?;
    incoming.ext_agent_host_id = host_id(db, account)?;
    ensure!(
        incoming.expires_at > now(),
        "imported ChatGPT access token expired"
    );
    identity(
        &client()?,
        &incoming.id_token,
        &incoming.client_id,
        None,
        Some(&incoming.subject),
    )
    .await?;
    let version = save(
        db,
        project,
        account,
        &incoming,
        accounts::credential_version(db, account)?,
    )?;
    Ok(summary(account, &incoming, version))
}
/// Explicit local CLI-only export. Never expose this function through daemon/MCP.
pub fn export(db: &Store, project: &str, account: &str) -> Result<Value> {
    crate::fleet_enrollment::admin()?;
    owner(db, project, account)?;
    let _guard = lock(db, account)?;
    let (registration, _) = loaded(db, project, account)?;
    serde_json::to_value(registration).context("cannot encode protected ChatGPT registration")
}
pub(super) fn summary(account: &str, record: &Registration, version: i64) -> Value {
    json!({"account":account,"credential_version":version,"identity_signed_in":true,"plan_usage_enabled":record.plan_enabled(),"subject":record.subject,"email":record.email,"client_id":record.client_id,"expires_at":record.expires_at})
}
pub async fn sign_out(db: &Store, project: &str, account: &str) -> Result<Value> {
    crate::fleet_enrollment::admin()?;
    owner(db, project, account)?;
    let _guard = lock_async(db, account).await?;
    let saved = loaded(db, project, account).ok();
    // Immediately gate subsequent calls before contacting the remote endpoint.
    db.conn
        .execute("UPDATE accounts SET authenticated=0 WHERE id=?", [account])?;
    db.conn.execute("UPDATE account_reservations SET state='revoked' WHERE account=? AND state IN ('active','uncertain')",[account])?;
    db.conn.execute("UPDATE account_remote_reservations SET state='revoked' WHERE account=? AND state IN ('active','uncertain')",[account])?;
    let mut revoked = false;
    if let Some((record, _)) = &saved {
        let http = client()?;
        if let Ok(discovery) = get_json(
            &http,
            "https://auth.openai.com/.well-known/openid-configuration",
        )
        .await
            && discovery["issuer"] == ISSUER
            && validated_discovery_url(&discovery, "revocation_endpoint").is_ok()
        {
            let endpoint = validated_discovery_url(&discovery, "revocation_endpoint")?;
            #[cfg(test)]
            let endpoint = loopback_mock_url(&endpoint);
            for attempt in 0..3 {
                let response = http
                    .post(&endpoint)
                    .form(&[
                        ("token", record.refresh_token.as_str()),
                        ("token_type_hint", "refresh_token"),
                        ("client_id", record.client_id.as_str()),
                    ])
                    .send()
                    .await;
                match response {
                    Ok(response) if response.status() == reqwest::StatusCode::OK => {
                        revoked = true;
                        break;
                    }
                    Ok(response) if !response.status().is_server_error() => break,
                    _ => {
                        if attempt < 2 {
                            tokio::time::sleep(std::time::Duration::from_millis(
                                200 * (1 << attempt),
                            ))
                            .await;
                        }
                    }
                }
            }
        }
    }
    clear_local(db, account)?;
    db.conn.execute("UPDATE credential_deliveries SET state='revocation_pending' WHERE account=? AND state='delivered'",[account])?;
    Ok(
        json!({"account":account,"signed_out":true,"remote_revocation_confirmed":revoked,"registration_retained":true}),
    )
}

fn clear_local(db: &Store, account: &str) -> Result<()> {
    db.atomic(|| {
        db.conn.execute("UPDATE accounts SET authenticated=0 WHERE id=?",[account])?;
        db.conn.execute("UPDATE auth_profiles SET credential_version=credential_version+1,kind=NULL,expires_at=NULL WHERE account=?",[account])?;
        for table in ["account_reservations","account_remote_reservations"] {db.conn.execute(&format!("UPDATE {table} SET state='revoked' WHERE account=? AND state IN ('active','uncertain')"),[account])?;}
        db.conn.execute("UPDATE credential_deliveries SET state='revocation_pending' WHERE account=? AND state='delivered'",[account])?;
        Ok(())
    })?;
    let path = db.root.join("private").join("accounts").join(account);
    if path.exists() {
        ensure!(
            std::fs::symlink_metadata(&path)?.file_type().is_dir(),
            "private account storage invalid"
        );
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            // This directory contains only credential versions and atomic-write
            // temporary files; a crashed writer can leave either behind.
            ensure!(
                entry.file_type()?.is_file(),
                "private account credential invalid"
            );
            std::fs::remove_file(entry.path())?;
        }
    }

    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingTransfer {
    version: i64,
    fingerprint: String,
}

/// Durably gate the source before producing a transportable bundle. A failed
/// write retains a protected, paused session for an explicit CLI retry.
fn prepare_transfer(db: &Store, project: &str, account: &str) -> Result<(Registration, i64)> {
    let (credential, version) = accounts::credential_with_version(db, project, account)?;
    validate_credential(&credential)?;
    let record: Registration = serde_json::from_str(&credential.secret)
        .map_err(|_| anyhow::anyhow!("invalid protected ChatGPT session"))?;
    let fingerprint = crate::store::hash(&serde_json::to_vec(&record)?);
    let journal = root(db, account)?.join("transfer-pending.json");
    db.atomic(|| {
        let live:bool=db.conn.query_row("SELECT EXISTS(SELECT 1 FROM account_allocations WHERE account=? AND state IN ('active','revoked','uncertain'))",[account],|row|row.get(0))?;
        ensure!(!live,"ChatGPT account must be idle before transferring refresh ownership");
        ensure!(accounts::credential_version(db,account)?==version,"account credentials changed during transfer");
        let active:bool=db.conn.query_row("SELECT authenticated=1 FROM accounts WHERE id=?",[account],|row|row.get(0))?;
        if active {
            // Advance the generation to invalidate every pending authorization,
            // while retaining a protected copy until the output is durable.
            let next=accounts::set_credential(db,project,account,&credential)?;
            write_private_atomic(&journal,&serde_json::to_vec(&PendingTransfer {version:next,fingerprint:fingerprint.clone()})?)?;
            db.conn.execute("UPDATE accounts SET authenticated=0 WHERE id=?",[account])?;
            Ok((record,next))
        }else{
            let pending:PendingTransfer=serde_json::from_slice(&read_private(&journal)?).map_err(|_|anyhow::anyhow!("no retryable ChatGPT transfer"))?;
            ensure!(pending.version==version&&pending.fingerprint==fingerprint,"protected transfer generation changed");
            Ok((record,version))
        }
    })
}

/// Transfer renewable-session ownership to the destination. Only the explicit
/// local CLI may call this; a private bundle remains available to retry import.
pub fn export_file(
    db: &Store,
    project: &str,
    account: &str,
    path: &std::path::Path,
) -> Result<Value> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    crate::fleet_enrollment::admin()?;
    owner(db, project, account)?;
    let _guard = lock(db, account)?;
    let (record, version) = prepare_transfer(db, project, account)?;
    let bytes = serde_json::to_vec(&record)?;
    let mut file=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(path).context("cannot create protected ChatGPT transfer file; source is paused, retry export with a new output path")?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::File::open(parent)?.sync_all()?;
    }
    clear_local(db, account)?;
    std::fs::remove_file(root(db, account)?.join("transfer-pending.json"))?;
    Ok(
        json!({"account":account,"exported":true,"credential_version":version,"refresh_owner":"destination","source_signed_out":true,"remote_session_revoked":false}),
    )
}
