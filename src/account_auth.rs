//! One authentication resolver shared by execution and managed account probes.
//! Refresh credentials are only materialized in the controller's private profile.
use crate::{accounts, config::ExecutorConfig, store::Store};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, process::Command};

#[derive(Serialize, Deserialize)]
pub struct AccessTokens {
    pub access_token: String,
    pub chatgpt_account_id: String,
    pub chatgpt_plan_type: Option<String>,
    pub version: i64,
}
impl AccessTokens {
    pub fn refresh(&self) -> Value {
        json!({"accessToken":self.access_token,"chatgptAccountId":self.chatgpt_account_id,"chatgptPlanType":self.chatgpt_plan_type})
    }
    pub fn login(&self) -> Value {
        let mut value = self.refresh();
        value["type"] = json!("chatgptAuthTokens");
        value
    }
}
pub fn redact_message(value: &Value, tokens: &AccessTokens) -> Value {
    crate::secrets::redact_json(
        value,
        &std::collections::BTreeMap::from([("access_token".into(), tokens.access_token.clone())]),
    )
}
fn private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if path.exists() {
        ensure!(
            !std::fs::symlink_metadata(path)?.file_type().is_symlink(),
            "managed profile directory must not be a symlink"
        );
    }
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}
fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    let directory = path.parent().context("private profile directory")?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)
        .map_err(|error| anyhow::anyhow!("cannot update private profile: {}", error.error))?;
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}
pub fn command(
    db: &Store,
    project: &str,
    account: &str,
    config: &ExecutorConfig,
) -> Result<Command> {
    ensure!(
        accounts::validate_account(db, project, config)?,
        "managed account required"
    );
    accounts::authorized(db, project, account)?;
    let profile = accounts::profile_directory(&db.root, project, account)?;
    let home = profile.join("home");
    private_directory(&home)?;
    let mut command =
        crate::executor::clean_command(config.program.as_deref().unwrap_or(&config.kind));
    command
        .env_remove("SSH_AUTH_SOCK")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", profile.join("config"))
        .env("XDG_CACHE_HOME", profile.join("cache"))
        .env("CODEX_HOME", profile.join("codex"))
        .env("CLAUDE_CONFIG_DIR", profile.join("claude"));
    for child in ["codex", "claude", "config", "cache"] {
        private_directory(&profile.join(child))?;
    }
    if config.kind == "claude" && config.auth_mode == "login" {
        let credential = accounts::credential(db, project, account)?;
        ensure!(
            credential.kind == "claude_setup_token",
            "Claude subscription requires a setup token; renew the selected account credential"
        );
        command.env("CLAUDE_CODE_OAUTH_TOKEN", credential.secret);
    }
    Ok(command)
}
pub fn api_key(db: &Store, project: &str, config: &ExecutorConfig) -> Result<String> {
    if accounts::validate_account(db, project, config)? {
        let credential =
            accounts::credential(db, project, config.account.as_deref().context("account")?)?;
        ensure!(
            credential.kind == "api_key",
            "selected account requires an API credential"
        );
        Ok(credential.secret)
    } else {
        crate::config::credential(&config.api_key_env)
    }
}

pub async fn invocation_tokens(
    i: &crate::executor::Invocation<'_>,
    project: &str,
    account: &str,
    force: bool,
    program: Option<&str>,
) -> Result<AccessTokens> {
    let origins =
        i.db.rows("SELECT * FROM remote_origins WHERE task=?", &[&i.task])?;
    if let Some(origin) = origins.first() {
        let config = crate::federation::config(i.db)?;
        let response=crate::federation::call(&config,origin["owner_peer"].as_str().context("controller")?,"account_tokens",json!({"project":project,"account":account,"task":origin["owner_task"],"remote_task":i.task,"force":force})).await?;
        return serde_json::from_value(response)
            .context("invalid controller access token response");
    }
    access_tokens(i.db, project, account, force, program).await
}

