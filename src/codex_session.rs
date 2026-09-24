//! Bounded stdio app-server transport. Authentication messages never become artifacts.
use crate::store::Store;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::process::{Command, Stdio};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

pub struct Session {
    child: tokio::process::Child,
    input: tokio::process::ChildStdin,
    output: BufReader<tokio::process::ChildStdout>,
    next_id: u64,
    pid: u32,
    _storage_process: crate::storage::control::ProcessGuard,
}
impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.pid as i32), libc::SIGKILL);
        }
    }
}
impl Session {
    pub fn spawn(command: Command, record: Option<(&Store, &str)>) -> Result<Self> {
        Self::spawn_locked(command, record, None)
    }
    /// A refresh subprocess must retain its account lock if the daemon crashes.
    /// Only the child changes descriptor inheritance; concurrent workers keep CLOEXEC.
    pub fn spawn_locked(
        command: Command,
        record: Option<(&Store, &str)>,
        lock: Option<&std::fs::File>,
    ) -> Result<Self> {
        use std::os::fd::AsRawFd;
        let lock_fd = lock.map(AsRawFd::as_raw_fd);
        let mut cmd = tokio::process::Command::from(command);
        cmd.kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        unsafe {
            cmd.pre_exec(move || {
                if let Some(fd) = lock_fd {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().context("launch Codex app-server")?;
        let pid = child.id().context("app-server pid")?;
        let storage_process =
            crate::storage::control::register_process(pid).inspect_err(|_| unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            })?;
        if let Some((db, attempt)) = record {
            db.conn.execute(
                "UPDATE attempts SET pid=? WHERE id=?",
                rusqlite::params![pid, attempt],
            )?;
        }
        Ok(Self {
            input: child.stdin.take().context("app-server stdin")?,
            output: BufReader::new(child.stdout.take().context("app-server stdout")?),
            child,
            next_id: 1,
            pid,
            _storage_process: storage_process,
        })
    }
    pub async fn send(&mut self, value: Value) -> Result<()> {
        crate::storage::control::checkpoint().await;
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        self.input.write_all(&bytes).await?;
        self.input.flush().await?;
        Ok(())
    }
    pub async fn receive(&mut self) -> Result<Value> {
        let mut line = Vec::new();
        (&mut self.output)
            .take(8 * 1024 * 1024 + 1)
            .read_until(b'\n', &mut line)
            .await?;
        if line.is_empty() {
            bail!("Codex app-server closed its protocol stream");
        }
        if line.len() > 8 * 1024 * 1024 {
            bail!("Codex app-server message exceeds 8 MiB");
        }
        serde_json::from_slice(&line).context("malformed Codex app-server message")
    }
    pub async fn initialize(&mut self) -> Result<()> {
        self.request("initialize", json!({"clientInfo":{"name":"horde","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}})).await?;
        self.send(json!({"method":"initialized","params":{}})).await
    }
    pub async fn begin_request(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"id":id,"method":method,"params":params}))
            .await?;
        Ok(id)
    }
    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.begin_request(method, params).await?;
        loop {
            let message = self.receive().await?;
            if message["id"] == id && message.get("method").is_none() {
                if message.get("error").is_some() {
                    // Provider error text can contain tokens or serialized authentication parameters.
                    bail!(
                        "Codex app-server rejected {method}; incompatible interface or authentication unavailable"
                    );
                }
                return message
                    .get("result")
                    .cloned()
                    .context("app-server response missing result");
            }
            if message.get("id").is_some() {
                self.send(json!({"id":message["id"],"error":{"code":-32601,"message":"request not allowed before invocation"}})).await?;
            }
        }
    }
    pub async fn stop(&mut self) {
        let _ = self.child.kill().await;
    }
}

pub async fn execute(
    i: &crate::executor::Invocation<'_>,
    config: &crate::config::ExecutorConfig,
    project: &str,
    account: &str,
) -> Result<Value> {
    crate::storage::control::timeout(
        std::time::Duration::from_secs(i.settings.timeout_seconds),
        execute_inner(i, config, project, account),
    )
    .await
    .context("Codex app-server invocation timed out; process group stopped")?
}
async fn execute_inner(
    i: &crate::executor::Invocation<'_>,
    config: &crate::config::ExecutorConfig,
    project: &str,
    account: &str,
) -> Result<Value> {
    let started = std::time::Instant::now();
    let generation = crate::capacity::invocation_generation(i.db, i.attempt, config)?;
    let token = crate::account_auth::invocation_tokens(
        i,
        project,
        account,
        false,
        config.program.as_deref(),
    )
    .await?;
    ensure!(
        generation.as_deref() == Some(format!("managed:{}", token.version).as_str()),
        "controller credential no longer matches invocation binding"
    );
    let mut cmd = crate::account_auth::command(i.db, project, account, config)?;
    cmd.args([
        "app-server",
        "--stdio",
        "-c",
        "cli_auth_credentials_store=\"ephemeral\"",
    ]);
    cmd.current_dir(i.workspace);
    crate::storage::control::checkpoint().await;
    let mut session = Session::spawn(cmd, Some((i.db, i.attempt)))?;
    session.initialize().await?;
    session
        .request("account/login/start", token.login())
        .await?;
    let executable = std::env::current_exe()?;
    let thread = session
        .request("thread/start", thread_start(i, config, &executable))
        .await?;
    let thread_id = thread["thread"]["id"]
        .as_str()
        .context("Codex app-server missing thread id")?;
    let turn_request = session.begin_request("turn/start",json!({"threadId":thread_id,"input":[{"type":"text","text":i.prompt()?,"text_elements":[]}]})).await?;
    let mut text = None;
    let mut usage = Value::Null;
    let mut event_bytes = Vec::new();
    let mut secrets = std::collections::BTreeMap::from([
        ("initial_access_token".into(), token.access_token.clone()),
        ("worker_token".into(), i.token.to_owned()),
    ]);
    loop {
        let message = session.receive().await?;
        let method = message["method"].as_str().unwrap_or("");
        if message["id"] == turn_request && message.get("method").is_none() {
            if message.get("error").is_some() {
                bail!("Codex app-server rejected turn/start");
            }
            continue;
        }
        if message.get("id").is_some() {
            if method == "account/chatgptAuthTokens/refresh" {
                crate::capacity::ensure_generation(i.db, config, generation.as_deref())?;
                let tokens = crate::account_auth::invocation_tokens(
                    i,
                    project,
                    account,
                    true,
                    config.program.as_deref(),
                )
                .await?;
                ensure!(
                    generation.as_deref() == Some(format!("managed:{}", tokens.version).as_str()),
                    "renewed credential no longer matches invocation binding"
                );
                secrets.insert(
                    format!("refresh_{}", secrets.len()),
                    tokens.access_token.clone(),
                );
                session
                    .send(json!({"id":message["id"],"result":tokens.refresh()}))
                    .await?;
            } else {
                session.send(json!({"id":message["id"],"error":{"code":-32601,"message":"interactive requests are disabled"}})).await?;
                bail!("Codex requested an unsupported interactive operation");
            }
            continue;
        }
        // Persist only model output and aggregate usage, never auth/control messages.
        if [
            "item/completed",
            "thread/tokenUsage/updated",
            "turn/completed",
        ]
        .contains(&method)
        {
            let redacted = crate::secrets::redact_json(&message, &secrets);
            let bytes = serde_json::to_vec(&redacted)?;
            if event_bytes.len() + bytes.len() > 8 * 1024 * 1024 {
                bail!("Codex event transcript exceeds 8 MiB");
            }
            event_bytes.extend(bytes);
            event_bytes.push(b'\n');
        }
        match method {
            "item/completed" if message["params"]["item"]["type"] == "agentMessage" => {
                text = message["params"]["item"]["text"]
                    .as_str()
                    .map(str::to_owned)
            }
            "thread/tokenUsage/updated" => {
                let report = &message["params"]["tokenUsage"];
                let total = &report["total"];
                usage = json!({"provider":{"input_tokens":total["inputTokens"],"output_tokens":total["outputTokens"],"cached_input_tokens":total["cachedInputTokens"]},"app_server":report,"subscription_capacity":null,"executor_role":i.spec.role,"account":account,"project":project})
            }
            "account/rateLimits/updated" => {
                crate::capacity::ingest_current(
                    i.db,
                    config,
                    generation.as_deref(),
                    &message["params"],
                )?;
            }
            "turn/completed" => {
                if message["params"]["turn"]["status"] != "completed" {
                    if crate::executor::is_capacity_message(
                        message["params"]["turn"]["error"]["message"]
                            .as_str()
                            .unwrap_or(""),
                    ) {
                        return Err(crate::executor::CapacityFailure(json!({"error":"Codex subscription capacity unavailable", "capacity":true})).into());
                    }
                    bail!("Codex app-server turn failed or was interrupted");
                }
                break;
            }
            "error" => bail!("Codex app-server reported an invocation error"),
            _ => {}
        }
    }
    session.stop().await;
    let artifact = i.db.artifact(
        i.task,
        Some(i.step),
        "executor-events",
        &event_bytes,
        &json!({"attempt":i.attempt}),
        false,
    )?;
    i.db.conn.execute(
        "UPDATE attempts SET usage=? WHERE id=?",
        rusqlite::params![usage.to_string(), i.attempt],
    )?;
    let text =
        crate::secrets::redact_values(&text.context("Codex returned no final result")?, &secrets);
    let mut result = crate::executor::parse_result(&text)?;
    result["usage"] = usage;
    result["events_artifact"] = json!(artifact);
    result["latency_ms"] = json!(started.elapsed().as_millis() as u64);
    Ok(result)
}

/// The `thread/start` request: a workspace-write sandbox with the coordination
/// MCP server, plus network access when the executor enables it.
pub(crate) fn thread_start(
    i: &crate::executor::Invocation<'_>,
    config: &crate::config::ExecutorConfig,
    executable: &std::path::Path,
) -> Value {
    let mut request = json!({"cwd":i.workspace,"model":config.model,"approvalPolicy":"never","sandbox":"workspace-write","config":{
        "mcp_servers.coordination.command":executable,
        "mcp_servers.coordination.args":["--data-dir",i.db.root,"mcp"],
        "mcp_servers.coordination.env.HORDE_WORKER_TOKEN":i.token,
        "mcp_servers.coordination.default_tools_approval_mode":"approve"
    }});
    if config.network {
        request["config"]["sandbox_workspace_write.network_access"] = json!(true);
    }
    request
}

#[cfg(test)]
mod tests {
    use super::thread_start;
    use serde_json::json;

    #[test]
    fn managed_thread_opens_the_sandbox_network_only_when_enabled() {
        let temp = tempfile::tempdir().unwrap();
        let db = crate::store::Store::open(&temp.path().join("data")).unwrap();
        let step: crate::template::Step = serde_json::from_value(json!({"id":"review"})).unwrap();
        let settings = crate::config::Settings::default();
        let invocation = crate::executor::Invocation {
            db: &db,
            task: "task",
            step: "step",
            attempt: "attempt",
            worker: "worker",
            token: "token",
            workspace: temp.path(),
            spec: &step,
            settings: &settings,
            context: json!({}),
        };
        let mut config = settings.executor("codex").unwrap();
        let offline = thread_start(&invocation, &config, std::path::Path::new("/bin/horde"));
        assert_eq!(offline["sandbox"], "workspace-write");
        assert!(
            offline["config"]
                .get("sandbox_workspace_write.network_access")
                .is_none()
        );
        config.network = true;
        let online = thread_start(&invocation, &config, std::path::Path::new("/bin/horde"));
        assert_eq!(online["sandbox"], "workspace-write");
        assert_eq!(
            online["config"]["sandbox_workspace_write.network_access"],
            true
        );
        assert_eq!(
            online["config"]["mcp_servers.coordination.default_tools_approval_mode"],
            "approve"
        );
    }
}
