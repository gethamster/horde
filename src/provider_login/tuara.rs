//! Tuara inference uses API keys; its account OAuth grants exclude router:invoke.
//! The account introspection endpoint verifies a key without an inference request.
use super::{Control, Session, append, configuration, finish};
use crate::config::{ExecutorConfig, Settings};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::mpsc;

pub(crate) fn origin(config: &ExecutorConfig) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(&config.base_url).context("invalid Tuara endpoint")?;
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    ensure!(
        (url.scheme() == "https" && url.host_str() == Some("tuara.com")
            || loopback && matches!(url.scheme(), "http" | "https"))
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path().trim_end_matches('/') == "/router/v1",
        "Tuara login requires the Tuara /router/v1 endpoint; configure keys for other native endpoints with agent_setup"
    );
    Ok(url)
}

pub(super) async fn supervise(
    session: &Session,
    receiver: &mut mpsc::Receiver<Control>,
) -> Result<()> {
    let origin = origin(&session.config)?;
    let console = origin.join("/app/buy/keys")?;
    append(session, format!("Open {console}, sign in to Tuara, and create or copy an inference API key. Give that key to your agent to submit in this login session. No Tuara CLI is required.\n").as_bytes());
    let deadline = tokio::time::sleep(
        Duration::from_secs(session.timeout).saturating_sub(session.created.elapsed()),
    );
    tokio::pin!(deadline);
    let key = tokio::select! {
        biased;
        _ = &mut deadline => { finish(session, "expired", None); return Ok(()); }
        command = receiver.recv() => match command {
            Some(Control::Input(key)) => key,
            _ => { finish(session, "cancelled", None); return Ok(()); }
        }
    };
    finish(session, "verifying", None);
    let result = tokio::select! {
        biased;
        _ = &mut deadline => { finish(session, "expired", None); return Ok(()); }
        _ = cancelled(receiver) => { finish(session, "cancelled", None); return Ok(()); }
        result = verify(origin.join("/account/api/v1/auth/introspect")?, &key) => result,
    };
    if result.is_err() {
        finish(
            session,
            "failed",
            Some(
                "Tuara could not verify this inference API key. Existing credentials were kept; start a new login to try again.",
            ),
        );
        return Ok(());
    }
    // No secret or untrusted HTTP body is copied into the session output.
    save(session, key).await?;
    finish(
        session,
        "succeeded",
        Some(
            "Tuara inference API key verified and saved. Future invocations use it without a restart; quota remains unknown.",
        ),
    );
    Ok(())
}

async fn cancelled(receiver: &mut mpsc::Receiver<Control>) {
    while let Some(command) = receiver.recv().await {
        if matches!(command, Control::Cancel) {
            return;
        }
    }
}

pub(crate) async fn verify(url: reqwest::Url, key: &str) -> Result<()> {
    verify_identity(url, key, None).await
}

pub(crate) async fn verify_signup(url: reqwest::Url, key: &str, organization: &str) -> Result<()> {
    verify_identity(url, key, Some(organization)).await
}

async fn verify_identity(url: reqwest::Url, key: &str, organization: Option<&str>) -> Result<()> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()?;
    let mut response = client.get(url).bearer_auth(key).send().await?;
    ensure!(
        response.status().is_success(),
        "Tuara credential verification failed"
    );
    const LIMIT: usize = 64 * 1024;
    ensure!(
        response
            .content_length()
            .is_none_or(|size| size <= LIMIT as u64),
        "Tuara credential response too large"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= LIMIT,
            "Tuara credential response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    let body: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        body["data"]["kind"] == "api"
            && body["data"]["scopes"]
                .as_array()
                .is_some_and(|scopes| scopes.iter().any(|scope| scope == "router:invoke")),
        "Tuara key does not grant inference access"
    );
    ensure!(
        organization.is_none_or(|expected| body["data"]["organizationId"] == expected),
        "Tuara key does not belong to the new signup organization"
    );
    Ok(())
}

async fn save(session: &Session, key: String) -> Result<()> {
    save_key(&session.root, &session.provider, &session.config, key).await
}

pub(crate) async fn save_key(
    root: &std::path::Path,
    requested_provider: &str,
    config: &ExecutorConfig,
    key: String,
) -> Result<()> {
    let current = configuration(requested_provider)?;
    ensure!(
        current.kind == config.kind
            && current.auth_mode == config.auth_mode
            && current.base_url == config.base_url
            && current.api_key_env == config.api_key_env,
        "provider configuration changed during login; start a new login"
    );
    let settings = Settings::load_user()?;
    // The 'tuara' preset normally resolves to 'default'. Preserve that provider's
    // model and role settings instead of applying preset defaults a second time.
    let provider = if settings.providers.contains_key(requested_provider) {
        requested_provider.to_owned()
    } else {
        settings
            .providers
            .iter()
            .find(|(_, provider)| {
                provider.kind == current.kind
                    && provider.auth_mode == current.auth_mode
                    && provider.base_url == current.base_url
                    && provider.api_key_env == current.api_key_env
            })
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| requested_provider.to_owned())
    };
    let result = crate::agent_setup::run(
        root,
        &json!({
            "action":"configure_provider", "provider":provider, "credential":key,
            "kind":"tuara", "auth_mode":"api", "base_url":current.base_url,
            "api_key_env":current.api_key_env
        }),
    )
    .await?;
    ensure!(
        result["status"] == "configured",
        "Tuara credential could not be saved"
    );
    Ok(())
}