pub async fn access_tokens(
    db: &Store,
    project: &str,
    account: &str,
    force: bool,
    program: Option<&str>,
) -> Result<AccessTokens> {
    accounts::authorized(db, project, account)?;
    accounts::identifier(account)?;
    let directory = db
        .root
        .join("private")
        .join("account-refresh")
        .join(account);
    private_directory(&directory)?;
    let lock_path = directory.join("refresh.lock");
    use std::os::unix::fs::OpenOptionsExt;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)?;
    let started = std::time::Instant::now();
    loop {
        match fs2::FileExt::try_lock_exclusive(&lock) {
            Ok(()) => break,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                ensure!(
                    started.elapsed().as_secs() < 60,
                    "account refresh owner is busy"
                );
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
    // Recheck grant and credential after acquiring the cross-process refresh lock.
    let (credential, version) = accounts::credential_with_version(db, project, account)?;
    if credential.kind == "codex_access_token" {
        ensure!(
            !force,
            "access token renewal requires the controller refresh owner"
        );
        return Ok(AccessTokens {
            access_token: credential.secret,
            chatgpt_account_id: credential.metadata["account_id"]
                .as_str()
                .context("Codex account id required")?
                .into(),
            chatgpt_plan_type: credential.metadata["chatgpt_plan_type"]
                .as_str()
                .map(str::to_owned),
            version,
        });
    }
    ensure!(
        credential.kind == "codex_refresh_token",
        "Codex subscription requires a managed refresh credential"
    );
    let auth_path = directory.join("auth.json");
    let version_path = directory.join("credential-version");
    let mut seeded = false;
    if std::fs::read_to_string(&version_path).ok().as_deref() != Some(&version.to_string()) {
        let auth=serde_json::from_str::<Value>(&credential.secret).unwrap_or_else(|_|json!({"auth_mode":"chatgpt","tokens":{"refresh_token":credential.secret,"access_token":credential.metadata["access_token"],"id_token":credential.metadata["id_token"],"account_id":credential.metadata["account_id"]}}));
        ensure!(
            auth["tokens"]["refresh_token"].is_string(),
            "Codex credential requires refresh token material"
        );
        write_private_atomic(&auth_path, &serde_json::to_vec(&auth)?)?;
        write_private_atomic(&version_path, version.to_string().as_bytes())?;
        seeded = true;
    }
    let refreshed = directory.join("refreshed-at");
    let recent = std::fs::read_to_string(&refreshed)
        .ok()
        .and_then(|x| x.parse::<i64>().ok())
        .is_some_and(|when| crate::store::now() - when < 30);
    if seeded || force || !recent {
        let mut command = crate::executor::clean_command(program.unwrap_or("codex"));
        command
            .env_remove("SSH_AUTH_SOCK")
            .env("HOME", &directory)
            .env("CODEX_HOME", &directory);
        command.args([
            "app-server",
            "--stdio",
            "-c",
            "cli_auth_credentials_store=\"file\"",
        ]);
        tokio::time::timeout(std::time::Duration::from_secs(45), async {
            let mut session =
                crate::codex_session::Session::spawn_locked(command, None, Some(&lock))?;
            session.initialize().await?;
            session
                .request("account/read", json!({"refreshToken":true}))
                .await?;
            session.stop().await;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("controller account refresh timed out")??;
        write_private_atomic(&refreshed, crate::store::now().to_string().as_bytes())?;
    }
    accounts::authorized(db, project, account)?;
    ensure!(
        version == accounts::credential_version(db, account)?,
        "credential changed during refresh; retry with current version"
    );
    let auth: Value = serde_json::from_slice(&std::fs::read(auth_path)?)?;
    Ok(AccessTokens {
        access_token: auth["tokens"]["access_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("controller refresh returned no access token")?
            .into(),
        chatgpt_account_id: auth["tokens"]["account_id"]
            .as_str()
            .or_else(|| credential.metadata["account_id"].as_str())
            .context("Codex account id required")?
            .into(),
        chatgpt_plan_type: credential.metadata["chatgpt_plan_type"]
            .as_str()
            .map(str::to_owned),
        version,
    })
}

/// Called only by authenticated federation/control dispatch; verifies a live assignment.
pub fn remote_tokens(db: &Store, peer: &str, args: &Value) -> Result<Value> {
    authorize_remote_tokens(db, peer, args)?;
    let root = db.root.clone();
    let peer = peer.to_owned();
    let args = args.clone();
    std::thread::spawn(move || {
        let db = Store::open(&root)?;
        let project = args["project"].as_str().context("project required")?;
        let account = args["account"].as_str().context("account required")?;
        let force = args["force"].as_bool().unwrap_or(false);
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(async {
                let tokens = access_tokens(&db, project, account, force, None).await?;
                // Refresh can wait for another process or provider. A revoked runtime
                // or canceled assignment must not receive the eventual response.
                authorize_remote_tokens(&db, &peer, &args)?;
                serde_json::to_value(tokens).map_err(Into::into)
            })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("controller refresh worker failed"))?
}

fn authorize_remote_tokens(db: &Store, peer: &str, args: &Value) -> Result<()> {
    let project = args["project"].as_str().context("project required")?;
    let account = args["account"].as_str().context("account required")?;
    let task = args["task"].as_str().context("task required")?;
    let remote_task = args["remote_task"]
        .as_str()
        .context("remote task required")?;
    crate::projects::authorize_task(db, project, task)?;
    accounts::authorized(db, project, account)?;
    ensure!(
        crate::projects::runtime_allowed(db, project, peer)?,
        "runtime grant revoked"
    );
    let linked:bool=db.conn.query_row("SELECT EXISTS(SELECT 1 FROM remote_links l JOIN tasks t ON t.id=l.task WHERE l.task=? AND l.peer=? AND l.remote_id=? AND l.state='running' AND t.status NOT IN ('succeeded','completed','failed','cancelled','blocked'))",rusqlite::params![task,peer,remote_task],|r|r.get(0))?;
    ensure!(
        linked,
        "credential refresh requires the assigned authenticated runtime"
    );
    let selected:bool=db.conn.query_row("SELECT EXISTS(SELECT 1 FROM account_remote_reservations r JOIN auth_profiles p ON p.account=r.account WHERE r.task=? AND r.project=? AND r.account=? AND r.runtime=? AND r.state='active' AND r.credential_version=p.credential_version)",rusqlite::params![task,project,account,peer],|r|r.get(0))?;
    ensure!(selected, "account is not bound to this remote assignment");
    Ok(())
}

/// Native project commands use separate home/config/cache state. The migrated
/// default project retains its existing environment for compatibility.
pub fn scope_command(command: &mut Command, db: &Store, task: &str) -> Result<()> {
    let project = crate::projects::task_project(db, task)?;
    if project == crate::projects::DEFAULT_PROJECT {
        return Ok(());
    }
    let root = crate::projects::storage_root(db, &project)?.join("command-state");
    for name in ["home", "config", "cache", "data", "state"] {
        private_directory(&root.join(name))?;
    }
    command
        .env_remove("SSH_AUTH_SOCK")
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"));
    Ok(())
}
