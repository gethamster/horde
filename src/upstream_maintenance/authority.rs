use super::*;
use std::{net::IpAddr, time::Duration};
pub(super) fn validate_url(policy: &Policy) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(&policy.authority_url)?;
    let loopback = url
        .host_str()
        .and_then(|h| h.parse::<IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback());
    ensure!(
        url.scheme() == "https"
            || url.scheme() == "http" && (loopback || policy.allow_private_http),
        "maintenance authority requires HTTPS or operator-approved private HTTP"
    );
    ensure!(
        url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "invalid maintenance authority endpoint"
    );
    ensure!(
        !policy.authority_token_env.is_empty()
            && policy
                .authority_token_env
                .bytes()
                .enumerate()
                .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || i > 0 && b.is_ascii_digit()),
        "invalid maintenance authority credential reference"
    );
    Ok(url)
}
pub(super) fn validate_response(policy: &Policy, binding: &Binding, value: &Value) -> Result<()> {
    ensure!(
        value["active"] == true
            && value["collected_until"] == binding.collected_until
            && value["upstreams"] == serde_json::to_value(&binding.upstreams)?
            && value["invocation_id"] == binding.invocation_id
            && value["occurrence_id"] == binding.occurrence_id
            && value["policy_id"] == policy.policy_id
            && value["policy_revision"] == policy.policy_revision
            && value["automation_grant_id"] == policy.automation_grant_id
            && value["routine_id"] == policy.routine_id
            && value["expires_at"]
                .as_i64()
                .is_some_and(|expiry| expiry > now() && expiry == policy.expires_at),
        "owning maintenance authority is inactive or changed"
    );
    Ok(())
}
pub(super) async fn online(policy: &Policy, binding: &Binding) -> Result<()> {
    let url = validate_url(policy)?;
    let token = std::env::var(&policy.authority_token_env)
        .map_err(|_| anyhow::anyhow!("maintenance authority credential unavailable"))?;
    ensure!(
        token.len() >= 32,
        "maintenance authority credential is invalid"
    );
    let result = fetch(url, &token, binding).await;
    let value =
        result.map_err(|_| anyhow::anyhow!("owning maintenance authority is unavailable"))?;
    validate_response(policy, binding, &value)
}
pub(super) async fn fetch(url: reqwest::Url, token: &str, binding: &Binding) -> Result<Value> {
    let mut response = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()?
        .get(url)
        .bearer_auth(token)
        .header("x-upstream-maintenance-invocation", &binding.invocation_id)
        .header("x-upstream-maintenance-occurrence", &binding.occurrence_id)
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "maintenance authority rejected request"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= 16384,
            "maintenance authority reply exceeds limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}
pub(super) fn online_sync(policy: &Policy, binding: &Binding) -> Result<()> {
    let policy = policy.clone();
    let binding = binding.clone();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(online(&policy, &binding))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("maintenance authority check interrupted"))?
}
