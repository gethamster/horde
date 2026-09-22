//! Bounded, non-retrying Tuara machine-payment requests.
use anyhow::{Result, anyhow, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Url, header};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Challenge {
    pub wire: BTreeMap<String, String>,
    pub charge_cents: u64,
    pub credit_units: u64,
    pub fee_units: u64,
    pub network_id: String,
    pub expires_at: i64,
}

pub(super) const LIMIT: usize = 65_536;

fn invalid() -> anyhow::Error {
    anyhow!("Tuara returned an invalid or unsupported signup payment response")
}

pub(super) fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| anyhow!("could not initialize Tuara signup HTTP client"))
}

pub(super) async fn bounded_body(mut response: reqwest::Response) -> Result<Vec<u8>> {
    ensure!(
        response.content_length().is_none_or(|n| n <= LIMIT as u64),
        "Tuara signup response exceeded its size limit"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| {
        anyhow!("Tuara signup response could not be read; do not retry payment automatically")
    })? {
        ensure!(
            bytes.len() + chunk.len() <= LIMIT,
            "Tuara signup response exceeded its size limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(super) async fn prepare(
    url: &Url,
    body: &Value,
    amount_cents: u64,
    max_charge_cents: u64,
    terms_version: &str,
) -> Result<Challenge> {
    let response = client()?
        .post(url.clone())
        .json(body)
        .send()
        .await
        .map_err(|_| anyhow!("Tuara signup challenge request failed"))?;
    challenge_response(
        response,
        amount_cents,
        max_charge_cents,
        terms_version,
        false,
    )
    .await
}

pub(super) async fn challenge_response(
    response: reqwest::Response,
    amount_cents: u64,
    max_charge_cents: u64,
    terms_version: &str,
    topup: bool,
) -> Result<Challenge> {
    ensure!(
        response.status() == reqwest::StatusCode::PAYMENT_REQUIRED,
        "Tuara did not return a signup payment challenge"
    );
    let headers: Vec<_> = response
        .headers()
        .get_all(header::WWW_AUTHENTICATE)
        .iter()
        .collect();
    ensure!(
        headers.len() == 1,
        "Tuara must return exactly one payment challenge"
    );
    let value = headers[0].to_str().map_err(|_| invalid())?.to_owned();
    let bytes = bounded_body(response).await?;
    let economics = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    parse_challenge_kind(
        &value,
        &economics,
        amount_cents,
        max_charge_cents,
        terms_version,
        topup,
    )
}

pub(super) async fn pay(
    url: &Url,
    body: &Value,
    challenge: &Challenge,
    spt: &str,
) -> Result<Vec<u8>> {
    let mut authorization = header::HeaderValue::from_str(&payment_authorization(challenge, spt)?)
        .map_err(|_| invalid())?;
    authorization.set_sensitive(true);
    let response = client()?
        .post(url.clone())
        .header(header::AUTHORIZATION, authorization)
        .json(body)
        .send()
        .await
        .map_err(|_| {
            anyhow!("Tuara signup payment outcome is unknown; do not retry automatically")
        })?;
    if response.status() != reqwest::StatusCode::CREATED {
        let status = response.status().as_u16();
        let body = bounded_body(response).await?;
        let code = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|value| value["code"].as_str().map(str::to_owned))
            .filter(|code| {
                code.len() <= 64 && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            });
        return Err(anyhow!(
            "Tuara payment returned HTTP {status} (code {}). Do not retry automatically",
            code.as_deref().unwrap_or("unavailable")
        ));
    }
    // Save all successful response bytes before interpreting the one-time key.
    bounded_body(response).await
}

pub(super) fn payment_authorization(challenge: &Challenge, spt: &str) -> Result<String> {
    ensure!(
        challenge.expires_at > time::OffsetDateTime::now_utc().unix_timestamp(),
        "Tuara signup payment challenge expired; prepare a new signup"
    );
    ensure!(
        spt.starts_with("spt_") && spt.len() <= 4096 && spt.bytes().all(|b| b.is_ascii_graphic()),
        "invalid shared payment token"
    );
    let encoded = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "challenge": challenge.wire, "payload": {"spt": spt}
        }))
        .map_err(|_| invalid())?,
    );
    Ok(format!("Payment {encoded}"))
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 4096)
        .ok_or_else(invalid)
}

