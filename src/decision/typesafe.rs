use super::{DecisionBackend, DecisionRequest, DecisionResponse, validate_response};
use crate::config::Decision;
use anyhow::{Context, Result, ensure};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, Url, redirect::Policy};
use serde_json::Value;
use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, SystemTime},
};
use tokio::time::{Instant, sleep_until, timeout_at};

const MAX_RESPONSE_BYTES: usize = 256 * 1024;

#[derive(Debug)]
pub struct DecisionFailure {
    pub error: anyhow::Error,
    pub attempts: usize,
}

impl std::fmt::Display for DecisionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(formatter)
    }
}

impl std::error::Error for DecisionFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

pub fn endpoint(base: &str) -> Result<Url> {
    let mut url = Url::parse(base).context("decision.base_url must be an absolute URL")?;
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "decision.base_url cannot contain user info"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "decision.base_url cannot contain a query or fragment"
    );
    let secure = url.scheme() == "https";
    let loopback = url.scheme() == "http"
        && url
            .host_str()
            .and_then(|host| host.parse::<IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback());
    ensure!(
        secure || loopback,
        "decision endpoint requires HTTPS except for literal loopback HTTP"
    );
    let path = url.path().trim_end_matches('/');
    url.set_path(&format!("{path}/v1/systemone"));
    Ok(url)
}

#[derive(Clone)]
pub struct DecisionHttpClient {
    client: Client,
    config: Decision,
    endpoint: Url,
    authority: Authority,
}

#[derive(Clone)]
enum Authority {
    User,
    Task { root: PathBuf, task: String },
}

/// Jev-compatible SystemOne HTTP transport, independent of provider identity.
impl DecisionHttpClient {
    pub fn new(config: Decision) -> Result<Self> {
        Self::new_with_authority(config, Authority::User)
    }

    /// Authorize every provider attempt against the task's current project owner.
    pub fn new_project(
        config: Decision,
        root: impl AsRef<Path>,
        task: impl Into<String>,
    ) -> Result<Self> {
        Self::new_with_authority(
            config,
            Authority::Task {
                root: root.as_ref().to_path_buf(),
                task: task.into(),
            },
        )
    }

    fn new_with_authority(config: Decision, authority: Authority) -> Result<Self> {
        config.validate()?;
        let endpoint = endpoint(&config.base_url)?;
        static HOSTED_CLIENT: OnceLock<Client> = OnceLock::new();
        static LOCAL_CLIENT: OnceLock<Client> = OnceLock::new();
        let slot = if endpoint.scheme() == "http" {
            &LOCAL_CLIENT
        } else {
            &HOSTED_CLIENT
        };
        let client = if let Some(client) = slot.get() {
            client.clone()
        } else {
            // A loopback request must not carry the key through an HTTP proxy.
            let builder = Client::builder().redirect(Policy::none());
            let built = if endpoint.scheme() == "http" {
                builder.no_proxy()
            } else {
                builder
            }
            .build()?;
            let _ = slot.set(built.clone());
            slot.get().cloned().unwrap_or(built)
        };
        Ok(Self {
            client,
            config,
            endpoint,
            authority,
        })
    }

    fn authorize(&self) -> Result<String> {
        let current = match &self.authority {
            Authority::User => crate::config::Settings::load_user()
                .context("current operator decision configuration unavailable")?,
            Authority::Task { root, task } => {
                let db = crate::store::Store::open(root)
                    .context("current project decision configuration unavailable")?;
                ensure!(
                    db.task(task)?["status"] == "running",
                    "task is no longer running for a decision request"
                );
                let project = crate::projects::task_project(&db, task)
                    .context("current task project unavailable")?;
                crate::config::Settings::load_project_user(&db, &project)
                    .context("current project decision configuration unavailable")?
            }
        };
        ensure!(
            current.decision.mode == crate::config::DecisionMode::Shadow
                && current.decision == self.config,
            "current operator decision configuration no longer authorizes this request"
        );
        crate::config::credential(&self.config.api_key_env)
            .context("decision credential unavailable")
    }

    fn retry_delay(response: &reqwest::Response, attempts: usize) -> Duration {
        if let Some(value) = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
        {
            if let Ok(seconds) = value.parse::<u64>() {
                return Duration::from_secs(seconds);
            }
            if let Ok(when) = httpdate::parse_http_date(value) {
                return when.duration_since(SystemTime::now()).unwrap_or_default();
            }
        }
        Duration::from_millis(100_u64.saturating_mul(1_u64 << attempts.saturating_sub(1).min(6)))
    }

