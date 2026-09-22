//! Administrative onboarding for the installed Stripe Link command-line wallet.
//!
//! This module deliberately exposes only the device verification URL/code and a
//! Link-hosted details URL. Credentials, payment methods, and card data remain
//! in Link's credential store and browser UI.
use crate::store::Store;
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, sync::mpsc};

/// Pinned after verifying the npm registry's latest stable release on 2026-09-22.
pub const LINK_CLI_VERSION: &str = "0.22.0";
const OUTPUT_LIMIT: usize = 16 * 1024;
const PACKAGE: &str = "@stripe/link-cli";
const INSTALL_MARKER: &str = ".horde-link-lock-sha256";
type Sessions = BTreeMap<String, Arc<Session>>;
static SESSIONS: OnceLock<Mutex<Sessions>> = OnceLock::new();

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Inspect,
    Install {
        request_id: String,
        #[serde(default)]
        timeout_seconds: Option<u64>,
    },
    LoginStart {
        request_id: String,
        #[serde(default)]
        timeout_seconds: Option<u64>,
    },
    Status {
        session_id: String,
    },
    Cancel {
        session_id: String,
    },
    LoginStatus {
        session_id: String,
    },
    LoginCancel {
        session_id: String,
    },
    Details,
}

struct Session {
    id: String,
    root: PathBuf,
    request_id: String,
    operation: Operation,
    timeout: u64,
    created: Instant,
    expires_at: i64,
    control: mpsc::Sender<Control>,
    state: Mutex<State>,
}

#[derive(Clone, Copy, PartialEq)]
enum Operation {
    Install,
    Login,
}
struct State {
    status: &'static str,
    output: Vec<u8>,
    truncated: bool,
    message: Option<&'static str>,
}
enum Control {
    Cancel,
}

fn sessions() -> &'static Mutex<Sessions> {
    SESSIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}
fn terminal(status: &str) -> bool {
    matches!(status, "succeeded" | "failed" | "cancelled" | "expired")
}

/// Resolve Horde's private pinned copy first, then a trusted user-managed CLI.
/// Callers receive a clean command and may append only fixed, validated arguments.
pub fn command() -> Result<tokio::process::Command> {
    let program = resolve_program().context(
        "Link CLI unavailable; run provider_wallet install to install Horde's private pinned copy",
    )?;
    let mut command = tokio::process::Command::from(crate::executor::clean_command(&program));
    command
        .env("NO_COLOR", "1")
        .env("PATH", safe_runtime_path()?)
        .env("TERM", "dumb")
        .env("BROWSER", "/usr/bin/false")
        .env("NO_UPDATE_NOTIFIER", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    Ok(command)
}

fn resolve_program() -> Option<String> {
    let candidate = private_prefix().join("node_modules/.bin/link-cli");
    if verified_private(&private_prefix()) {
        return Some(candidate.to_string_lossy().into_owned());
    }
    find_tool("link-cli").map(|path| path.to_string_lossy().into_owned())
}

fn find_tool(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join(name))
            .find(|candidate| trusted_external(candidate))
            .and_then(|candidate| std::fs::canonicalize(candidate).ok())
    })
}