fn number(value: &Value, key: &str) -> Result<u64> {
    value.get(key).and_then(Value::as_u64).ok_or_else(invalid)
}

fn attribute<'a>(wire: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str> {
    wire.iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(invalid)
}

fn attributes(header: &str) -> Result<BTreeMap<String, String>> {
    ensure!(
        header.len() <= LIMIT,
        "Tuara payment challenge exceeded its size limit"
    );
    let (scheme, mut rest) = header.trim().split_once(' ').ok_or_else(invalid)?;
    ensure!(
        scheme.eq_ignore_ascii_case("Payment"),
        "unsupported Tuara payment challenge"
    );
    let mut result: BTreeMap<String, String> = BTreeMap::new();
    loop {
        rest = rest.trim_start();
        let (name, value) = rest.split_once('=').ok_or_else(invalid)?;
        let name = name.trim_end();
        ensure!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "invalid Tuara payment challenge attribute"
        );
        ensure!(
            !result.keys().any(|key| key.eq_ignore_ascii_case(name)),
            "duplicate Tuara payment challenge attribute"
        );
        let value = value.trim_start().strip_prefix('"').ok_or_else(invalid)?;
        let mut decoded = String::new();
        let mut escaped = false;
        let mut end = None;
        for (i, ch) in value.char_indices() {
            ensure!(
                !ch.is_control(),
                "invalid Tuara payment challenge attribute"
            );
            if escaped {
                decoded.push(ch);
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                end = Some(i + 1);
                break;
            } else {
                decoded.push(ch);
            }
        }
        rest = value
            .get(end.ok_or_else(invalid)?..)
            .ok_or_else(invalid)?
            .trim_start();
        result.insert(name.to_owned(), decoded);
        if rest.is_empty() {
            return Ok(result);
        }
        rest = rest.strip_prefix(',').ok_or_else(invalid)?;
    }
}

#[cfg(test)]
fn parse_challenge(
    header: &str,
    body: &Value,
    amount: u64,
    cap: u64,
    terms: &str,
) -> Result<Challenge> {
    parse_challenge_kind(header, body, amount, cap, terms, false)
}

