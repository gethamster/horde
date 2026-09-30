//! Transport runs in a separate service namespace containing no model executors.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Trusted {
    pub schema_version: u32,
    pub endpoint: String,
    pub token_file: PathBuf,
    pub tenant_id: String,
    pub telemetry_project_id: String,
    pub diagnostic_thread_id: String,
    pub project_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    project_id: String,
    thread_id: String,
    run_id: String,
    kind: String,
    severity: String,
    message: String,
    source_ref: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    schema_version: u32,
    event_seq: i64,
    task_id: String,
    tenant_id: String,
    project_id: String,
    thread_id: Option<String>,
    brief_id: Option<String>,
    run_id: String,
    status: String,
    reason: String,
}

impl Trusted {
    fn load(path: &std::path::Path) -> Result<Self> {
        let bytes = transport::private_file(path, 16384)?;
        let c: Self =
            serde_json::from_slice(&bytes).context("invalid immutable exporter configuration")?;
        ensure!(
            c.schema_version == 1
                && c.endpoint == "http://signals:8080/v1/observations"
                && c.token_file == std::path::Path::new("/run/system/telemetry/token"),
            "exporter transport must match installation-private Signals"
        );
        ensure!(
            [
                &c.tenant_id,
                &c.telemetry_project_id,
                &c.diagnostic_thread_id
            ]
            .into_iter()
            .all(|s| valid_id(s))
                && c.telemetry_project_id == "system-operations"
                && c.diagnostic_thread_id == format!("{}-operations", c.tenant_id),
            "invalid immutable diagnostic scope"
        );
        ensure!(
            !c.project_ids.is_empty()
                && c.project_ids.len() <= 128
                && c.project_ids.iter().all(|s| valid_id(s))
                && c.project_ids
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    == c.project_ids.len(),
            "invalid immutable source projects"
        );
        Ok(c)
    }

    pub(super) fn authorize(&self, c: &Config, key: &str, payload: &str) -> Result<()> {
        ensure!(
            c.schema_version == 1
                && c.endpoint == self.endpoint
                && c.token_file == self.token_file
                && c.tenant_id == self.tenant_id
                && c.telemetry_project_id == self.telemetry_project_id
                && c.diagnostic_thread_id == self.diagnostic_thread_id
                && self.project_ids.contains(&c.project_id),
            "database transport policy mismatch"
        );
        ensure!(payload.len() <= 4096, "observation payload exceeds bound");
        let envelope: Envelope = serde_json::from_str(payload)?;
        let m: Message = serde_json::from_str(&envelope.message)?;
        ensure!(
            m.schema_version == 1
                && m.event_seq > 0
                && m.project_id == c.project_id
                && m.task_id == m.run_id
                && envelope.run_id == m.run_id,
            "invalid observation correlation"
        );
        ensure!(
            [&m.tenant_id, &m.project_id, &m.task_id, &m.run_id]
                .into_iter()
                .all(|s| valid_id(s))
                && [&m.thread_id, &m.brief_id]
                    .into_iter()
                    .all(|s| s.as_deref().is_none_or(valid_id))
                && m.thread_id.is_some() == m.brief_id.is_some(),
            "invalid structural identity"
        );
        let expected = match envelope.kind.as_str() {
            "deliver.attempt.failed.v1" => {
                m.status == "failed"
                    && ["attempt_failed", "credential_refresh_required"]
                        .contains(&m.reason.as_str())
            }
            "deliver.preview.held.v1" => m.status == "held" && m.reason == "preview_held",
            "deliver.worker.interrupted.v1" => {
                m.status == "interrupted" && m.reason == "worker_interrupted"
            }
            _ => false,
        };
        ensure!(
            expected
                && envelope.severity == "high"
                && envelope.project_id == self.telemetry_project_id
                && envelope.thread_id == self.diagnostic_thread_id,
            "invalid diagnostic observation envelope"
        );
        ensure!(
            envelope.source_ref == format!("deliver:run:{}:event:{}", m.run_id, m.event_seq),
            "invalid event source reference"
        );
        ensure!(
            key == format!(
                "deliver:{}",
                crate::store::hash(
                    format!("{}:{}:{}", m.project_id, m.task_id, m.event_seq).as_bytes()
                )
            ),
            "invalid observation idempotency identity"
        );
        Ok(())
    }
}

pub async fn serve(root: &std::path::Path, config: &std::path::Path) -> Result<()> {
    use fs2::FileExt;
    let trusted = Trusted::load(config)?;
    let db = Store::open(root)?;
    // Register before publishing the service lock so startup cannot race shutdown.
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("operational-observations-exporter.lock"))?;
    lock.try_lock_exclusive()
        .context("operational observation exporter is already active")?;
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _=interrupt.recv()=>break,
            _=terminate.recv()=>break,
            _=interval.tick()=>{
                if export_tick(&db,&trusted).await.is_err() {
                    eprintln!("Operational observation export unavailable; inspect redacted outbox status");
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn immutable_configuration_is_private_pinned_and_bounded() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("exporter.json");
        let value = json!({"schema_version":1,"endpoint":"http://signals:8080/v1/observations","token_file":"/run/system/telemetry/token","tenant_id":"foundry","telemetry_project_id":"system-operations","diagnostic_thread_id":"foundry-operations","project_ids":["source-project"]});
        std::fs::write(&path, value.to_string()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Trusted::load(&path).is_ok());
        for (field, wrong) in [
            ("schema_version", json!(2)),
            ("endpoint", json!("https://evil.example/v1/observations")),
            ("token_file", json!("/private/other-token")),
            ("tenant_id", json!(":tenant")),
            ("telemetry_project_id", json!("other")),
            ("project_ids", json!([])),
            ("project_ids", json!(["source-project", "source-project"])),
            ("unknown", json!(true)),
        ] {
            let mut changed = value.clone();
            changed[field] = wrong;
            std::fs::write(&path, changed.to_string()).unwrap();
            assert!(Trusted::load(&path).is_err());
        }
        std::fs::write(&path, "x".repeat(16385)).unwrap();
        assert!(Trusted::load(&path).is_err());
        std::fs::write(&path, value.to_string()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Trusted::load(&path).is_err());
    }
}
