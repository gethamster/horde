use super::*;
use fs2::FileExt;
use std::{
    fs::{File, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub client_id: String,
    pub ext_agent_host_id: String,
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub token_type: String,
    pub scopes: Vec<String>,
    pub expires_at: i64,
    #[serde(default)]
    pub earliest_refresh_at: Option<i64>,
}
impl Registration {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.issuer == ISSUER && !self.subject.is_empty() && self.subject.len() <= 1024,
            "invalid ChatGPT registration identity"
        );
        callback_client(Some(&self.client_id), None)?;
        ensure!(
            self.ext_agent_host_id
                .strip_prefix("urn:uuid:")
                .is_some_and(|value| uuid::Uuid::parse_str(value).is_ok()),
            "invalid ChatGPT host identity"
        );
        ensure!(
            self.token_type.eq_ignore_ascii_case("Bearer"),
            "unsupported ChatGPT token type"
        );
        for token in [&self.access_token, &self.refresh_token, &self.id_token] {
            ensure!(
                !token.is_empty() && token.len() <= 32 * 1024 && !token.contains('\0'),
                "invalid ChatGPT token material"
            );
        }
        ensure!(
            self.scopes.len() <= 32 && self.scopes.iter().all(|s| !s.is_empty() && s.len() <= 128),
            "invalid granted scopes"
        );
        Ok(())
    }
    pub fn plan_enabled(&self) -> bool {
        ["resource.invoke", "chatgpt.tokens.use.direct"]
            .iter()
            .all(|required| self.scopes.iter().any(|scope| scope == required))
    }
    fn credential(&self) -> Result<accounts::Credential> {
        self.validate()?;
        Ok(accounts::Credential {
            kind: "chatgpt_oauth".into(),
            secret: serde_json::to_string(self)?,
            expires_at: None,
            metadata: json!({"issuer":self.issuer,"subject":self.subject,"email":self.email,"client_id":self.client_id,"ext_agent_host_id":self.ext_agent_host_id,"identity_signed_in":true,"plan_usage_enabled":self.plan_enabled(),"access_expires_at":self.expires_at}),
        })
    }
}
pub fn validate_credential(credential: &accounts::Credential) -> Result<()> {
    ensure!(
        credential.kind == "chatgpt_oauth" && credential.expires_at.is_none(),
        "invalid ChatGPT credential envelope"
    );
    let registration: Registration = serde_json::from_str(&credential.secret)
        .map_err(|_| anyhow::anyhow!("invalid ChatGPT registration"))?;
    registration.validate()
}
pub(super) fn owner(db: &Store, project: &str, account: &str) -> Result<()> {
    accounts::identifier(project)?;
    accounts::identifier(account)?;
    accounts::authorized(db, project, account)?;
    let valid:bool=db.conn.query_row("SELECT owner_project=? AND provider='chatgpt' AND auth_mode='login' FROM accounts WHERE id=?",rusqlite::params![project,account],|r|r.get(0))?;
    ensure!(valid, "ChatGPT connection requires its account owner");
    Ok(())
}
pub(super) fn directory(path: &Path) -> Result<()> {
    if path.exists() {
        ensure!(
            std::fs::symlink_metadata(path)?.file_type().is_dir(),
            "private storage must be a regular directory"
        );
    }
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}
pub(super) fn root(db: &Store, account: &str) -> Result<PathBuf> {
    accounts::identifier(account)?;
    let private = db.root.join("private");
    directory(&private)?;
    let all = private.join("chatgpt");
    directory(&all)?;
    let root = all.join(account);
    directory(&root)?;
    Ok(root)
}
/// Release the advisory lock explicitly, rather than waiting for every dup/fork
/// copy of its open file description to close. Rust opens descriptors CLOEXEC,
/// but concurrent subprocess forks can inherit them briefly before exec.
pub(super) struct AccountLock {
    file: File,
    owner_pid: u32,
}
impl AccountLock {
    fn acquired(file: File) -> Self {
        Self {
            file,
            owner_pid: std::process::id(),
        }
    }
}
impl Drop for AccountLock {
    fn drop(&mut self) {
        // File close still releases the lock if unlock fails. Drop cannot return
        // an error; do not panic during an unrelated credential failure.
        if std::process::id() == self.owner_pid {
            let _ = FileExt::unlock(&self.file);
        }
    }
}
pub(super) fn lock(db: &Store, account: &str) -> Result<AccountLock> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root(db, account)?.join("refresh.lock"))?;
    file.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("ChatGPT account operation already in progress"))?;
    Ok(AccountLock::acquired(file))
}
pub(super) async fn lock_async(db: &Store, account: &str) -> Result<AccountLock> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root(db, account)?.join("refresh.lock"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(AccountLock::acquired(file)),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                ensure!(
                    std::time::Instant::now() < deadline,
                    "ChatGPT credential operation timed out"
                );
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
}
pub(super) fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("private credential parent missing")?;
    directory(parent)?;
    if path.exists() {
        ensure!(
            std::fs::symlink_metadata(path)?.file_type().is_file(),
            "private credential path invalid"
        );
    }
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)
        .map_err(|_| anyhow::anyhow!("cannot persist ChatGPT private state"))?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
