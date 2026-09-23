//! Model-independent, agent-mediated provider login and API-key handoffs.
//! Sessions live in the daemon, not the short-lived setup request runtime.
use crate::{
    config::{ExecutorConfig, Settings},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};

pub(crate) mod process;
pub(crate) mod tuara;

const OUTPUT_LIMIT: usize = 16 * 1024;
type Sessions = BTreeMap<String, Arc<Session>>;
static SESSIONS: OnceLock<Mutex<Sessions>> = OnceLock::new();

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    action: String,
    provider: Option<String>,
    request_id: Option<String>,
    session_id: Option<String>,
    input: Option<String>,
    timeout_seconds: Option<u64>,
}

struct Session {
    id: String,
    root: PathBuf,
    provider: String,
    request_id: String,
    config: ExecutorConfig,
    timeout: u64,
    created: Instant,
    expires_at: i64,
    control: mpsc::Sender<Control>,
    state: Mutex<State>,
}

struct State {
    status: &'static str,
    output: Vec<u8>,
    truncated: bool,
    submitted: Vec<String>,
    message: Option<&'static str>,
}

enum Control {
    Input(String),
    Cancel,
}

fn sessions() -> &'static Mutex<Sessions> {
    SESSIONS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn terminal(status: &str) -> bool {
    matches!(status, "succeeded" | "failed" | "cancelled" | "expired")
}

pub fn dispatch(db: &Store, args: &Value) -> Result<Value> {
    crate::fleet_enrollment::admin()?;
    let request: Request = serde_json::from_value(args.clone())
        .map_err(|_| anyhow::anyhow!("invalid provider login request"))?;
    if request.action == "start" {
        ensure!(
            request.input.is_none() && request.session_id.is_none(),
            "start accepts provider, request_id and timeout_seconds only"
        );
        return start(&db.root, request);
    }
    ensure!(
        request.provider.is_none()
            && request.request_id.is_none()
            && request.timeout_seconds.is_none(),
        "session actions accept session_id and optional login input only"
    );
    ensure!(
        request.action == "submit" || request.input.is_none(),
        "input is only accepted by submit"
    );
    let id = request.session_id.context("session_id required")?;
    let session = sessions()
        .lock()
        .unwrap()
        .get(&id)
        .filter(|session| session.root == db.root)
        .cloned()
        .context("login session unavailable or daemon restarted; start a new login")?;
    match request.action.as_str() {
        "status" => (),
        "submit" => submit(&session, request.input.context("login input required")?)?,
        "cancel" => {
            if !terminal(session.state.lock().unwrap().status) {
                session
                    .control
                    .try_send(Control::Cancel)
                    .context("login control unavailable; inspect status")?;
            }
        }
        _ => anyhow::bail!("unknown provider login action"),
    }
    Ok(report(&session))
}

pub(crate) fn configuration(provider: &str) -> Result<ExecutorConfig> {
    resolve_configuration(&Settings::load_user()?, provider)
}

fn resolve_configuration(settings: &Settings, provider: &str) -> Result<ExecutorConfig> {
    ensure!(
        !provider.is_empty() && provider.len() <= 48,
        "invalid provider name"
    );
    let config = settings
        .provider(provider)
        .or_else(|| {
            crate::provisioning::preset(provider).map(|preset| {
                if provider == "tuara"
                    && let Some((name, _)) = settings.providers.iter().find(|(_, configured)| {
                        configured.kind == preset.kind
                            && configured.auth_mode == preset.auth_mode
                            && configured.base_url.trim_end_matches('/') == preset.base_url
                    })
                {
                    return settings.provider(name).expect("configured provider");
                }
                ExecutorConfig {
                    kind: preset.kind.into(),
                    auth_mode: preset.auth_mode.into(),
                    base_url: preset.base_url.into(),
                    api_key_env: preset.api_key_env.into(),
                    ..Default::default()
                }
            })
        })
        .context("provider not configured; configure it with agent_setup first")?;
    if config.kind == "tuara" && config.auth_mode == "api" {
        tuara::origin(&config)?;
        return Ok(config);
    }
    ensure!(
        config.auth_mode == "login" && ["codex", "claude"].contains(&config.kind.as_str()),
        "provider login supports Codex and Claude subscriptions and Tuara API key handoff; supply other API keys through agent_setup configure_provider"
    );
    Ok(config)
}

