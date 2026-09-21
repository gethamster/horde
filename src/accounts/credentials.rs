use super::*;
use std::io::Write;
/// Intentionally has no Debug implementation. Never include this in event payloads.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
    pub kind: String,
    pub secret: String,
    #[serde(default)]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub metadata: Value,
}
pub fn credential_version(db: &Store, account: &str) -> Result<i64> {
    Ok(db.conn.query_row(
        "SELECT credential_version FROM auth_profiles WHERE account=?",
        [account],
        |r| r.get(0),
    )?)
}
fn directory(db: &Store, account: &str) -> Result<PathBuf> {
    identifier(account)?;
    let root = db.root.join("private");
    for path in [
        &root,
        &root.join("accounts"),
        &root.join("accounts").join(account),
    ] {
        if path.exists() {
            ensure!(
                !std::fs::symlink_metadata(path)?.file_type().is_symlink(),
                "private credential directory must not be a symlink"
            );
        }
        std::fs::create_dir_all(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(root.join("accounts").join(account))
}
fn validate(credential: &Credential, provider: &str, mode: &str) -> Result<()> {
    ensure!(
        !credential.secret.is_empty()
            && credential.secret.len() <= 48 * 1024
            && !credential.secret.contains('\0'),
        "invalid credential size or encoding"
    );
    ensure!(
        credential.expires_at.is_none_or(|t| t > now()),
        "credential has expired"
    );
    ensure!(
        match credential.kind.as_str() {
            "api_key" => mode == "api",
            "claude_setup_token" => mode == "login" && provider == "claude",
            "codex_refresh_token" | "codex_access_token" => mode == "login" && provider == "codex",
            _ => false,
        },
        "credential kind does not match account provider and authentication mode"
    );
    Ok(())
}
pub fn set_credential(
    db: &Store,
    project: &str,
    account: &str,
    credential: &Credential,
) -> Result<i64> {
    authorized(db, project, account)?;
    let (owner, provider, mode): (String, String, String) = db.conn.query_row(
        "SELECT owner_project,provider,auth_mode FROM accounts WHERE id=?",
        [account],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    ensure!(
        owner == project,
        "only account owner may replace credentials"
    );
    validate(credential, &provider, &mode)?;
    db.atomic(||{
        let version=credential_version(db,account)?+1;
        let directory=directory(db,account)?;
        let path=directory.join(format!("{version}.json"));
        let mut file=tempfile::NamedTempFile::new_in(&directory)?;
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;file.as_file().set_permissions(std::fs::Permissions::from_mode(0o600))?;}
        let bytes=serde_json::to_vec(credential)?;
        ensure!(bytes.len()<=64*1024,"credential payload too large");
        file.write_all(&bytes)?;file.as_file().sync_all()?;
        // A database rollback can leave an orphan version; replace only that unreferenced next version.
        file.persist(&path).map_err(|e|anyhow::anyhow!("cannot persist credential: {}",e.error))?;
        std::fs::File::open(&directory)?.sync_all()?;
        db.conn.execute("UPDATE auth_profiles SET credential_version=?,kind=?,expires_at=? WHERE account=?",params![version,credential.kind,credential.expires_at,account])?;
        db.conn.execute("UPDATE accounts SET authenticated=1 WHERE id=?",[account])?;
        db.conn.execute("UPDATE credential_deliveries SET state='replacement_pending' WHERE account=? AND state='delivered'",[account])?;
        Ok(version)
    })
}
fn read_version(db: &Store, account: &str, version: i64) -> Result<Credential> {
    ensure!(version > 0, "account has no credential");
    let path = directory(db, account)?.join(format!("{version}.json"));
    ensure!(
        !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
        "credential must not be a symlink"
    );
    let bytes = std::fs::read(path).context("account credential unavailable")?;
    ensure!(bytes.len() <= 64 * 1024, "credential payload too large");
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid stored credential"))
}
pub fn credential(db: &Store, project: &str, account: &str) -> Result<Credential> {
    authorized(db, project, account)?;
    let credential = read_version(db, account, credential_version(db, account)?)?;
    ensure!(
        credential.expires_at.is_none_or(|t| t > now()),
        "account authentication expired; renewal required"
    );
    Ok(credential)
}
/// A transport envelope must travel only over authenticated controller/runtime channels.
#[derive(Serialize, Deserialize)]
pub struct Provision {
    pub granted_at: i64,
    pub project: String,
    pub account: String,
    pub profile: String,
    pub version: i64,
    pub provider: String,
    pub auth_mode: String,
    pub base_url: String,
    pub name: String,
    pub concurrency: u64,
    pub credential_kind: String,
    pub expires_at: Option<i64>,
    pub credential: Option<Credential>,
}
/// Call only after transport authentication has bound `runtime` to the remote peer.
pub fn provision(
    db: &Store,
    project: &str,
    account: &str,
    runtime: &str,
    request_id: &str,
) -> Result<Provision> {
    identifier(request_id)?;
    identifier(runtime)?;
    authorized(db, project, account)?;
    ensure!(
        crate::projects::runtime_allowed(db, project, runtime)?,
        "runtime is not granted to project"
    );
    db.atomic(||{
        let version=credential_version(db,account)?;
        if runtime!="local" {
            let leased:bool=db.conn.query_row("SELECT EXISTS(SELECT 1 FROM account_remote_reservations WHERE request_id=? AND project=? AND account=? AND runtime=? AND credential_version=? AND state='active')",params![request_id,project,account,runtime,version],|r|r.get(0))?;
            ensure!(leased,"credential delivery requires an active account reservation");
        }
        if let Some(old)=db.rows("SELECT * FROM credential_deliveries WHERE request_id=?",&[&request_id])?.first() {
            ensure!(old["project"]==project&&old["account"]==account&&old["runtime"]==runtime&&old["version"]==version&&old["state"]=="delivered","credential delivery identity changed or revoked");
        } else {
            db.conn.execute("INSERT INTO credential_deliveries VALUES(?,?,?,?,?,'delivered',?)",params![request_id,project,account,runtime,version,now()])?;
        }
        let credential=credential(db,project,account)?;
        let kind=credential.kind.clone();let expires_at=credential.expires_at;
        let credential=if kind=="codex_refresh_token"{None}else{Some(credential)};
        let (profile,provider,auth_mode,base_url,name,concurrency)=db.conn.query_row("SELECT p.id,a.provider,a.auth_mode,a.base_url,a.name,a.concurrency FROM auth_profiles p JOIN accounts a ON a.id=p.account WHERE a.id=?",[account],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
        let granted_at=db.conn.query_row("SELECT created FROM account_grants WHERE project=? AND account=?",params![project,account],|r|r.get(0))?;
        Ok(Provision{granted_at,project:project.into(),account:account.into(),profile,version,provider,auth_mode,base_url,name,concurrency,credential_kind:kind,expires_at,credential})
    })
}
/// Install an authenticated controller's account reference. The caller must validate
/// the assignment's immutable project and permitted provider before invoking this.
pub fn receive(db: &Store, envelope: &Provision) -> Result<()> {
    identifier(&envelope.account)?;
    identifier(&envelope.profile)?;
    identifier(&envelope.project)?;
    ensure!(
        envelope.version > 0 && envelope.concurrency > 0 && envelope.concurrency <= 4096,
        "invalid credential envelope"
    );
    ensure!(
        envelope.credential.is_some() || envelope.credential_kind == "codex_refresh_token",
        "credential material missing"
    );
    if let Some(credential) = &envelope.credential {
        ensure!(
            credential.kind == envelope.credential_kind
                && credential.expires_at == envelope.expires_at,
            "credential metadata mismatch"
        );
        validate(credential, &envelope.provider, &envelope.auth_mode)?;
        ensure!(
            credential.kind != "codex_refresh_token",
            "controller refresh material must never be transferred"
        );
    }
    db.atomic(||{
        let prior_grant:Option<i64>=db.conn.query_row("SELECT version FROM account_grant_revocations WHERE project=? AND account=?",params![envelope.project,envelope.account],|r|r.get(0)).optional()?;
        ensure!(prior_grant.is_none_or(|v|v<envelope.granted_at),"account grant was revoked; explicit regrant required");
        if let Some(old)=db.rows("SELECT a.*,p.id AS profile,p.credential_version,p.kind,p.expires_at FROM accounts a JOIN auth_profiles p ON p.account=a.id WHERE a.id=?",&[&envelope.account])?.first() {
            ensure!(old["profile"]==envelope.profile&&old["provider"]==envelope.provider&&old["auth_mode"]==envelope.auth_mode&&old["base_url"]==envelope.base_url,"account identity changed");
            let old_version=old["credential_version"].as_i64().context("credential version")?;
            ensure!(old_version<=envelope.version,"credential version regressed");
            if old_version==envelope.version {
                ensure!(old["kind"]==envelope.credential_kind&&old["expires_at"]==json!(envelope.expires_at),"credential version reused with changed metadata");
                if let Some(incoming)=&envelope.credential {
                    match read_version(db,&envelope.account,envelope.version) {
                        Ok(current)=>ensure!(serde_json::to_value(current)?==serde_json::to_value(incoming)?,"credential version reused with changed material"),
                        Err(_) if old["authenticated"]==0 && prior_grant.is_some_and(|v|v<envelope.granted_at)=>{
                            let directory=directory(db,&envelope.account)?;
                            crate::secrets::write_private(&directory.join(format!("{}.json",envelope.version)),&serde_json::to_vec(incoming)?)?;
                            db.conn.execute("UPDATE accounts SET authenticated=1 WHERE id=?",[&envelope.account])?;
                        },
                        Err(error)=>return Err(error),
                    }
                }
                db.conn.execute("INSERT OR IGNORE INTO account_grants VALUES(?,?,?)",params![envelope.project,envelope.account,envelope.granted_at])?;
                authorized(db,&envelope.project,&envelope.account)?;return Ok(());
            }
        } else {
            let occupied:bool=db.conn.query_row("SELECT EXISTS(SELECT 1 FROM accounts WHERE owner_project=? AND name=?)",params![envelope.project,envelope.name],|r|r.get(0))?;
            // Account IDs remain authoritative across controllers. Reserve a
            // non-user-selectable alias when a local display name is occupied.
            let name=if occupied {format!("received:{}",envelope.account)} else {envelope.name.clone()};
            db.conn.execute("INSERT INTO accounts(id,owner_project,name,provider,auth_mode,base_url,concurrency) VALUES(?,?,?,?,?,?,?)",params![envelope.account,envelope.project,name,envelope.provider,envelope.auth_mode,envelope.base_url,envelope.concurrency])?;
            db.conn.execute("INSERT INTO auth_profiles(id,account) VALUES(?,?)",params![envelope.profile,envelope.account])?;
        }
        if let Some(credential)=&envelope.credential {
            let directory=directory(db,&envelope.account)?;
            crate::secrets::write_private(&directory.join(format!("{}.json",envelope.version)),&serde_json::to_vec(credential)?)?;
        }
        db.conn.execute("UPDATE auth_profiles SET credential_version=?,kind=?,expires_at=? WHERE account=?",params![envelope.version,envelope.credential_kind,envelope.expires_at,envelope.account])?;
        db.conn.execute("UPDATE accounts SET authenticated=1 WHERE id=?",[&envelope.account])?;
        db.conn.execute("INSERT OR IGNORE INTO account_grants VALUES(?,?,?)",params![envelope.project,envelope.account,envelope.granted_at])?;
        Ok(())
    })
}
/// Remote acknowledgements carry only metadata; absence leaves revocation visibly pending.
pub fn acknowledge_removal(db: &Store, project: &str, account: &str, runtime: &str) -> Result<()> {
    db.conn.execute("UPDATE credential_deliveries SET state='removed' WHERE project=? AND account=? AND runtime=? AND state IN ('revocation_pending','replacement_pending')",params![project,account,runtime])?;
    Ok(())
}

/// Revoke a received grant immediately; remove credentials only after its invocations stop.
/// Called by the authenticated controller revocation channel, never by workers.
pub fn remove_received(db: &Store, project: &str, account: &str) -> Result<Value> {
    identifier(project)?;
    identifier(account)?;
    db.atomic(|| {
        db.conn.execute("INSERT OR REPLACE INTO account_grant_revocations SELECT project,account,created FROM account_grants WHERE project=? AND account=?",params![project,account])?;
        db.conn.execute("DELETE FROM account_grants WHERE project=? AND account=?",params![project,account])?;
        db.conn.execute("UPDATE account_reservations SET state='revoked' WHERE project=? AND account=? AND state IN ('active','uncertain')",params![project,account])?;
        Ok(())
    })?;
    let active: i64 = db.conn.query_row("SELECT COUNT(*) FROM attempts a JOIN steps s ON s.id=a.step JOIN task_projects p ON p.task=s.task WHERE p.project=?1 AND a.state IN ('running','uncertain') AND (EXISTS(SELECT 1 FROM attempt_accounts c WHERE c.attempt=a.id AND c.account=?2) OR EXISTS(SELECT 1 FROM attempt_bindings b WHERE b.attempt=a.id AND b.account=?2))",params![project,account],|r|r.get(0))?;
    if active > 0 {
        return Ok(json!({"removed":false,"state":"revocation_pending","active":active}));
    }
    let profile = profile_directory(&db.root, project, account)?;
    if profile.exists() {
        ensure!(
            !std::fs::symlink_metadata(&profile)?
                .file_type()
                .is_symlink(),
            "managed profile must not be a symlink"
        );
        std::fs::remove_dir_all(profile)?;
    }
    let remaining: i64 = db.conn.query_row(
        "SELECT COUNT(*) FROM account_grants WHERE account=?",
        [account],
        |r| r.get(0),
    )?;
    if remaining == 0 {
        let path = directory(db, account)?;
        std::fs::remove_dir_all(path)?;
        db.conn
            .execute("UPDATE accounts SET authenticated=0 WHERE id=?", [account])?;
    }
    db.conn.execute("UPDATE account_reservations SET state='released' WHERE project=? AND account=? AND state='revoked'",params![project,account])?;
    Ok(json!({"removed":true,"state":"removed"}))
}

/// Retire a replaced credential without revoking the project grant.
pub fn retire_received(db: &Store, project: &str, account: &str, version: i64) -> Result<Value> {
    authorized(db, project, account)?;
    ensure!(version > 0, "invalid credential version");
    let active:i64=db.conn.query_row("SELECT COUNT(*) FROM attempts a WHERE a.state IN ('running','uncertain') AND (EXISTS(SELECT 1 FROM attempt_accounts c WHERE c.attempt=a.id AND c.account=?1) OR EXISTS(SELECT 1 FROM attempt_bindings b WHERE b.attempt=a.id AND b.account=?1))",[account],|r|r.get(0))?;
    if active > 0 {
        return Ok(json!({"removed":false,"state":"replacement_pending","active":active}));
    }
    let directory = directory(db, account)?;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if entry.path().extension().is_some_and(|s| s == "json")
            && let Some(found) = entry
                .path()
                .file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.parse::<i64>().ok())
            && found <= version
        {
            ensure!(
                entry.file_type()?.is_file(),
                "credential version must be a regular file"
            );
            std::fs::remove_file(entry.path())?;
        }
    }
    if credential_version(db, account)? <= version {
        db.conn
            .execute("UPDATE accounts SET authenticated=0 WHERE id=?", [account])?;
    }
    Ok(json!({"removed":true,"state":"removed"}))
}

/// Reconcile controller copies only after every project grant and live reservation is gone.
pub fn cleanup_ungranted(db: &Store) -> Result<()> {
    for revoked in db.rows("SELECT project,account FROM account_grant_revocations WHERE NOT EXISTS(SELECT 1 FROM account_grants g WHERE g.project=account_grant_revocations.project AND g.account=account_grant_revocations.account)",&[])? {
        let project=revoked["project"].as_str().context("project")?;
        let account=revoked["account"].as_str().context("account")?;
        let live:bool=db.conn.query_row("SELECT EXISTS(SELECT 1 FROM attempts a JOIN steps s ON s.id=a.step JOIN task_projects p ON p.task=s.task WHERE p.project=?1 AND a.state IN ('running','uncertain') AND (EXISTS(SELECT 1 FROM attempt_accounts c WHERE c.attempt=a.id AND c.account=?2) OR EXISTS(SELECT 1 FROM attempt_bindings b WHERE b.attempt=a.id AND b.account=?2)))",params![project,account],|r|r.get(0))?;
        if !live {
            let profile=profile_directory(&db.root,project,account)?;
            if profile.exists(){ensure!(!std::fs::symlink_metadata(&profile)?.file_type().is_symlink(),"managed profile must not be a symlink");std::fs::remove_dir_all(profile)?;}
        }
    }
    for row in db.rows("SELECT a.id FROM accounts a WHERE NOT EXISTS(SELECT 1 FROM account_grants g WHERE g.account=a.id) AND NOT EXISTS(SELECT 1 FROM account_allocations r WHERE r.account=a.id AND r.state IN ('active','uncertain','revoked'))",&[])? {
        let account=row["id"].as_str().context("account id missing")?;
        identifier(account)?;
        let active:bool=db.conn.query_row("SELECT EXISTS(SELECT 1 FROM attempts a WHERE a.state IN ('running','uncertain') AND (EXISTS(SELECT 1 FROM attempt_accounts c WHERE c.attempt=a.id AND c.account=?1) OR EXISTS(SELECT 1 FROM attempt_bindings b WHERE b.attempt=a.id AND b.account=?1)))",[account],|r|r.get(0))?;
        if active {continue;}
        let refresh=db.root.join("private").join("account-refresh").join(account);
        let lock=if refresh.join("refresh.lock").exists() {
            let file=std::fs::OpenOptions::new().read(true).write(true).open(refresh.join("refresh.lock"))?;
            match fs2::FileExt::try_lock_exclusive(&file) {
                Ok(())=>Some(file),
                Err(error) if error.kind()==std::io::ErrorKind::WouldBlock=>continue,
                Err(error)=>return Err(error.into()),
            }
        }else{None};
        db.atomic(|| {
            let granted:bool=db.conn.query_row("SELECT EXISTS(SELECT 1 FROM account_grants WHERE account=?)",[account],|r|r.get(0))?;
            if granted{return Ok(());}
            for path in [db.root.join("private").join("accounts").join(account),refresh.clone()] {
                if path.exists() {
                    ensure!(!std::fs::symlink_metadata(&path)?.file_type().is_symlink(),"private account directory must not be a symlink");
                    std::fs::remove_dir_all(path)?;
                }
            }
            db.conn.execute("UPDATE accounts SET authenticated=0 WHERE id=?",[account])?;
            Ok(())
        })?;
        drop(lock);
    }
    Ok(())
}