/// Keep shebangs such as `/usr/bin/env node` on the same trusted runtime that
/// Horde checked, even when the caller's PATH contains an earlier executable.
fn safe_runtime_path() -> Result<OsString> {
    let mut directories = Vec::new();
    if let Some(node) = find_tool("node") {
        directories.push(node.parent().context("invalid Node.js path")?.to_path_buf());
    }
    directories.extend([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
    std::env::join_paths(directories).context("could not build trusted Link CLI PATH")
}

/// A deliberately installed local CLI is allowed only when neither it nor its
/// containing directory can be replaced by another local account.
fn trusted_external(path: &Path) -> bool {
    let Ok(path) = std::fs::canonicalize(path) else {
        return false;
    };
    let Ok(file) = std::fs::metadata(&path) else {
        return false;
    };
    let uid = unsafe { libc::geteuid() };
    let trusted_directory = |directory: &Path| {
        std::fs::metadata(directory).ok().is_some_and(|metadata| {
            metadata.is_dir()
                && (metadata.uid() == uid || metadata.uid() == 0)
                && (metadata.permissions().mode() & 0o022 == 0
                    || metadata.uid() == 0 && metadata.permissions().mode() & 0o1000 != 0)
        })
    };
    file.is_file()
        && file.permissions().mode() & 0o111 != 0
        && (file.uid() == uid || file.uid() == 0)
        && file.permissions().mode() & 0o022 == 0
        && path.ancestors().skip(1).all(trusted_directory)
}

fn lock_digest() -> String {
    hex::encode(Sha256::digest(include_bytes!(
        "../assets/provider-wallet/package-lock.json"
    )))
}

fn verified_private(prefix: &Path) -> bool {
    let marker = prefix.join(INSTALL_MARKER);
    let Ok(metadata) = std::fs::symlink_metadata(&marker) else {
        return false;
    };
    metadata.is_file()
        && !metadata.file_type().is_symlink()
        && std::fs::read_to_string(&marker)
            .ok()
            .is_some_and(|value| value.trim() == lock_digest())
        && executable(&prefix.join("node_modules/.bin/link-cli"), Some(prefix))
}

fn executable(path: &Path, root: Option<&Path>) -> bool {
    let Ok(resolved) = std::fs::canonicalize(path) else {
        return false;
    };
    if let Some(root) = root {
        let Ok(root) = std::fs::canonicalize(root) else {
            return false;
        };
        if !resolved.starts_with(root) {
            return false;
        }
    }
    std::fs::metadata(resolved)
        .ok()
        .is_some_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

fn private_prefix() -> PathBuf {
    crate::branding::config_dir()
        .join("provider-wallet")
        .join(format!("link-cli-{LINK_CLI_VERSION}"))
}

pub fn dispatch(db: &Store, args: &Value) -> Result<Value> {
    crate::fleet_enrollment::admin()?;
    let request: Request = serde_json::from_value(args.clone())
        .map_err(|_| anyhow::anyhow!("invalid provider wallet request"))?;
    match request {
        Request::Inspect => Ok(inspect()),
        Request::Details => details(&db.root),
        Request::Install {
            request_id,
            timeout_seconds,
        } => start(
            &db.root,
            request_id,
            timeout_seconds.unwrap_or(300),
            Operation::Install,
        ),
        Request::LoginStart {
            request_id,
            timeout_seconds,
        } => start(
            &db.root,
            request_id,
            timeout_seconds.unwrap_or(600),
            Operation::Login,
        ),
        Request::Status { session_id } | Request::LoginStatus { session_id } => {
            get_session(&db.root, &session_id).map(|session| report(&session))
        }
        Request::Cancel { session_id } | Request::LoginCancel { session_id } => {
            let session = get_session(&db.root, &session_id)?;
            if !terminal(session.state.lock().unwrap().status) {
                session
                    .control
                    .try_send(Control::Cancel)
                    .context("wallet control unavailable; inspect status")?;
            }
            Ok(report(&session))
        }
    }
}

fn inspect() -> Value {
    let program = resolve_program();
    let node_available = find_tool("node").is_some();
    let npm_available = find_tool("npm").is_some();
    json!({
        "installed":program.is_some(), "source":program.as_ref().map(|p| if p.starts_with(&private_prefix().to_string_lossy().to_string()) { "horde_private" } else { "user_managed" }),
        "version":if program.as_ref().is_some_and(|p| p.starts_with(&private_prefix().to_string_lossy().to_string())) { Value::String(LINK_CLI_VERSION.into()) } else { Value::Null },
        "pinned_package":format!("{PACKAGE}@{LINK_CLI_VERSION}"),
        "node_available":node_available,
        "npm_available":npm_available,
        "next_actions":if program.is_some() { json!([{"kind":"tool","tool":"provider_wallet","arguments":{"action":"login_start","request_id":"stable-request-id"}}]) } else if node_available && npm_available { json!([{"kind":"tool","tool":"provider_wallet","arguments":{"action":"install","request_id":"stable-request-id"}}]) } else { json!([{"kind":"link","url":"https://nodejs.org/en/download","reason":"Install Node.js and npm, then inspect the Link wallet again."}]) }
    })
}

/// Read the minimum readiness information needed to direct an operator to Link.
/// CLI output is treated as private: email, names, addresses, balances, payment
/// method identifiers, and every provider message are deliberately discarded.
fn details(root: &Path) -> Result<Value> {
    // Protocol dispatch can run on Tokio already. Use the same isolated-thread
    // pattern as the long-lived login supervisor so this never nests runtimes.
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let root = root.to_owned();
    std::thread::Builder::new()
        .name("provider-wallet-details".into())
        .spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(anyhow::Error::from)
                .and_then(|runtime| runtime.block_on(details_async(&root)));
            let _ = send.send(result);
        })
        .context("could not start Link wallet readiness check")?;
    receive
        .recv_timeout(Duration::from_secs(46))
        .map_err(|_| anyhow::anyhow!("Link wallet readiness check timed out"))?
}

async fn details_async(root: &Path) -> Result<Value> {
    let auth = wallet_json(root, ["auth", "status", "--format", "json"]).await?;
    let authenticated = json_object(&auth)
        .and_then(|value| value.get("authenticated"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !authenticated {
        return Ok(json!({
            "status":"login_required", "hosted_url":"https://app.link.com/",
            "payment_method_count":Value::Null,
            "next_actions":[{"kind":"tool","tool":"provider_wallet","arguments":{"action":"login_start","request_id":"stable-request-id"}}]
        }));
    }
    let user = wallet_json(root, ["user-info", "retrieve", "--format", "json"]).await?;
    let methods = wallet_json(root, ["payment-methods", "list", "--format", "json"]).await?;
    let count = methods
        .as_array()
        .map(Vec::len)
        .or_else(|| methods.get("data").and_then(Value::as_array).map(Vec::len));
    let user = json_object(&user);
    let needs_verification = user
        .and_then(|value| value.get("agent_wallet_verification_requirement"))
        .and_then(|v| v.get("action_url"))
        .and_then(Value::as_str)
        .filter(|url| valid_link_url(url))
        .is_some();
    let verification_status = user
        .and_then(|value| value.get("agent_wallet_verification_requirement"))
        .and_then(|v| v.get("status"))
        .and_then(Value::as_str)
        .filter(|status| {
            status.len() <= 64
                && status
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        });
    Ok(json!({
        "status": if needs_verification { "verification_required" } else if count == Some(0) { "payment_method_required" } else if count.is_some() { "ready" } else { "details_unknown" },
        "hosted_url":"https://app.link.com/wallet",
        "verification_status": verification_status,
        "payment_method_count": count,
        "next_actions":[{"kind":"link","url":"https://app.link.com/wallet"}]
    }))
}

fn json_object(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    value
        .as_object()
        .or_else(|| value.as_array()?.first()?.as_object())
}

async fn wallet_json<const N: usize>(root: &Path, args: [&str; N]) -> Result<Value> {
    let mut command = command()?;
    command.args(args).stdin(Stdio::null());
    let mut child = command.spawn().context("Link wallet command unavailable")?;
    let pid = child.id().context("wallet readiness pid missing")?;
    let guard = crate::provider_login::process::Guard::record(
        root,
        &format!("wallet-readiness-{pid}"),
        pid,
    )?;
    let stdout = child
        .stdout
        .take()
        .context("wallet readiness output unavailable")?;
    let stderr = child
        .stderr
        .take()
        .context("wallet readiness output unavailable")?;
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let (bytes, _, status) =
            tokio::try_join!(read_bounded(stdout), read_bounded(stderr), async {
                child.wait().await.map_err(anyhow::Error::from)
            })?;
        ensure!(status.success(), "Link wallet readiness check failed");
        serde_json::from_slice::<Value>(&bytes)
            .map_err(|_| anyhow::anyhow!("Link wallet readiness response was invalid"))
    })
    .await;
    guard.stop();
    let _ = child.wait().await;
    result.map_err(|_| anyhow::anyhow!("Link wallet readiness check timed out"))?
}

fn get_session(root: &Path, id: &str) -> Result<Arc<Session>> {
    sessions()
        .lock()
        .unwrap()
        .get(id)
        .filter(|s| s.root == root)
        .cloned()
        .context("wallet session unavailable or daemon restarted; start a new operation")
}

fn start(root: &Path, request_id: String, timeout: u64, operation: Operation) -> Result<Value> {
    if operation == Operation::Install {
        ensure!(
            find_tool("node").is_some() && find_tool("npm").is_some(),
            "Node.js and npm are required to install Link; install them, then retry provider_wallet install"
        );
    }
    ensure!(
        !request_id.is_empty()
            && request_id.len() <= 128
            && request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')),
        "invalid request_id"
    );
    ensure!(
        (1..=1800).contains(&timeout),
        "timeout_seconds must be 1 through 1800"
    );
    let mut registry = sessions().lock().unwrap();
    registry.retain(|_, s| {
        s.created.elapsed().as_secs() < s.timeout + 3600
            || !terminal(s.state.lock().unwrap().status)
    });
    if let Some(existing) = registry
        .values()
        .find(|s| s.root == root && s.request_id == request_id)
    {
        ensure!(
            existing.operation == operation && existing.timeout == timeout,
            "request_id already used for a different wallet operation"
        );
        return Ok(report(existing));
    }
    ensure!(registry.len() < 128, "wallet session limit reached");
    let lock = crate::provider_login::process::lock("link")?;
    let (sender, receiver) = mpsc::channel(2);
    let session = Arc::new(Session {
        id: uuid::Uuid::new_v4().to_string(),
        root: root.to_owned(),
        request_id,
        operation,
        timeout,
        created: Instant::now(),
        expires_at: crate::store::now() + timeout as i64,
        control: sender,
        state: Mutex::new(State {
            status: "starting",
            output: vec![],
            truncated: false,
            message: None,
        }),
    });
    registry.insert(session.id.clone(), session.clone());
    let owned = session.clone();
    if std::thread::Builder::new()
        .name("provider-wallet".into())
        .spawn(move || {
            let _lock = lock;
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(anyhow::Error::from)
                .and_then(|runtime| runtime.block_on(supervise(&owned, receiver)));
            if result.is_err() {
                finish(
                    &owned,
                    "failed",
                    Some("Link wallet operation could not finish; start a new operation."),
                );
            }
        })
        .is_err()
    {
        registry.remove(&session.id);
        anyhow::bail!("could not start Link wallet supervisor");
    }
    Ok(report(&session))
}

fn finish(session: &Session, status: &'static str, message: Option<&'static str>) {
    let mut state = session.state.lock().unwrap();
    state.status = status;
    state.message = message;
}
fn append(session: &Session, bytes: &[u8]) {
    let mut state = session.state.lock().unwrap();
    state.output.extend_from_slice(bytes);
    if state.output.len() > OUTPUT_LIMIT {
        let n = state.output.len() - OUTPUT_LIMIT;
        state.output.drain(..n);
        state.truncated = true;
    }
}

async fn supervise(session: &Session, mut receiver: mpsc::Receiver<Control>) -> Result<()> {
    let install_stage = if session.operation == Operation::Install {
        let prefix = private_prefix();
        std::fs::create_dir_all(prefix.parent().context("invalid private Link prefix")?)?;
        Some(prefix.with_file_name(format!(
            ".link-cli-{LINK_CLI_VERSION}.{}.staging",
            uuid::Uuid::new_v4()
        )))
    } else {
        None
    };
    let mut child = if let Some(stage) = install_stage.as_ref() {
        let npm = find_tool("npm").context("npm unavailable; install Node.js and npm")?;
        std::fs::create_dir_all(stage)?;
        std::fs::write(
            stage.join("package.json"),
            include_bytes!("../assets/provider-wallet/package.json"),
        )?;
        std::fs::write(
            stage.join("package-lock.json"),
            include_bytes!("../assets/provider-wallet/package-lock.json"),
        )?;
        let mut cmd = tokio::process::Command::from(crate::executor::clean_command(
            npm.to_str().context("npm path is not UTF-8")?,
        ));
        cmd.current_dir(stage);
        cmd.env("PATH", safe_runtime_path()?);
        cmd.args(["ci", "--no-audit", "--no-fund", "--ignore-scripts"]);
        cmd
    } else {
        let mut cmd = command()?;
        cmd.args([
            "auth",
            "login",
            "--client-name",
            "Horde",
            "--interval",
            "5",
            "--timeout",
            &session.timeout.to_string(),
            "--format",
            "jsonl",
        ]);
        cmd
    };
    child
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = child.spawn().context("Link wallet command unavailable")?;
    let pid = child.id().context("wallet pid missing")?;
    let guard = match crate::provider_login::process::Guard::record(
        &session.root,
        &format!("wallet-{}", session.id),
        pid,
    ) {
        Ok(guard) => guard,
        Err(error) => {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            let _ = child.wait().await;
            return Err(error);
        }
    };
    finish(
        session,
        if session.operation == Operation::Install {
            "installing"
        } else {
            "awaiting_device"
        },
        None,
    );
    let mut stdout = child.stdout.take().context("wallet output unavailable")?;
    let mut stderr = child.stderr.take().context("wallet output unavailable")?;
    let mut out = [0u8; 4096];
    let mut err = [0u8; 4096];
    let mut out_open = true;
    let mut err_open = true;
    let deadline = tokio::time::sleep(Duration::from_secs(session.timeout));
    tokio::pin!(deadline);
    let result: Result<(&str, Option<std::process::ExitStatus>)> = async { Ok(loop { tokio::select! {
        _=&mut deadline => break ("expired",None),
        control=receiver.recv()=> match control { Some(Control::Cancel)|None=>break("cancelled",None) },
        size=stdout.read(&mut out), if out_open=>{let n=size?;out_open=n!=0;append(session,&out[..n]);},
        size=stderr.read(&mut err), if err_open=>{let n=size?;err_open=n!=0;append(session,&err[..n]);},
        status=child.wait()=>break("exited",Some(status?)),
    } }) }.await;
    guard.stop();
    let _ = child.wait().await;
    let (kind, status) = result?;
    if kind != "exited" {
        if let Some(stage) = install_stage.as_ref() {
            let _ = std::fs::remove_dir_all(stage);
        }
        finish(session, kind, None);
        return Ok(());
    }
    ensure!(
        status.is_some_and(|s| s.success()),
        "Link wallet command failed"
    );
    if let Some(stage) = install_stage.as_ref() {
        publish_install(stage)?;
    }
    if session.operation == Operation::Login {
        ensure!(
            verify_login(session).await?,
            "Link wallet did not confirm authentication"
        );
    }
    finish(
        session,
        "succeeded",
        Some(if session.operation == Operation::Install {
            "Installed the pinned Link CLI privately for Horde. Start login to connect your wallet."
        } else {
            "Link wallet is connected. Use details to enter or update payment details in Link."
        }),
    );
    Ok(())
}

/// Publish only a complete npm prefix. Interrupted downloads remain in a
/// uniquely named staging directory and are never selected by `resolve_program`.
fn publish_install(stage: &Path) -> Result<()> {
    let installed_executable = stage.join("node_modules/.bin/link-cli");
    ensure!(
        executable(&installed_executable, Some(stage)),
        "Link CLI installation was incomplete"
    );
    let marker = stage.join(INSTALL_MARKER);
    std::fs::write(&marker, format!("{}\n", lock_digest()))?;
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600))?;
    std::fs::File::open(&marker)?.sync_all()?;
    std::fs::File::open(stage)?.sync_all()?;
    let prefix = private_prefix();
    if prefix.exists() {
        if verified_private(&prefix) {
            let _ = std::fs::remove_dir_all(stage);
            return Ok(());
        }
        ensure!(
            !std::fs::symlink_metadata(&prefix)?.file_type().is_symlink(),
            "private Link CLI prefix cannot be a symlink"
        );
        // A prior interrupted direct npm install is not a usable release. It
        // is removed only after this complete staging prefix is validated.
        std::fs::remove_dir_all(&prefix)?;
    }
    std::fs::rename(stage, &prefix).context("publish private Link CLI installation")?;
    if let Some(parent) = prefix.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn report(session: &Session) -> Value {
    let state = session.state.lock().unwrap();
    let output = if session.operation == Operation::Login {
        public_login_output(&state.output)
    } else {
        Value::Null
    };
    let action = if session.operation == Operation::Install {
        "install"
    } else {
        "login"
    };
    let next_actions = if state.status == "succeeded" && session.operation == Operation::Install {
        json!([{"kind":"tool","tool":"provider_wallet","arguments":{"action":"login_start","request_id":"stable-request-id"}}])
    } else if state.status == "succeeded" {
        json!([{"kind":"tool","tool":"provider_wallet","arguments":{"action":"details"}}])
    } else if terminal(state.status) {
        json!([])
    } else {
        json!([{"kind":"tool","tool":"provider_wallet","arguments":{"action":"login_status","session_id":session.id}}])
    };
    json!({"session_id":session.id,"operation":action,"status":state.status,"expires_at":session.expires_at,"output":output,"output_truncated":state.truncated,"message":state.message,"next_actions":next_actions})
}

