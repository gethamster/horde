//! Loopback authority only. Never contacts a live provider or owning service.
use horde::{
    store::{Store, now},
    upstream_maintenance::{Binding, Policy},
};
use serde_json::json;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
pub async fn bind(
    db: &Store,
    task: &str,
    repo: &Path,
    calls: u64,
    input: u64,
    output: u64,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let expires = now() + 3600;
    let budget = json!({"reference":"shared-decision-budget","max_model_calls":calls,"max_input_bytes":input,"max_output_bytes":output});
    let binding: Binding = serde_json::from_value(
        json!({"schema_version":1,"policy_id":"decision-maintenance","policy_revision":1,"automation_grant_id":"grant-1","routine_id":"routine-1","occurrence_id":"occurrence-1","invocation_id":"invocation-1","collected_until":now()-10,"expires_at":expires,"upstreams":{"vercel/ai":"a".repeat(40),"vercel/chat":"b".repeat(40),"vercel/eve":"c".repeat(40)},"budget":budget}),
    )?;
    let policy: Policy = serde_json::from_value(
        json!({"schema_version":1,"policy_id":binding.policy_id,"policy_revision":1,"project_id":"default","tenant_id":"default","repo":repo.canonicalize()?,"repository":"test/repo","base":"main","automation_grant_id":binding.automation_grant_id,"routine_id":binding.routine_id,"expires_at":expires,"authority_url":format!("http://{address}/authorization"),"authority_token_env":"HORDE_DECISION_MAINTENANCE_FIXTURE_AUTH","budget":budget}),
    )?;
    unsafe {
        std::env::set_var(
            "HORDE_DECISION_MAINTENANCE_FIXTURE_AUTH",
            "fixture-authority-token-at-least-thirty-two-bytes",
        )
    };
    horde::upstream_maintenance::register(db, policy)?;
    horde::upstream_maintenance::pin(db, task, &binding)?;
    let mut reply = serde_json::to_value(binding)?;
    reply["active"] = json!(true);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut request = [0; 4096];
            let _ = socket.read(&mut request).await;
            let body = reply.to_string();
            let _=socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
        }
    });
    Ok(())
}
