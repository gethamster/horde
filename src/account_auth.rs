//! One authentication resolver shared by execution and managed account probes.
//! Refresh credentials are only materialized in the controller's private profile.
use crate::{accounts, config::ExecutorConfig, store::Store};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, process::Command};

/// Resolve the attached Google service account for this request, not for the
/// lifetime of a worker. No refresh token or service-account key is persisted.
pub async fn vertex_access_token() -> Result<String> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    metadata_access_token(&client, "http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token").await
}

async fn metadata_access_token(client: &reqwest::Client, url: &str) -> Result<String> {
    let mut retries = 0;
    let response = loop {
        match client
            .get(url)
            .header("Metadata-Flavor", "Google")
            .send()
            .await
        {
            Ok(response) => break response,
            Err(_) if retries < 2 => {
                // A brief metadata connection failure must not discard an ongoing
                // model conversation. Retry transport only, before any response
                // validation, without retaining credentials between requests.
                tokio::time::sleep(std::time::Duration::from_millis(100 << retries)).await;
                retries += 1;
            }
            Err(_) => anyhow::bail!("Vertex ADC metadata is unavailable"),
        }
    };
    ensure!(
        response.status().is_success(),
        "Vertex ADC metadata returned {}",
        response.status()
    );
    ensure!(
        response
            .headers()
            .get("Metadata-Flavor")
            .is_some_and(|value| value == "Google"),
        "Vertex ADC metadata identity is missing"
    );
    let data: Value = response
        .json()
        .await
        .context("Vertex ADC metadata response is invalid")?;
    ensure!(
        data["token_type"] == "Bearer"
            && data["expires_in"]
                .as_u64()
                .is_some_and(|seconds| seconds > 0),
        "Vertex ADC metadata token is invalid or expired"
    );
    Ok(data["access_token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .context("Vertex ADC metadata token is missing")?
        .to_owned())
}

#[cfg(test)]
mod vertex_tests {
    use super::*;
    use std::io::{Read, Write};

    fn metadata_server(
        responses: Vec<Option<(u16, bool, String)>>,
    ) -> (String, std::thread::JoinHandle<usize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let count = responses.len();
            for response in responses {
                let (mut socket, _) = listener.accept().unwrap();
                let mut headers = Vec::new();
                while !headers.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).unwrap();
                    headers.push(byte[0]);
                }
                assert!(
                    String::from_utf8(headers)
                        .unwrap()
                        .to_ascii_lowercase()
                        .contains("metadata-flavor: google")
                );
                if let Some((status, flavor, body)) = response {
                    write!(socket,"HTTP/1.1 {status} OK\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n{body}",body.len(),if flavor {"Metadata-Flavor: Google\r\n"} else {""}).unwrap();
                }
            }
            count
        });
        (url, server)
    }

    #[tokio::test]
    async fn metadata_transport_failure_recovers_without_restarting_the_native_step() {
        let (url, server) = metadata_server(vec![
            None,
            Some((
                200,
                true,
                json!({"access_token":"recovered","token_type":"Bearer","expires_in":3600})
                    .to_string(),
            )),
        ]);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        assert_eq!(
            metadata_access_token(&client, &url).await.unwrap(),
            "recovered"
        );
        assert_eq!(server.join().unwrap(), 2);
    }

    #[tokio::test]
    async fn metadata_transport_retries_stop_after_three_requests() {
        let (url, server) = metadata_server(vec![None, None, None]);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        assert_eq!(
            metadata_access_token(&client, &url)
                .await
                .unwrap_err()
                .to_string(),
            "Vertex ADC metadata is unavailable"
        );
        assert_eq!(server.join().unwrap(), 3);
    }

    #[tokio::test]
    async fn metadata_responses_fail_immediately_without_transport_retry() {
        for (status, flavor, body, expected) in [
            (403, true, "{}", "403"),
            (503, true, "{}", "503"),
            (200, false, "{}", "identity is missing"),
            (200, true, "invalid json", "response is invalid"),
            (200, true, "{}", "invalid or expired"),
        ] {
            let (url, server) = metadata_server(vec![
                Some((status, flavor, body.to_owned())),
                Some((
                    200,
                    true,
                    json!({"access_token":"next-request","token_type":"Bearer","expires_in":3600})
                        .to_string(),
                )),
            ]);
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            assert!(
                metadata_access_token(&client, &url)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains(expected)
            );
            assert_eq!(
                metadata_access_token(&client, &url).await.unwrap(),
                "next-request"
            );
            assert_eq!(server.join().unwrap(), 2);
        }
    }

    #[tokio::test]
    async fn attached_identity_is_resolved_again_and_invalid_metadata_fails_closed() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for (status, flavor, body) in [
                (
                    200,
                    true,
                    json!({"access_token":"first","token_type":"Bearer","expires_in":1}),
                ),
                (
                    200,
                    true,
                    json!({"access_token":"renewed","token_type":"Bearer","expires_in":3600}),
                ),
                (403, true, json!({"error":"denied"})),
                (
                    200,
                    false,
                    json!({"access_token":"untrusted","token_type":"Bearer","expires_in":3600}),
                ),
            ] {
                let (mut socket, _) = listener.accept().unwrap();
                let mut headers = Vec::new();
                while !headers.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).unwrap();
                    headers.push(byte[0]);
                }
                assert!(
                    String::from_utf8(headers)
                        .unwrap()
                        .to_ascii_lowercase()
                        .contains("metadata-flavor: google")
                );
                let body = body.to_string();
                write!(socket,"HTTP/1.1 {status} OK\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n{body}",body.len(),if flavor {"Metadata-Flavor: Google\r\n"} else {""}).unwrap();
            }
        });
        let client = reqwest::Client::new();
        assert_eq!(metadata_access_token(&client, &url).await.unwrap(), "first");
        assert_eq!(
            metadata_access_token(&client, &url).await.unwrap(),
            "renewed"
        );
        assert!(
            metadata_access_token(&client, &url)
                .await
                .unwrap_err()
                .to_string()
                .contains("403")
        );
        assert!(
            metadata_access_token(&client, &url)
                .await
                .unwrap_err()
                .to_string()
                .contains("identity is missing")
        );
        server.join().unwrap();
    }
}