/// Keep only device-code instructions from the provider. Raw CLI output can contain token-like data.
fn public_login_output(bytes: &[u8]) -> Value {
    let mut url = None;
    let mut phrase = None;
    for frame in bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
    {
        visit_login_frame(&frame, &mut url, &mut phrase);
    }
    if url.is_none()
        && phrase.is_none()
        && let Ok(frame) = serde_json::from_slice::<Value>(bytes)
    {
        visit_login_frame(&frame, &mut url, &mut phrase);
    }
    if url.is_none() && phrase.is_none() {
        return json!({"message":"Complete the Link device approval in the Link app, then poll status."});
    }
    let code = phrase;
    json!({"verification_url":url,"verification_code":code})
}
fn visit_login_frame(value: &Value, url: &mut Option<String>, phrase: &mut Option<String>) {
    match value {
        Value::Array(rows) => {
            for row in rows {
                visit_login_frame(row, url, phrase)
            }
        }
        Value::Object(row) => {
            if let Some(data) = row.get("data") {
                visit_login_frame(data, url, phrase);
            }
            if let Some(value) = row
                .get("verification_url")
                .or_else(|| row.get("verification_uri"))
                .and_then(Value::as_str)
                .filter(|value| valid_link_url(value))
            {
                *url = Some(value.to_owned());
            }
            if let Some(value) = row
                .get("phrase")
                .or_else(|| row.get("user_code"))
                .and_then(Value::as_str)
                .filter(valid_phrase)
            {
                *phrase = Some(value.to_owned());
            }
        }
        _ => (),
    }
}
fn valid_link_url(value: &str) -> bool {
    reqwest::Url::parse(value).ok().is_some_and(|url| {
        url.scheme() == "https"
            && matches!(url.host_str(), Some("app.link.com" | "link.com"))
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
    })
}
fn valid_phrase(value: &&str) -> bool {
    value.len() <= 128
        && !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == ' ')
}

