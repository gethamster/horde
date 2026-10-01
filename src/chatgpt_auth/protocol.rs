use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    rand::{SecureRandom, SystemRandom},
    signature,
};
use sha2::{Digest, Sha256};

pub(super) const ISSUER: &str = "https://auth.openai.com";
pub(super) const RESOURCE: &str = "https://api.openai.com/v1";
pub(super) const TOKEN: &str = "https://auth.openai.com/api/accounts/oauth/token";
fn identity_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(std::time::Duration::from_secs(20))
}
pub(super) fn client() -> Result<reqwest::Client> {
    let builder = identity_client_builder();
    #[cfg(test)]
    let builder = builder.https_only(false);
    Ok(builder.build()?)
}
pub(super) fn random() -> Result<String> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("secure randomness unavailable"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
pub(super) fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}
pub(super) fn callback_client(saved: Option<&str>, supplied: Option<&str>) -> Result<String> {
    let value = match saved {
        Some(saved) => {
            ensure!(
                supplied.is_none_or(|v| v == saved),
                "callback changed selected registration"
            );
            saved
        }
        None => supplied.context("registration incomplete: issued client ID missing")?,
    };
    ensure!(
        value.starts_with("oaiapp_")
            && value.len() <= 256
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
        "invalid issued client ID"
    );
    Ok(value.into())
}
pub(super) async fn get_json(http: &reqwest::Client, url: &str) -> Result<Value> {
    let response = http
        .get(service_url(url))
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("ChatGPT identity service unavailable"))?;
    ensure!(
        response.status().is_success(),
        "ChatGPT identity service unavailable"
    );
    limited_json(response).await
}
async fn limited_json(response: reqwest::Response) -> Result<Value> {
    use futures_util::StreamExt;
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| anyhow::anyhow!("ChatGPT response interrupted"))?;
        ensure!(
            bytes.len() + chunk.len() <= 128 * 1024,
            "ChatGPT response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid ChatGPT service response"))
}
#[derive(Debug)]
pub(super) struct SessionRevoked;
impl std::fmt::Display for SessionRevoked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChatGPT renewable session expired or revoked; sign in required")
    }
}
impl std::error::Error for SessionRevoked {}
pub(super) fn trusted_endpoint(value: &Value, key: &str) -> Result<String> {
    let raw = value[key]
        .as_str()
        .context("identity service endpoint missing")?;
    let url = reqwest::Url::parse(raw).context("identity service endpoint invalid")?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("auth.openai.com")
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "untrusted identity service endpoint"
    );
    // Only the path comes from discovery. Preserve the fixed HTTPS authority
    // in the request itself as well as enforcing it on the HTTP client.
    Ok(format!("https://auth.openai.com{}", url.path()))
}
pub(super) async fn token(http: &reqwest::Client, form: &[(&str, &str)]) -> Result<Value> {
    let response = http
        .post(service_url(TOKEN))
        .form(form)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("ChatGPT token exchange unavailable"))?;
    let status = response.status();
    let body = limited_json(response).await?;
    if !status.is_success()
        && matches!(
            body["error"].as_str(),
            Some(
                "invalid_grant"
                    | "invalid_refresh_token"
                    | "token_expired"
                    | "refresh_token_expired"
                    | "refresh_token_invalid"
                    | "refresh_token_invalidated"
                    | "refresh_token_reused"
            )
        )
    {
        return Err(SessionRevoked.into());
    }
    ensure!(
        status.is_success(),
        "{}",
        match body["error"].as_str() {
            Some("invalid_grant") => "ChatGPT sign-in required: session or authorization expired",
            Some("access_denied") => "ChatGPT permission denied",
            _ => "ChatGPT token exchange failed",
        }
    );
    Ok(body)
}
pub(super) fn validate_jwt(
    jwt: &str,
    jwks: &Value,
    client_id: &str,
    nonce: Option<&str>,
    subject: Option<&str>,
) -> Result<Value> {
    ensure!(jwt.len() <= 32 * 1024, "ID token too large");
    let parts: Vec<_> = jwt.split('.').collect();
    ensure!(parts.len() == 3, "invalid ID token");
    let decode = |value: &str| {
        URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| anyhow::anyhow!("invalid ID token encoding"))
    };
    let header: Value = serde_json::from_slice(&decode(parts[0])?)
        .map_err(|_| anyhow::anyhow!("invalid ID token header"))?;
    ensure!(header["alg"] == "RS256", "unsupported ID token signature");
    let kid = header["kid"].as_str().context("ID token key missing")?;
    let keys = jwks["keys"].as_array().context("identity keys missing")?;
    let key = keys
        .iter()
        .find(|k| {
            k["kid"] == kid
                && k["kty"] == "RSA"
                && k["use"].as_str().is_none_or(|v| v == "sig")
                && k["alg"].as_str().is_none_or(|v| v == "RS256")
        })
        .context("ID token key unavailable")?;
    let n = decode(key["n"].as_str().context("RSA modulus missing")?)?;
    let e = decode(key["e"].as_str().context("RSA exponent missing")?)?;
    signature::RsaPublicKeyComponents { n: &n, e: &e }
        .verify(
            &signature::RSA_PKCS1_2048_8192_SHA256,
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &decode(parts[2])?,
        )
        .map_err(|_| anyhow::anyhow!("ID token signature rejected"))?;
    let claims: Value = serde_json::from_slice(&decode(parts[1])?)
        .map_err(|_| anyhow::anyhow!("invalid ID token claims"))?;
    ensure!(claims["iss"] == ISSUER, "ID token issuer mismatch");
    let audience = &claims["aud"];
    ensure!(
        audience == client_id
            || audience
                .as_array()
                .is_some_and(|v| v.iter().any(|a| a == client_id)),
        "ID token audience mismatch"
    );
    ensure!(
        claims["azp"]
            .as_str()
            .is_none_or(|party| party == client_id),
        "ID token authorized party mismatch"
    );
    if audience.as_array().is_some_and(|v| v.len() > 1) {
        ensure!(
            claims["azp"] == client_id,
            "ID token authorized party mismatch"
        );
    }
    ensure!(
        claims["exp"].as_i64().is_some_and(|t| t > now()),
        "ID token expired"
    );
    ensure!(
        claims["nbf"].as_i64().is_none_or(|t| t <= now()),
        "ID token not yet valid"
    );
    if let Some(nonce) = nonce {
        ensure!(claims["nonce"] == nonce, "ID token nonce mismatch");
    }
    let actual = claims["sub"]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("ID token identity missing")?;
    ensure!(
        subject.is_none_or(|s| s == actual),
        "ChatGPT identity does not match selected account"
    );
    Ok(claims)
}
pub(super) async fn identity(
    http: &reqwest::Client,
    jwt: &str,
    client_id: &str,
    nonce: Option<&str>,
    subject: Option<&str>,
) -> Result<Value> {
    let discovery = get_json(
        http,
        "https://auth.openai.com/.well-known/openid-configuration",
    )
    .await?;
    ensure!(
        discovery["issuer"] == ISSUER,
        "invalid identity discovery issuer"
    );
    let endpoint = trusted_endpoint(&discovery, "jwks_uri")?;
    let keys = get_json(http, &endpoint).await?;
    validate_jwt(jwt, &keys, client_id, nonce, subject)
}

#[cfg(test)]
thread_local! {pub(super) static TEST_SERVICE:std::cell::RefCell<Option<String>>=const {std::cell::RefCell::new(None)};}
#[cfg(test)]
pub(super) fn service_url(url: &str) -> String {
    if let Some(base) = TEST_SERVICE.with(|value| value.borrow().clone()) {
        return url.replacen(ISSUER, &base, 1);
    }
    url.to_owned()
}

#[cfg(not(test))]
pub(super) fn service_url(url: &str) -> String {
    url.to_owned()
}

#[cfg(test)]
mod transport_tests {
    use super::*;

    #[tokio::test]
    async fn identity_transport_rejects_cleartext_before_connecting() {
        let error = identity_client_builder()
            .build()
            .unwrap()
            .get("http://127.0.0.1:1/private-session")
            .send()
            .await
            .unwrap_err();
        assert!(error.is_builder(), "cleartext must fail before networking");
    }
}