fn parse_challenge_kind(
    header: &str,
    body: &Value,
    amount: u64,
    cap: u64,
    terms: &str,
    topup: bool,
) -> Result<Challenge> {
    ensure!(
        (500..=1_000_000).contains(&amount),
        "Tuara signup credit must be between $5 and $10,000"
    );
    let wire = attributes(header)?;
    ensure!(
        attribute(&wire, "method")? == "stripe" && attribute(&wire, "intent")? == "charge",
        "Tuara signup requires a Stripe charge challenge"
    );
    attribute(&wire, "realm")?;
    attribute(&wire, "id")?;
    if let Ok(field) = attribute(&wire, "header") {
        ensure!(
            field.eq_ignore_ascii_case("Authorization"),
            "unsupported Tuara payment credential header"
        );
    }
    let request: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(attribute(&wire, "request")?)
            .map_err(|_| invalid())?,
    )
    .map_err(|_| invalid())?;
    ensure!(
        text(&request, "currency")? == "usd"
            && request
                .get("decimals")
                .is_none_or(|_| number(&request, "decimals").ok() == Some(2))
            && text(&request, "externalId")? == text(body, "challenge_id")?,
        "Tuara signup requires a matching USD-cents challenge"
    );
    let amount_text = text(&request, "amount")?;
    ensure!(
        amount_text.bytes().all(|b| b.is_ascii_digit()),
        "invalid Tuara payment amount"
    );
    let charge: u64 = amount_text.parse().map_err(|_| invalid())?;
    let credit = amount.checked_mul(1_000_000).ok_or_else(invalid)?;
    let fee = credit
        .checked_mul(number(body, "fee_basis_points")?)
        .ok_or_else(invalid)?
        .div_ceil(10_000);
    ensure!(
        number(body, "credit_units")? == credit && number(body, "fee_units")? == fee,
        "Tuara payment credit or fee does not match its declared terms"
    );
    ensure!(
        charge == number(body, "charge_cents")?
            && charge
                == credit
                    .checked_add(fee)
                    .ok_or_else(invalid)?
                    .div_ceil(1_000_000)
            && charge <= cap,
        "Tuara payment charge exceeds its cap or disagrees with the quoted amount"
    );
    if !topup
        || body
            .get("terms_version")
            .is_some_and(|value| !value.is_null())
    {
        ensure!(
            text(body, "terms_version")? == terms,
            "Tuara payment terms version changed; review and prepare again"
        );
    }
    let details = request.get("methodDetails").ok_or_else(invalid)?;
    let network_id = text(details, "networkId")?.to_owned();
    ensure!(
        network_id.len() <= 256
            && network_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "invalid Tuara Stripe network identifier"
    );
    ensure!(
        details.get("paymentMethodTypes") == Some(&json!(["card"])),
        "Tuara signup requires a card payment challenge"
    );
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let mut expires_at = now + 900;
    let wire_expiry = wire
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("expires"))
        .map(|(_, value)| value.as_str());
    let request_expiry = request
        .get("expires")
        .map(|value| value.as_str().ok_or_else(invalid))
        .transpose()?;
    for expiry in [wire_expiry, request_expiry].into_iter().flatten() {
        let timestamp =
            time::OffsetDateTime::parse(expiry, &time::format_description::well_known::Rfc3339)
                .map_err(|_| invalid())?
                .unix_timestamp();
        ensure!(timestamp > now, "Tuara payment challenge has expired");
        expires_at = expires_at.min(timestamp);
    }
    Ok(Challenge {
        wire,
        charge_cents: charge,
        credit_units: credit,
        fee_units: fee,
        network_id,
        expires_at,
    })
}