async fn verify_login(session: &Session) -> Result<bool> {
    let mut command = command()?;
    command.args(["auth", "status", "--format", "json"]);
    let mut child = command.spawn()?;
    let guard = crate::provider_login::process::Guard::record(
        &session.root,
        &format!("wallet-{}-verify", session.id),
        child.id().context("wallet verification pid missing")?,
    )?;
    let stdout = child
        .stdout
        .take()
        .context("wallet verification output unavailable")?;
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let bytes = read_bounded(stdout).await?;
        let status = child.wait().await?;
        Ok::<_, anyhow::Error>((bytes, status))
    })
    .await;
    guard.stop();
    let _ = child.wait().await;
    let (bytes, status) =
        result.map_err(|_| anyhow::anyhow!("Link authentication verification timed out"))??;
    ensure!(status.success(), "Link authentication verification failed");
    Ok(login_authenticated(&bytes))
}
fn login_authenticated(bytes: &[u8]) -> bool {
    json_frames(bytes).iter().any(|frame| match frame {
        Value::Array(rows) => rows.iter().any(|row| row["authenticated"] == true),
        value => value["authenticated"] == true,
    })
}
async fn read_bounded(reader: impl tokio::io::AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    ensure!(
        bytes.len() <= OUTPUT_LIMIT,
        "Link wallet output limit exceeded"
    );
    Ok(bytes)
}
fn json_frames(bytes: &[u8]) -> Vec<Value> {
    let mut values = bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect::<Vec<_>>();
    if values.is_empty()
        && let Ok(value) = serde_json::from_slice(bytes)
    {
        values.push(value);
    }
    values
}