fn start(root: &Path, request: Request) -> Result<Value> {
    let provider = request.provider.context("provider required")?;
    let request_id = request
        .request_id
        .context("request_id required for retry-safe login")?;
    ensure!(
        !request_id.is_empty()
            && request_id.len() <= 128
            && !request_id.chars().any(char::is_control),
        "invalid request_id"
    );
    let timeout = request.timeout_seconds.unwrap_or(600);
    ensure!(
        (1..=1800).contains(&timeout),
        "timeout_seconds must be 1 through 1800"
    );
    let mut registry = sessions().lock().unwrap();
    // Keep completed receipts for one hour beyond their deadline for retry deduplication.
    registry.retain(|_, session| {
        session.created.elapsed().as_secs() < session.timeout + 3600
            || !terminal(session.state.lock().unwrap().status)
    });
    if let Some(existing) = registry
        .values()
        .find(|s| s.root == root && s.request_id == request_id)
    {
        ensure!(
            existing.provider == provider && existing.timeout == timeout,
            "request_id already used for a different login"
        );
        return Ok(report(existing));
    }
    let config = configuration(&provider)?;
    if let Some(account) = config.account.as_deref() {
        let db = Store::open(root)?;
        let managed: bool = db.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?)",
            [account],
            |row| row.get(0),
        )?;
        ensure!(
            !managed,
            "provider selects a managed account; renew it with account_credential_set instead of shared CLI login"
        );
    }
    ensure!(
        !registry
            .values()
            .any(|s| s.config.kind == config.kind && !terminal(s.state.lock().unwrap().status)),
        "a login for this shared provider credential store is already active; finish or cancel it first"
    );
    ensure!(
        registry.len() < 128,
        "login session limit reached; wait for completed sessions to expire"
    );
    // Different data roots can still share the same installed CLI credential store.
    let login_lock = process::lock(&config.kind)?;
    let (sender, receiver) = mpsc::channel(4);
    let session = Arc::new(Session {
        id: uuid::Uuid::new_v4().to_string(),
        root: root.to_owned(),
        provider,
        request_id,
        config,
        timeout,
        created: Instant::now(),
        expires_at: crate::store::now() + timeout as i64,
        control: sender,
        state: Mutex::new(State {
            status: "starting",
            output: vec![],
            truncated: false,
            submitted: vec![],
            message: None,
        }),
    });
    registry.insert(session.id.clone(), session.clone());
    let owned = session.clone();
    if std::thread::Builder::new().name("provider-login".into()).spawn(move || {
        let _login_lock = login_lock;
        let result = tokio::runtime::Builder::new_current_thread().enable_all().build()
            .map_err(anyhow::Error::from)
            .and_then(|runtime| runtime.block_on(supervise(&owned, receiver)));
        if result.is_err() {
            finish(&owned, "failed", Some("Provider login could not finish; inspect its output and start a new login."));
        }
    }).is_err() {
        registry.remove(&session.id);
        anyhow::bail!("could not start provider login supervisor");
    }
    Ok(report(&session))
}

fn submit(session: &Session, input: String) -> Result<()> {
    ensure!(
        !input.trim().is_empty() && input.len() <= 4096 && !input.chars().any(char::is_control),
        "login input must be one nonempty line of at most 4096 bytes"
    );
    let mut state = session.state.lock().unwrap();
    ensure!(
        state.status == "awaiting_user",
        "login is not awaiting input; inspect status"
    );
    // Retrying an identical one-time response must not feed the CLI twice.
    if !state.submitted.contains(&input) {
        ensure!(
            state.submitted.len() < 8,
            "too many login responses; start a new login"
        );
        session
            .control
            .try_send(Control::Input(input.clone()))
            .context("login input queue unavailable; inspect status")?;
        state.submitted.push(input);
    }
    Ok(())
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
        let excess = state.output.len() - OUTPUT_LIMIT;
        state.output.drain(..excess);
        state.truncated = true;
    }
    if state.status == "starting" {
        state.status = "awaiting_user";
    }
}

