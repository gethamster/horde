use super::Config;
use anyhow::{Result, ensure};
use futures_util::StreamExt;
use std::{io::Read, path::Path, time::Duration};

pub(super) enum Outcome {
    Delivered,
    Retry(&'static str),
    Held(&'static str),
}

pub(super) fn token(path: &Path) -> Result<String> {
    let contents = String::from_utf8(private_file(path, 4096)?)?;
    let value = contents.trim();
    ensure!(
        (32..=4096).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_graphic()),
        "invalid observation credential"
    );
    Ok(value.to_owned())
}

pub(super) fn private_file(path: &Path, limit: u64) -> Result<Vec<u8>> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.mode() & 0o077 == 0
            && metadata.uid() == unsafe { libc::geteuid() },
        "credential must be controller-owned private regular file"
    );
    let mut contents = Vec::new();
    file.take(limit + 1).read_to_end(&mut contents)?;
    ensure!(contents.len() as u64 <= limit, "private file exceeds bound");
    Ok(contents)
}

pub(super) async fn send(c: &Config, key: &str, payload: &str) -> Outcome {
    let Ok(token) = token(&c.token_file) else {
        return Outcome::Held("credential_unavailable");
    };
    let Ok(client) = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return Outcome::Held("transport_unavailable");
    };
    let result = client
        .post(&c.endpoint)
        .bearer_auth(token)
        .header("X-Tenant-ID", &c.tenant_id)
        .header("Idempotency-Key", key)
        .header("Content-Type", "application/json")
        .body(payload.to_owned())
        .send()
        .await;
    let Ok(response) = result else {
        return Outcome::Retry("transport_uncertain");
    };
    let status = response.status().as_u16();
    if status == 429 || status >= 500 {
        return Outcome::Retry("service_unavailable");
    }
    if status == 401 || status == 403 {
        return Outcome::Held("credential_rejected");
    }
    if status == 409 {
        return Outcome::Held("idempotency_conflict");
    }
    if ![200, 202].contains(&status) {
        return Outcome::Held("observation_rejected");
    }
    if response.content_length().is_some_and(|n| n > 4096) {
        return Outcome::Held("invalid_receipt");
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            return Outcome::Retry("transport_uncertain");
        };
        if bytes.len() + chunk.len() > 4096 {
            return Outcome::Held("invalid_receipt");
        }
        bytes.extend_from_slice(&chunk);
    }
    let Ok(receipt) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Outcome::Held("invalid_receipt");
    };
    if receipt["idempotency_key"] != key
        || receipt["id"]
            .as_str()
            .is_none_or(|s| uuid::Uuid::parse_str(s).is_err())
    {
        return Outcome::Held("invalid_receipt");
    }
    Outcome::Delivered
}