pub fn shutdown(root: &Path) {
    let active = sessions()
        .lock()
        .unwrap()
        .values()
        .filter(|s| s.root == root && !terminal(s.state.lock().unwrap().status))
        .cloned()
        .collect::<Vec<_>>();
    let deadline = Instant::now() + Duration::from_secs(5);
    while active
        .iter()
        .any(|s| !terminal(s.state.lock().unwrap().status))
        && Instant::now() < deadline
    {
        for session in &active {
            let _ = session.control.try_send(Control::Cancel);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
pub fn recover(root: &Path) -> Result<()> {
    crate::provider_login::recover(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn login_output_whitelists_only_device_instructions() {
        let output=public_login_output(br#"{"verification_url":"https://app.link.com/device","user_code":"ABCD-1234","access_token":"secret"}"#);
        assert_eq!(output["verification_url"], "https://app.link.com/device");
        assert_eq!(output["verification_code"], "ABCD-1234");
        assert!(!output.to_string().contains("secret"));
    }
    #[test]
    fn login_output_reads_link_jsonl_data_without_exposing_other_fields() {
        let output = public_login_output(br#"{"type":"data","data":{"verification_url":"https://app.link.com/device","phrase":"ABCD-1234","access_token":"secret"}}
{"type":"done","ok":true}"#);
        assert_eq!(output["verification_url"], "https://app.link.com/device");
        assert_eq!(output["verification_code"], "ABCD-1234");
        assert!(!output.to_string().contains("secret"));
    }
    #[test]
    fn login_verification_accepts_link_json_array() {
        assert!(login_authenticated(br#"[{"authenticated":true}]"#));
        assert!(login_authenticated(b"{\"authenticated\":true}\n"));
        assert!(!login_authenticated(br#"[{"authenticated":false}]"#));
    }
    #[test]
    fn login_output_does_not_relay_text_or_untrusted_urls() {
        assert!(
            public_login_output(b"token=secret")
                .to_string()
                .contains("Complete the Link")
        );
        let output = public_login_output(
            br#"{"verification_url":"https://app.link.com.evil.invalid/a","user_code":"bad\tcode"}"#,
        );
        assert!(output["verification_url"].is_null());
        assert!(output["verification_code"].is_null());
    }
    #[test]
    fn resolver_rejects_group_writable_path_executables() {
        let root = tempfile::tempdir().unwrap();
        let link = root.path().join("link-cli");
        std::fs::write(&link, "#!/bin/sh").unwrap();
        std::fs::set_permissions(&link, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let old = std::env::var_os("PATH");
        unsafe { std::env::set_var("PATH", root.path()) };
        assert!(resolve_program().is_none());
        match old {
            Some(value) => unsafe { std::env::set_var("PATH", value) },
            None => unsafe { std::env::remove_var("PATH") },
        }
    }

    #[test]
    fn bundled_npm_lock_pins_link_and_dependency_integrity() {
        let lock = include_str!("../assets/provider-wallet/package-lock.json");
        assert!(lock.contains("\"@stripe/link-cli\": \"0.22.0\""));
        assert!(lock.contains("\"node_modules/@stripe/link-cli\""));
        assert!(lock.contains("\"integrity\": \"sha512-"));
        assert!(!lock.contains("\"@stripe/link-cli\": \"^"));
    }

    #[test]
    fn private_install_requires_the_bundled_lock_digest() {
        let root = tempfile::tempdir().unwrap();
        let prefix = root.path();
        let bin = prefix.join("node_modules/.bin");
        std::fs::create_dir_all(&bin).unwrap();
        let cli = bin.join("link-cli");
        std::fs::write(&cli, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!verified_private(prefix));
        std::fs::write(prefix.join(INSTALL_MARKER), "unverified\n").unwrap();
        assert!(!verified_private(prefix));
        std::fs::write(prefix.join(INSTALL_MARKER), format!("{}\n", lock_digest())).unwrap();
        assert!(verified_private(prefix));
    }
}