fn report(session: &Session) -> Value {
    let state = session.state.lock().unwrap();
    let mut output = process::plain_text(&state.output);
    // Tuara output is authored here and never contains the submitted key.
    // Short key prefixes must not corrupt the fixed console URL or instructions.
    if session.config.kind != "tuara" {
        output = redact_input(output, &state.submitted, state.truncated);
    }
    // Lossy decoding or redaction may expand bytes; keep the output contract bounded.
    while output.len() > OUTPUT_LIMIT {
        output.remove(0);
    }
    json!({
        "session_id":session.id, "provider":session.provider, "status":state.status,
        "expires_at":session.expires_at, "output":output, "output_truncated":state.truncated,
        "message":state.message, "provider_authentication":if state.status == "succeeded" {"verified"} else {"not_verified"},
        "capacity":"unknown",
        "method":if session.config.kind == "tuara" { "api_key" } else { "cli_login" },
        "scope":if session.config.kind == "tuara" { "provider API credential on this runtime" } else { "shared CLI login on this runtime" },
        "credential_activation":if session.config.kind == "tuara" && state.status == "succeeded" { json!("next_invocation") } else { Value::Null },
        "next_actions":if state.status == "succeeded" {
            if session.config.kind == "tuara" { json!([{"kind":"tool","tool":"account_status","arguments":{}}]) } else {
            json!([{"kind":"tool","tool":"agent_setup","arguments":{"action":"configure_provider","provider":session.provider}},
                   {"kind":"tool","tool":"account_status","arguments":{}}]) }
        } else if terminal(state.status) { json!([]) } else {
            json!([{"kind":"tool","tool":"provider_login","arguments":{"action":"status","session_id":session.id}}])
        }
    })
}

fn redact_input(mut output: String, submitted: &[String], truncated: bool) -> String {
    for secret in submitted {
        output = output.replace(secret, "[redacted]");
        // A poll can arrive between two writes of an echoed one-time code.
        for boundary in secret
            .char_indices()
            .map(|(index, _)| index)
            .filter(|index| *index > 0)
            .rev()
        {
            if output.ends_with(&secret[..boundary]) {
                output.truncate(output.len() - boundary);
                output.push_str("[redacted]");
                break;
            }
        }
        if truncated {
            for boundary in secret
                .char_indices()
                .map(|(index, _)| index)
                .filter(|index| *index > 0)
            {
                if output.starts_with(&secret[boundary..]) {
                    output.replace_range(..secret.len() - boundary, "[redacted]");
                    break;
                }
            }
        }
    }
    output
}