pub(super) fn read_private(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.permissions().mode() & 0o077 == 0,
        "ChatGPT storage must be owner-only"
    );
    let mut bytes = Vec::new();
    file.by_ref().take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 64 * 1024,
        "ChatGPT storage exceeds size limit"
    );
    Ok(bytes)
}
#[derive(Serialize, Deserialize)]
pub(super) struct Mapping {
    pub client_id: String,
    pub subject: String,
    pub email: Option<String>,
    pub ext_agent_host_id: String,
}
pub(super) fn mapping(db: &Store, account: &str) -> Result<Option<Mapping>> {
    let path = root(db, account)?.join("registration.json");
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(
        serde_json::from_slice(&read_private(&path)?)
            .map_err(|_| anyhow::anyhow!("invalid saved ChatGPT mapping"))?,
    ))
}
pub(super) fn save(
    db: &Store,
    project: &str,
    account: &str,
    registration: &Registration,
    expected_version: i64,
) -> Result<i64> {
    owner(db, project, account)?;
    ensure!(
        accounts::credential_version(db, account)? == expected_version,
        "account credentials changed during authorization"
    );
    if let Some(saved) = mapping(db, account)? {
        ensure!(
            saved.client_id == registration.client_id
                && saved.subject == registration.subject
                && saved.ext_agent_host_id == registration.ext_agent_host_id,
            "ChatGPT registration does not match selected account"
        );
    }
    db.atomic(|| {
        ensure!(
            accounts::credential_version(db, account)? == expected_version,
            "account credentials changed during authorization"
        );
        write_private_atomic(
            &root(db, account)?.join("registration.json"),
            &serde_json::to_vec(&Mapping {
                client_id: registration.client_id.clone(),
                subject: registration.subject.clone(),
                email: registration.email.clone(),
                ext_agent_host_id: registration.ext_agent_host_id.clone(),
            })?,
        )?;
        accounts::set_credential(db, project, account, &registration.credential()?)
    })
}
pub(super) fn loaded(db: &Store, project: &str, account: &str) -> Result<(Registration, i64)> {
    accounts::authorized(db, project, account)?;
    let active: bool = db.conn.query_row(
        "SELECT authenticated=1 FROM accounts WHERE id=?",
        [account],
        |r| r.get(0),
    )?;
    ensure!(active, "ChatGPT account is signed out; sign in required");
    let (credential, version) = accounts::credential_with_version(db, project, account)?;
    validate_credential(&credential)?;
    Ok((
        serde_json::from_str(&credential.secret)
            .map_err(|_| anyhow::anyhow!("invalid saved ChatGPT registration"))?,
        version,
    ))
}
pub fn prepare_remote(db: &Store, project: &str, account: &str) -> Result<Value> {
    crate::fleet_enrollment::admin()?;
    owner(db, project, account)?;
    let _guard = lock(db, account)?;
    Ok(json!({"account":account,"ext_agent_host_id":host_id(db,account)?}))
}
pub(super) fn host_id(db: &Store, account: &str) -> Result<String> {
    // One opaque identity describes this runtime across every registration.
    let account_root = root(db, account)?;
    let shared = account_root
        .parent()
        .context("ChatGPT storage root missing")?;
    let guard = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(shared.join("host.lock"))?;
    guard
        .try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("ChatGPT host identity is being initialized"))?;
    let _guard = AccountLock::acquired(guard);
    let path = shared.join("host-id");
    if path.exists() {
        let value =
            String::from_utf8(read_private(&path)?).context("invalid saved host identity")?;
        ensure!(
            value.starts_with("urn:uuid:") && uuid::Uuid::parse_str(&value[9..]).is_ok(),
            "invalid saved host identity"
        );
        return Ok(value);
    }
    let id = format!("urn:uuid:{}", uuid::Uuid::new_v4());
    write_private_atomic(&path, id.as_bytes())?;
    Ok(id)
}
#[cfg(test)]
mod lock_tests {
    use super::*;
    use crate::chatgpt_auth::integration_tests::setup;
    #[test]
    fn released_lock_does_not_wait_for_an_inherited_descriptor_to_close() {
        let (_temp, db, account) = setup();
        let guard = lock(&db, &account).unwrap();
        // dup and fork refer to the same open file description: closing only
        // the parent descriptor must not leave the account busy in a child.
        let inherited = guard.file.try_clone().unwrap();
        assert!(lock(&db, &account).is_err());
        drop(guard);
        let next = lock(&db, &account);
        assert!(
            next.is_ok(),
            "released guard must unlock while inherited descriptors remain open"
        );
        drop(inherited);
    }
    #[test]
    fn async_guard_explicitly_releases_inherited_open_description() {
        let (_temp, db, account) = setup();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let guard = runtime.block_on(lock_async(&db, &account)).unwrap();
        let inherited = guard.file.try_clone().unwrap();
        assert!(lock(&db, &account).is_err());
        drop(guard);
        assert!(lock(&db, &account).is_ok());
        drop(inherited);
    }
}