pub(super) fn credential(
    bytes: &[u8],
    challenge: &Challenge,
    terms: &str,
) -> Result<(String, Value)> {
    ensure!(
        bytes.len() <= LIMIT,
        "Tuara signup response exceeded its size limit"
    );
    let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let key = value.get("key").ok_or_else(invalid)?;
    let raw_key = text(key, "raw_key")?;
    ensure!(
        raw_key.bytes().all(|b| b.is_ascii_graphic()),
        "invalid Tuara inference key"
    );
    ensure!(
        key.get("scopes")
            .and_then(Value::as_array)
            .is_some_and(|scopes| scopes.iter().any(|s| s.as_str() == Some("router:invoke"))),
        "Tuara signup key lacks inference access"
    );
    let organization_id = text(value.get("organization").ok_or_else(invalid)?, "id")?;
    ensure!(
        organization_id.len() <= 256,
        "invalid Tuara organization identifier"
    );
    let payment = value.get("payment").ok_or_else(invalid)?;
    ensure!(
        number(payment, "charge_cents")? == challenge.charge_cents,
        "Tuara payment receipt charge differs from its challenge"
    );
    ensure!(
        text(value.get("terms").ok_or_else(invalid)?, "version")? == terms,
        "Tuara signup terms receipt differs from accepted terms"
    );
    Ok((
        raw_key.to_owned(),
        json!({"organization_id":organization_id, "charge_cents":challenge.charge_cents,"currency":"usd","terms_version":terms}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[tokio::test]
    #[ignore = "live Tuara test-mode quote; opt in explicitly"]
    async fn live_test_mode_quote_contract() {
        let url = Url::parse("https://tuara.com/v1/agents").unwrap();
        let body = json!({"amount_dollars":5,"organization_name":"Horde Contract Test","agent":{"name":"horde-smoke","platform":"horde"},"terms_version":"2026-09"});
        let challenge = prepare(&url, &body, 500, 512, "2026-09").await.unwrap();
        assert_eq!(challenge.charge_cents, 512);
    }

    async fn server(router: axum::Router) -> (Url, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!(
            "http://{}/v1/agents",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (url, task)
    }

    #[tokio::test]
    async fn exchanges_exact_challenge_and_preserves_success_bytes() {
        let (header, economics) = fixture();
        let body = json!({"amount_dollars":20,"terms_version":"2026-09"});
        let expected_body = body.clone();
        let expected_wire = attributes(&header).unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let router = axum::Router::new().route(
            "/v1/agents",
            axum::routing::post(
                move |headers: axum::http::HeaderMap, axum::Json(received): axum::Json<Value>| {
                    let (header, economics, expected_body, expected_wire, count) = (
                        header.clone(),
                        economics.clone(),
                        expected_body.clone(),
                        expected_wire.clone(),
                        count.clone(),
                    );
                    async move {
                        assert_eq!(received, expected_body);
                        if count.fetch_add(1, Ordering::SeqCst) == 0 {
                            assert!(!headers.contains_key("authorization"));
                            axum::http::Response::builder()
                                .status(402)
                                .header("www-authenticate", header)
                                .body(axum::body::Body::from(economics.to_string()))
                                .unwrap()
                        } else {
                            let encoded = headers["authorization"]
                                .to_str()
                                .unwrap()
                                .strip_prefix("Payment ")
                                .unwrap();
                            let credential: Value =
                                serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded).unwrap())
                                    .unwrap();
                            assert_eq!(
                                credential["challenge"],
                                serde_json::to_value(expected_wire).unwrap()
                            );
                            assert_eq!(credential["payload"], json!({"spt":"spt_private"}));
                            // Even malformed JSON must reach the caller's durable recovery storage.
                            axum::http::Response::builder()
                                .status(201)
                                .body(axum::body::Body::from("one-time-key response bytes"))
                                .unwrap()
                        }
                    }
                },
            ),
        );
        let (url, task) = server(router).await;
        let challenge = prepare(&url, &body, 2000, 2048, "2026-09").await.unwrap();
        assert_eq!(
            pay(&url, &body, &challenge, "spt_private").await.unwrap(),
            b"one-time-key response bytes"
        );
        assert_eq!(seen.load(Ordering::SeqCst), 2);
        task.abort();
    }

    #[tokio::test]
    async fn does_not_follow_redirects_or_retry_errors() {
        for status in [302, 503] {
            let count = Arc::new(AtomicUsize::new(0));
            let seen = count.clone();
            let router = axum::Router::new().fallback(move || {
                let count = count.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    axum::http::Response::builder()
                        .status(status)
                        .header("location", "/redirect")
                        .body(axum::body::Body::from("secret provider error"))
                        .unwrap()
                }
            });
            let (url, task) = server(router).await;
            let error = prepare(&url, &json!({}), 2000, 2048, "2026-09")
                .await
                .unwrap_err();
            assert!(!error.to_string().contains("secret provider error"));
            assert_eq!(seen.load(Ordering::SeqCst), 1);
            task.abort();
        }
    }

    #[tokio::test]
    async fn rejects_expired_payment_before_network_and_oversized_bodies() {
        let (header, economics) = fixture();
        let mut challenge = parse_challenge(&header, &economics, 2000, 2048, "2026-09").unwrap();
        challenge.expires_at = 0;
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let router = axum::Router::new().fallback(move || {
            let count = count.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                axum::http::Response::builder()
                    .status(201)
                    .body(axum::body::Body::from(vec![b'x'; LIMIT + 1]))
                    .unwrap()
            }
        });
        let (url, task) = server(router).await;
        assert!(
            pay(&url, &json!({}), &challenge, "spt_private")
                .await
                .is_err()
        );
        assert_eq!(seen.load(Ordering::SeqCst), 0);
        challenge.expires_at = time::OffsetDateTime::now_utc().unix_timestamp() + 60;
        assert!(
            pay(&url, &json!({}), &challenge, "spt_private")
                .await
                .is_err()
        );
        assert_eq!(seen.load(Ordering::SeqCst), 1);
        task.abort();
    }

    fn fixture() -> (String, Value) {
        let request = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "amount":"2048", "currency":"usd", "externalId":"challenge_test",
                "methodDetails":{"networkId":"profile_test", "paymentMethodTypes":["card"]}
            }))
            .unwrap(),
        );
        (
            format!(
                "Payment id=\"mpp_challenge_id\", realm=\"tuara.com\", method=\"stripe\", intent=\"charge\", request=\"{request}\", opaque=\"opaque_unchanged\", description=\"A, \\\"quoted\\\" purchase\""
            ),
            json!({"challenge_id":"challenge_test", "credit_units":2_000_000_000_u64,
            "fee_units":48_000_000, "charge_cents":2048,"fee_basis_points":240,
            "terms_version":"2026-09"}),
        )
    }

    #[test]
    fn preserves_challenge_values_and_quoted_escapes() {
        let (header, body) = fixture();
        let result = parse_challenge(&header, &body, 2000, 2048, "2026-09").unwrap();
        assert_eq!(result.wire["opaque"], "opaque_unchanged");
        assert_eq!(result.wire["description"], "A, \"quoted\" purchase");
        assert_eq!(result.charge_cents, 2048);
        assert_eq!(result.network_id, "profile_test");
    }

    #[test]
    fn rejects_inconsistent_economics_and_terms() {
        let (header, body) = fixture();
        assert!(parse_challenge(&header, &body, 2000, 2047, "2026-09").is_err());
        assert!(parse_challenge(&header, &body, 2000, 2048, "old").is_err());
        for (key, value) in [
            ("charge_cents", json!(2047)),
            ("credit_units", json!(100)),
            ("fee_units", json!(47_000_000)),
            ("fee_basis_points", json!(0)),
            ("challenge_id", json!("different")),
        ] {
            let mut altered = body.clone();
            altered[key] = value;
            assert!(
                parse_challenge(&header, &altered, 2000, 2048, "2026-09").is_err(),
                "{key}"
            );
        }
    }

    #[test]
    fn rejects_unsupported_request_fields_and_malformed_expiry() {
        let (header, body) = fixture();
        let wire = attributes(&header).unwrap();
        let request: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&wire["request"]).unwrap()).unwrap();
        for (field, value) in [
            ("amount", json!("+2048")),
            ("currency", json!("eur")),
            ("externalId", json!("other_challenge")),
            ("decimals", json!(3)),
            ("expires", json!(null)),
            (
                "methodDetails",
                json!({"networkId":"profile_test","paymentMethodTypes":["link"]}),
            ),
        ] {
            let mut altered = request.clone();
            altered[field] = value;
            let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&altered).unwrap());
            let altered_header = header.replace(&wire["request"], &encoded);
            assert!(
                parse_challenge(&altered_header, &body, 2000, 2048, "2026-09").is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn rejects_duplicate_attributes_multiple_challenges_and_expiry() {
        let (header, body) = fixture();
        for suffix in [
            ", ID=\"other\"",
            ", Payment id=\"other\"",
            ",",
            ", expires=\"2000-01-01T00:00:00Z\"",
        ] {
            assert!(
                parse_challenge(&(header.clone() + suffix), &body, 2000, 2048, "2026-09").is_err()
            );
        }
    }

    #[test]
    fn credential_summary_does_not_copy_secrets_or_card_details() {
        let (header, body) = fixture();
        let challenge = parse_challenge(&header, &body, 2000, 2048, "2026-09").unwrap();
        let response = json!({"organization":{"id":"org_test","name":"private name"},
            "key":{"raw_key":"sk_tuara_secret", "scopes":["router:invoke"]},
            "payment":{"charge_cents":2048,"card":{"last4":"4242"}},
            "terms":{"version":"2026-09"}, "token":"private_token"});
        let (key, summary) = credential(
            &serde_json::to_vec(&response).unwrap(),
            &challenge,
            "2026-09",
        )
        .unwrap();
        assert_eq!(key, "sk_tuara_secret");
        let text = summary.to_string();
        for private in ["sk_tuara_secret", "private name", "4242", "private_token"] {
            assert!(!text.contains(private));
        }
        let mut invalid = response;
        invalid["key"]["scopes"] = json!(["agent:read"]);
        assert!(
            credential(
                &serde_json::to_vec(&invalid).unwrap(),
                &challenge,
                "2026-09"
            )
            .is_err()
        );
    }
}