async fn supervise(session: &Session, mut receiver: mpsc::Receiver<Control>) -> Result<()> {
    if session.config.kind == "tuara" {
        return tuara::supervise(session, &mut receiver).await;
    }
    let mut command = process::command(&session.config);
    if session.config.kind == "codex" {
        command.args(["login", "--device-auth"]);
    } else {
        command.args(["auth", "login"]);
    }
    let mut child = command.spawn().context("provider CLI unavailable")?;
    let guard = match process::Guard::record(
        &session.root,
        &session.id,
        child.id().context("login pid missing")?,
    ) {
        Ok(guard) => guard,
        Err(error) => {
            let _ = child.kill().await;
            return Err(error);
        }
    };
    let mut input = child.stdin.take().context("login input unavailable")?;
    let mut stdout = child.stdout.take().context("login output unavailable")?;
    let mut stderr = child
        .stderr
        .take()
        .context("login error output unavailable")?;
    let deadline = tokio::time::sleep(
        Duration::from_secs(session.timeout).saturating_sub(session.created.elapsed()),
    );
    tokio::pin!(deadline);
    let (mut out, mut err) = ([0u8; 4096], [0u8; 4096]);
    let (mut stdout_open, mut stderr_open) = (true, true);
    let result: Result<_> = async { Ok(loop {
        tokio::select! {
            _ = &mut deadline => { break ("expired", None); }
            control = receiver.recv() => match control {
                Some(Control::Input(value)) => {
                    tokio::time::timeout(Duration::from_secs(2), input.write_all(format!("{value}\n").as_bytes())).await??;
                }
                _ => break ("cancelled", None),
            },
            size = stdout.read(&mut out), if stdout_open => {
                let size = size?; stdout_open = size != 0; append(session, &out[..size]);
            }
            size = stderr.read(&mut err), if stderr_open => {
                let size = size?; stderr_open = size != 0; append(session, &err[..size]);
            }
            status = child.wait() => { break ("exited", Some(status?)); }
        }
    }) }.await;
    // Stop descendants as well as the CLI, then reap before reporting termination.
    guard.stop();
    let _ = child.wait().await;
    let exit = result?;
    let _ = tokio::time::timeout(Duration::from_millis(100), async {
        loop {
            let size = stdout.read(&mut out).await?;
            if size == 0 {
                break;
            }
            append(session, &out[..size]);
        }
        loop {
            let size = stderr.read(&mut err).await?;
            if size == 0 {
                break;
            }
            append(session, &err[..size]);
        }
        Ok::<_, std::io::Error>(())
    })
    .await;
    if exit.0 != "exited" {
        finish(session, exit.0, None);
        return Ok(());
    }
    ensure!(
        exit.1.is_some_and(|status| status.success()),
        "provider login failed"
    );
    finish(session, "verifying", None);
    let verified = process::verify(
        &session.config,
        &session.root,
        &session.id,
        Duration::from_secs(session.timeout).saturating_sub(session.created.elapsed()),
        &mut receiver,
    )
    .await?;
    if verified != "succeeded" {
        finish(session, verified, None);
        return Ok(());
    }
    crate::capacity::credentials_changed(&Store::open(&session.root)?, &session.config)?;
    finish(
        session,
        "succeeded",
        Some(
            "Signed in. Future invocations use this shared CLI login; existing tasks keep their saved provider selection.",
        ),
    );
    Ok(())
}

/// Graceful shutdown cancels pending handoffs. A new daemon never replays login input.
pub fn shutdown(root: &Path) {
    let active: Vec<_> = sessions()
        .lock()
        .unwrap()
        .values()
        .filter(|s| s.root == root && !terminal(s.state.lock().unwrap().status))
        .cloned()
        .collect();
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
    process::recover(root)
}

#[cfg(test)]
mod tests {
    use super::redact_input;

    #[test]
    fn tuara_preset_uses_the_existing_provider_model_and_credential_variable() {
        let defaults = crate::config::Settings::default();
        let custom = crate::config::Provider {
            model: Some("custom/model".into()),
            api_key_env: "CUSTOM_TUARA_KEY".into(),
            base_url: "https://tuara.com/router/v1/".into(),
            ..defaults.providers["default"].clone()
        };
        let settings = crate::config::Settings {
            providers: [("default".into(), custom)].into_iter().collect(),
            ..defaults
        };
        let resolved = super::resolve_configuration(&settings, "tuara").unwrap();
        assert_eq!(resolved.api_key_env, "CUSTOM_TUARA_KEY");
        assert_eq!(resolved.model.as_deref(), Some("custom/model"));
        assert_eq!(resolved.base_url, "https://tuara.com/router/v1/");
    }

    #[test]
    fn echoed_input_is_redacted_across_poll_and_truncation_boundaries() {
        let input = vec!["secret-code#state".to_owned()];
        assert_eq!(
            redact_input("Prompt: secret-".into(), &input, false),
            "Prompt: [redacted]"
        );
        assert_eq!(
            redact_input("code#state done".into(), &input, true),
            "[redacted] done"
        );
        assert_eq!(
            redact_input("secret-code#state done".into(), &input, false),
            "[redacted] done"
        );
        assert_eq!(
            redact_input("Login ready".into(), &input, false),
            "Login ready"
        );
        let unicode = vec!["écho-code".into()];
        assert_eq!(redact_input("écho".into(), &unicode, false), "[redacted]");
    }
}
