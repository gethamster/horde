//! Authenticated balance reads and bounded, non-retrying machine top-ups.
use super::wire::{self, Challenge};
use anyhow::{Result, anyhow, ensure};
use reqwest::{Url, header};
use serde_json::{Value, json};
use std::time::Duration;

fn invalid() -> anyhow::Error {
    anyhow!("Tuara returned an invalid top-up response")
}

fn authorization(key: &str, payment: Option<String>) -> Result<header::HeaderValue> {
    ensure!(
        !key.is_empty()
            && key.len() <= 4096
            && key.bytes().all(|b| b.is_ascii_graphic() && b != b','),
        "invalid Tuara account credential"
    );
    let value = match payment {
        Some(payment) => format!("{payment}, Bearer {key}"),
        None => format!("Bearer {key}"),
    };
    let mut header = header::HeaderValue::from_str(&value).map_err(|_| invalid())?;
    header.set_sensitive(true);
    Ok(header)
}

async fn read(url: Url, key: &str) -> Result<Value> {
    let response = wire::client()?
        .get(url)
        .timeout(Duration::from_secs(5))
        .header(header::AUTHORIZATION, authorization(key, None)?)
        .send()
        .await
        .map_err(|_| anyhow!("Tuara account read failed"))?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "Tuara account read was rejected"
    );
    serde_json::from_slice(&wire::bounded_body(response).await?).map_err(|_| invalid())
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        })
        .ok_or_else(invalid)
}

fn number(value: &Value, key: &str) -> Result<u64> {
    value.get(key).and_then(Value::as_u64).ok_or_else(invalid)
}

/// Read identity and balance with the same credential before preparing any charge.
pub(super) async fn balance(origin: &Url, key: &str) -> Result<(String, i64)> {
    let identity = read(
        origin
            .join("/account/api/v1/auth/introspect")
            .map_err(|_| invalid())?,
        key,
    )
    .await?;
    let data = identity.get("data").ok_or_else(invalid)?;
    ensure!(
        data["kind"] == "api",
        "Tuara automatic top-up requires an API key"
    );
    let organization = text(data, "organizationId")?.to_owned();
    ensure!(
        data.get("scopes")
            .and_then(Value::as_array)
            .is_some_and(|scopes| scopes
                .iter()
                .any(|scope| scope.as_str() == Some("agent:manage"))),
        "Tuara automatic top-up requires an agent:manage credential"
    );
    let account = read(
        origin.join("/api/v1/buy/account").map_err(|_| invalid())?,
        key,
    )
    .await?;
    text(&account, "buyer_id")?;
    for key in ["balance_units", "pending_units"] {
        ensure!(
            account.get(key).and_then(Value::as_i64).is_some(),
            "invalid Tuara account balance"
        );
    }
    let available = account
        .get("available_units")
        .and_then(Value::as_i64)
        .ok_or_else(invalid)?;
    Ok((organization, available))
}

pub(super) async fn prepare_topup(
    url: &Url,
    key: &str,
    body: &Value,
    amount_cents: u64,
    max_charge_cents: u64,
    terms_version: &str,
) -> Result<Challenge> {
    let response = wire::client()?
        .post(url.clone())
        .timeout(Duration::from_secs(5))
        .header(header::AUTHORIZATION, authorization(key, None)?)
        .json(body)
        .send()
        .await
        .map_err(|_| anyhow!("Tuara top-up challenge request failed"))?;
    // Top-up challenges omit terms_version; a declared version must still match.
    wire::challenge_response(
        response,
        amount_cents,
        max_charge_cents,
        terms_version,
        true,
    )
    .await
}

pub(super) async fn pay_topup(
    url: &Url,
    key: &str,
    body: &Value,
    challenge: &Challenge,
    spt: &str,
) -> Result<Vec<u8>> {
    let payment = wire::payment_authorization(challenge, spt)?;
    let response = wire::client()?
        .post(url.clone())
        .timeout(Duration::from_secs(5))
        .header(header::AUTHORIZATION, authorization(key, Some(payment))?)
        .json(body)
        .send()
        .await
        .map_err(|_| {
            anyhow!("Tuara top-up payment outcome is unknown; do not retry automatically")
        })?;
    ensure!(
        response.status() == reqwest::StatusCode::OK,
        "Tuara top-up payment did not return success; do not retry automatically"
    );
    wire::bounded_body(response).await
}