    async fn body(response: reqwest::Response) -> Result<Value> {
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("decision response read failed")?;
            ensure!(
                bytes.len() + chunk.len() <= MAX_RESPONSE_BYTES,
                "decision response exceeds the size limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).context("decision response was not valid JSON")
    }

    pub async fn decide_counted(
        &self,
        request: &DecisionRequest,
    ) -> std::result::Result<(DecisionResponse, usize), DecisionFailure> {
        self.decide_counted_with(request, |_| {})
            .await
            .map_err(|(error, attempts)| DecisionFailure { error, attempts })
    }

    pub(crate) async fn decide_counted_with(
        &self,
        request: &DecisionRequest,
        attempt_started: impl Fn(usize),
    ) -> std::result::Result<(DecisionResponse, usize), (anyhow::Error, usize)> {
        let prepared = (|| -> Result<Vec<u8>> {
            request.validate()?;
            ensure!(
                request.model == self.config.model,
                "decision model pin mismatch"
            );
            let body = serde_json::to_vec(&super::wire_request(request))?;
            ensure!(
                body.len() <= super::MAX_REQUEST_BYTES,
                "decision request exceeds the size limit"
            );
            Ok(body)
        })();
        let body = prepared.map_err(|error| (error, 0))?;
        let deadline = Instant::now() + Duration::from_millis(self.config.deadline_ms);
        let mut attempts = 0;
        loop {
            let key = self.authorize().map_err(|error| (error, attempts))?;
            if key.is_empty()
                || body
                    .windows(key.len())
                    .any(|window| window == key.as_bytes())
            {
                return Err((
                    anyhow::anyhow!("decision request contains the active credential"),
                    attempts,
                ));
            }
            attempts += 1;
            attempt_started(attempts);
            let sent = timeout_at(
                deadline,
                self.client
                    .post(self.endpoint.clone())
                    .bearer_auth(key)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body.clone())
                    .send(),
            )
            .await;
            match sent {
                Err(_) => return Err((anyhow::anyhow!("decision deadline exceeded"), attempts)),
                Ok(Ok(response)) if response.status().is_success() => {
                    let value = match timeout_at(deadline, Self::body(response)).await {
                        Ok(Ok(value)) => value,
                        Ok(Err(error)) => return Err((error, attempts)),
                        Err(_) => {
                            return Err((anyhow::anyhow!("decision deadline exceeded"), attempts));
                        }
                    };
                    return validate_response(request, &value)
                        .map(|response| (response, attempts))
                        .map_err(|error| (error, attempts));
                }
                Ok(Ok(response)) => {
                    let status = response.status();
                    let retryable =
                        status == StatusCode::TOO_MANY_REQUESTS || status.as_u16() == 529;
                    if !retryable || attempts >= self.config.max_attempts {
                        return Err((
                            anyhow::anyhow!("decision provider returned HTTP {status}"),
                            attempts,
                        ));
                    }
                    let delay = Self::retry_delay(&response, attempts);
                    let wake = Instant::now() + delay;
                    if wake >= deadline {
                        return Err((
                            anyhow::anyhow!("decision deadline exceeded before retry"),
                            attempts,
                        ));
                    }
                    sleep_until(wake).await;
                }
                Ok(Err(error)) => {
                    if !(error.is_connect() || error.is_timeout())
                        || attempts >= self.config.max_attempts
                    {
                        return Err((
                            anyhow::anyhow!("decision provider request failed"),
                            attempts,
                        ));
                    }
                    let wake = Instant::now()
                        + Duration::from_millis(
                            100_u64.saturating_mul(1_u64 << attempts.saturating_sub(1).min(6)),
                        );
                    if wake >= deadline {
                        return Err((
                            anyhow::anyhow!("decision deadline exceeded before retry"),
                            attempts,
                        ));
                    }
                    sleep_until(wake).await;
                }
            }
        }
    }
}

#[tonic::async_trait]
impl DecisionBackend for DecisionHttpClient {
    async fn decide(&self, request: &DecisionRequest) -> Result<DecisionResponse> {
        self.decide_counted(request)
            .await
            .map(|(response, _)| response)
            .map_err(|failure| failure.error)
    }
}

/// Compatibility alias for callers compiled against the launch transport.
pub type TypeSafe = DecisionHttpClient;