#[derive(Debug)]
pub struct RefreshOwnerRequired;
impl std::fmt::Display for RefreshOwnerRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("access token renewal requires the controller refresh owner")
    }
}
impl std::error::Error for RefreshOwnerRequired {}

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
    crate::execution_setup::ensure_account(db, project, account)?;
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

/// Native ChatGPT workers receive short-lived access only; refresh remains on
/// the account's owner controller even when the invocation runs elsewhere.
pub async fn invocation_chatgpt_token(
    i: &crate::executor::Invocation<'_>,
    project: &str,
    account: &str,
) -> Result<String> {
    accounts::authorized(i.db, project, account)?;
    let origins =
        i.db.rows("SELECT * FROM remote_origins WHERE task=?", &[&i.task])?;
    if let Some(origin) = origins.first() {
        let response = crate::federation::call(
            &crate::federation::config(i.db)?,
            origin["owner_peer"].as_str().context("controller")?,
            "account_chatgpt_tokens",
            json!({"project":project,"account":account,"task":origin["owner_task"],"remote_task":i.task}),
        ).await?;
        let token = response["access_token"]
            .as_str()
            .filter(|v| !v.is_empty())
            .context("invalid controller access token response")?
            .to_owned();
        let version = response["version"]
            .as_i64()
            .filter(|v| *v > 0)
            .context("invalid controller credential version")?;
        accounts::authorized(i.db, project, account)?;
        i.db.atomic(|| {
            let old = accounts::credential_version(i.db, account)?;
            ensure!(version >= old, "controller credential version regressed");
            if version > old {
                i.db.conn.execute("UPDATE auth_profiles SET credential_version=? WHERE account=? AND kind='chatgpt_oauth' AND credential_version=?", rusqlite::params![version,account,old])?;
                i.db.conn.execute("UPDATE attempt_bindings SET credential_version=? WHERE account=? AND credential_version=? AND attempt IN (SELECT id FROM attempts WHERE state='running')", rusqlite::params![version,account,old])?;
                i.db.conn.execute("UPDATE account_reservations SET credential_version=? WHERE account=? AND credential_version=? AND state='active'", rusqlite::params![version,account,old])?;
            }
            Ok(())
        })?;
        return Ok(token);
    }
    crate::chatgpt_auth::access_token(i.db, project, account).await
}

pub fn remote_chatgpt_tokens(db: &Store, peer: &str, args: &Value) -> Result<Value> {
    authorize_remote_tokens(db, peer, args)?;
    let root = db.root.clone();
    let peer = peer.to_owned();
    let args = args.clone();
    std::thread::spawn(move || {
        let db = Store::open(&root)?;
        let project = args["project"].as_str().context("project required")?;
        let account = args["account"].as_str().context("account required")?;
        tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
            let token = crate::chatgpt_auth::access_token(&db, project, account).await?;
            authorize_remote_tokens(&db, &peer, &args)?;
            Ok(json!({"access_token":token,"version":accounts::credential_version(&db,account)?}))
        })
    })
    .join()
    .map_err(|_| anyhow::anyhow!("controller refresh worker failed"))?
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
        if force {
            return Err(RefreshOwnerRequired.into());
        }
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