pub(super) fn validate_topup_response(bytes: &[u8], challenge: &Challenge) -> Result<Value> {
    ensure!(
        bytes.len() <= wire::LIMIT,
        "Tuara top-up response exceeded its size limit"
    );
    let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let payment = value.get("payment").ok_or_else(invalid)?;
    ensure!(
        number(payment, "charge_cents")? == challenge.charge_cents
            && number(payment, "credit_units")? == challenge.credit_units
            && number(payment, "fee_units")? == challenge.fee_units,
        "Tuara top-up receipt differs from the authorized charge and credit"
    );
    let event = value.get("funding_event").ok_or_else(invalid)?;
    ensure!(
        number(event, "credit_units")? == challenge.credit_units
            && number(event, "fee_units")? == challenge.fee_units
            && number(event, "charge_units")?
                == challenge
                    .credit_units
                    .checked_add(challenge.fee_units)
                    .ok_or_else(invalid)?
            && text(event, "payment_intent_id")? == text(payment, "payment_intent_id")?
            && text(event, "channel")? == "machine"
            && text(event, "rail")? == "card",
        "Tuara top-up funding event differs from its payment receipt"
    );
    text(event, "provider_namespace")?;
    let balance = value.get("balance").ok_or_else(invalid)?;
    let available = balance
        .get("available_units")
        .and_then(Value::as_i64)
        .ok_or_else(invalid)?;
    let total = balance
        .get("balance_units")
        .and_then(Value::as_i64)
        .ok_or_else(invalid)?;
    Ok(
        json!({"charge_cents":challenge.charge_cents,"credit_units":challenge.credit_units,
        "fee_units":challenge.fee_units,"currency":"usd","available_units":available,"balance_units":total}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn quote() -> (String, Value) {
        let request = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(
                &json!({"amount":"2048", "currency":"usd", "externalId":"challenge_test",
            "methodDetails":{"networkId":"profile_test","paymentMethodTypes":["card"]}}),
            )
            .unwrap(),
        );
        (
            format!(
                "Payment id=\"mpp_challenge_id\", realm=\"tuara.com\", method=\"stripe\", intent=\"charge\", request=\"{request}\""
            ),
            json!({"challenge_id":"challenge_test","credit_units":2_000_000_000_u64,"fee_units":48_000_000,"fee_basis_points":240,"charge_cents":2048}),
        )
    }

    fn receipt() -> Value {
        json!({"payment":{"charge_cents":2048,"credit_units":2_000_000_000_u64,"fee_units":48_000_000,"payment_intent_id":"pi_test","card":{"last4":"4242"}},
            "balance":{"balance_units":2_500_000_000_u64,"available_units":2_000_000_000_u64},
            "funding_event":{"credit_units":2_000_000_000_u64,"fee_units":48_000_000,"charge_units":2_048_000_000_u64,"payment_intent_id":"pi_test","provider_namespace":"private_namespace","channel":"machine","rail":"card"},"bonus":{"granted":false}})
    }

    #[test]
    fn credential_headers_are_sensitive_and_cannot_inject_another_scheme() {
        assert!(
            authorization("sk_tuara_private", None)
                .unwrap()
                .is_sensitive()
        );
        assert!(
            authorization("sk_tuara_private", Some("Payment encoded".to_owned()))
                .unwrap()
                .is_sensitive()
        );
        assert!(authorization("key,Payment other", None).is_err());
        assert!(authorization("key\r\nX-Secret: value", None).is_err());
    }

    #[tokio::test]
    async fn permits_absent_topup_terms_but_rejects_a_new_declared_version() {
        for (terms, accepted) in [(Value::Null, true), (json!("new-terms"), false)] {
            let (header, mut economics) = quote();
            economics["terms_version"] = terms;
            let router = axum::Router::new().fallback(move || {
                let (header, economics) = (header.clone(), economics.clone());
                async move {
                    axum::http::Response::builder()
                        .status(402)
                        .header("www-authenticate", header)
                        .body(axum::body::Body::from(economics.to_string()))
                        .unwrap()
                }
            });
            let (url, task) = server(router).await;
            assert_eq!(
                prepare_topup(
                    &url,
                    "sk_tuara_private",
                    &json!({"amount_dollars":20}),
                    2000,
                    2048,
                    "2026-09"
                )
                .await
                .is_ok(),
                accepted
            );
            task.abort();
        }
    }

    async fn server(router: axum::Router) -> (Url, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!(
            "http://{}/router/v1",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (url, task)
    }

    #[tokio::test]
    async fn authenticates_identity_before_balance_and_allows_signed_available_units() {
        let router = axum::Router::new()
            .route("/account/api/v1/auth/introspect", axum::routing::get(|headers: axum::http::HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer sk_tuara_private");
                axum::Json(json!({"data":{"kind":"api","organizationId":"org_test","scopes":["agent:manage"]}}))
            }))
            .route("/api/v1/buy/account", axum::routing::get(|headers: axum::http::HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer sk_tuara_private");
                axum::Json(json!({"buyer_id":"buyer_test","balance_units":100,"pending_units":101,"available_units":-1}))
            }));
        let (url, task) = server(router).await;
        assert_eq!(
            balance(&url, "sk_tuara_private").await.unwrap(),
            ("org_test".to_owned(), -1)
        );
        task.abort();
    }

    #[tokio::test]
    async fn insufficient_scope_stops_before_balance_or_payment() {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let router = axum::Router::new()
            .route(
                "/account/api/v1/auth/introspect",
                axum::routing::get(|| async {
                    axum::Json(
                        json!({"data":{"kind":"api","organizationId":"org_test","scopes":["router:invoke"]}}),
                    )
                }),
            )
            .fallback(move || {
                count.fetch_add(1, Ordering::SeqCst);
                async { "unexpected" }
            });
        let (url, task) = server(router).await;
        assert!(balance(&url, "sk_tuara_private").await.is_err());
        assert_eq!(seen.load(Ordering::SeqCst), 0);
        task.abort();
    }

    #[tokio::test]
    async fn non_api_identity_stops_before_balance_or_payment() {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let router = axum::Router::new()
            .route("/account/api/v1/auth/introspect", axum::routing::get(|| async {
                axum::Json(json!({"data":{"kind":"session","organizationId":"org_test","scopes":["agent:manage"]}}))
            }))
            .fallback(move || {
                count.fetch_add(1, Ordering::SeqCst);
                async { "unexpected" }
            });
        let (url, task) = server(router).await;
        assert!(balance(&url, "sk_tuara_private").await.is_err());
        assert_eq!(seen.load(Ordering::SeqCst), 0);
        task.abort();
    }

    #[tokio::test]
    async fn topup_uses_same_body_and_combines_payment_with_exact_bearer() {
        let (header, economics) = quote();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let router = axum::Router::new().route(
            "/v1/account/topup",
            axum::routing::post(
                move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| {
                    let (header, economics, count) =
                        (header.clone(), economics.clone(), count.clone());
                    async move {
                        assert_eq!(body, json!({"amount_dollars":20}));
                        let auth = headers["authorization"].to_str().unwrap();
                        if count.fetch_add(1, Ordering::SeqCst) == 0 {
                            assert_eq!(auth, "Bearer sk_tuara_private");
                            axum::http::Response::builder()
                                .status(402)
                                .header("www-authenticate", header)
                                .body(axum::body::Body::from(economics.to_string()))
                                .unwrap()
                        } else {
                            let payment = auth
                                .strip_suffix(", Bearer sk_tuara_private")
                                .unwrap()
                                .strip_prefix("Payment ")
                                .unwrap();
                            let credential: Value =
                                serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payment).unwrap())
                                    .unwrap();
                            assert_eq!(credential["payload"], json!({"spt":"spt_private"}));
                            axum::http::Response::builder()
                                .status(200)
                                .body(axum::body::Body::from(receipt().to_string()))
                                .unwrap()
                        }
                    }
                },
            ),
        );
        let (url, task) = server(router).await;
        let endpoint = url.join("/v1/account/topup").unwrap();
        let body = json!({"amount_dollars":20});
        let challenge = prepare_topup(&endpoint, "sk_tuara_private", &body, 2000, 2048, "2026-09")
            .await
            .unwrap();
        let raw = pay_topup(
            &endpoint,
            "sk_tuara_private",
            &body,
            &challenge,
            "spt_private",
        )
        .await
        .unwrap();
        let summary = validate_topup_response(&raw, &challenge).unwrap();
        assert_eq!(summary["charge_cents"], 2048);
        assert!(!summary.to_string().contains("4242"));
        assert!(!summary.to_string().contains("private_namespace"));
        for path in [
            "/payment/credit_units",
            "/payment/charge_cents",
            "/funding_event/charge_units",
        ] {
            let mut altered = receipt();
            *altered.pointer_mut(path).unwrap() = json!(1);
            assert!(
                validate_topup_response(&serde_json::to_vec(&altered).unwrap(), &challenge)
                    .is_err()
            );
        }
        assert_eq!(seen.load(Ordering::SeqCst), 2);
        task.abort();
    }
}
