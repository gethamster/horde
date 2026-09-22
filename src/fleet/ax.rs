//! Experimental stock Google AX integration. Management endpoints are explicit;
//! enrollment credentials travel only in the runner's private bootstrap body.
use super::{Profile, ax_spec as spec, project_host::bootstrap_path};
use crate::{management, store::Store};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::time::Duration;
use tonic::{Code, transport::Channel};

pub const PINNED_REVISION: &str = "d8ed0fe38bceb7842d3c47817d53d16ccdfcb601";
#[doc(hidden)]
pub mod wire {
    tonic::include_proto!("ax.v1alpha1");
}
use wire::{ax_client::AxClient, *};
type Client = AxClient<Channel>;

pub(super) fn validate(p: &Profile) -> Result<()> {
    ensure!(
        p.host.is_none(),
        "AX profiles connect directly to the AX API"
    );
    ensure!(
        p.ax_revision == PINNED_REVISION,
        "unsupported experimental AX revision"
    );
    let digest = p
        .image
        .rsplit_once("@sha256:")
        .context("AX runner image must be pinned by digest")?
        .1;
    ensure!(
        digest.len() == 64 && digest.bytes().all(|c| c.is_ascii_hexdigit()),
        "invalid AX runner image digest"
    );
    for value in [&p.endpoint, &p.ax_router_endpoint] {
        let url =
            reqwest::Url::parse(value).context("AX requires explicit API and router endpoints")?;
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/",
            "AX endpoint must be an origin without credentials"
        );
        ensure!(
            url.scheme() == "https"
                || (url.scheme() == "http"
                    && [Some("127.0.0.1"), Some("localhost"), Some("[::1]")]
                        .contains(&url.host_str())),
            "AX endpoints require HTTPS or a loopback tunnel"
        );
    }
    ensure!(
        !p.project.is_empty()
            && p.project.len() <= 56
            && p.project
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
        "invalid AX project namespace"
    );
    for value in &p.ax_egress {
        spec::rule(value)?;
    }
    Ok(())
}
async fn client(p: &Profile) -> Result<Client> {
    let endpoint = tonic::transport::Endpoint::from_shared(p.endpoint.clone())?
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30));
    let endpoint = if p.endpoint.starts_with("https://") {
        endpoint.tls_config(tonic::transport::ClientTlsConfig::new().with_webpki_roots())?
    } else {
        endpoint
    };
    Ok(
        Client::new(endpoint.connect().await.context("AX connection failed")?)
            .max_decoding_message_size(4 * 1024 * 1024),
    )
}
fn rpc(error: tonic::Status) -> anyhow::Error {
    // Remote error bodies may contain specifications or credentials. Persist only codes.
    anyhow::anyhow!("AX API returned {}", error.code())
}
fn request<T>(p: &Profile, body: T) -> Result<tonic::Request<T>> {
    let mut request = tonic::Request::new(body);
    if !p.api_key_env.is_empty() {
        let key =
            crate::config::credential(&p.api_key_env).context("AX API credential unavailable")?;
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {key}")
                .parse()
                .context("invalid AX API credential")?,
        );
    }
    Ok(request)
}
fn owner(db: &Store, id: &str) -> Result<String> {
    management::value(db, &format!("ax_owner:{id}"))?.context("AX ownership record unavailable")
}
fn resource(id: &str, owner: &str) -> String {
    format!(
        "horde-{}-{}",
        &id[..id.len().min(32)],
        &crate::store::hash(owner.as_bytes())[..12]
    )
}
fn packet(db: &Store, p: &Profile, id: &str) -> Result<Value> {
    Ok(serde_json::from_slice(&std::fs::read(bootstrap_path(
        db, &p.project, id,
    )?)?)?)
}
async fn get(client: &mut Client, p: &Profile, name: &str) -> Result<Option<Task>> {
    match client
        .get_task(request(
            p,
            GetTaskRequest {
                atespace: spec::atespace(p),
                name: name.into(),
            },
        )?)
        .await
    {
        Ok(value) => Ok(Some(value.into_inner())),
        Err(e) if e.code() == Code::NotFound => Ok(None),
        Err(e) => Err(rpc(e)),
    }
}
async fn ensure_aux(client: &mut Client, p: &Profile, name: &str, bootstrap: &Value) -> Result<()> {
    let expected = spec::workspace(p, name);
    match client
        .get_workspace(request(
            p,
            GetWorkspaceRequest {
                atespace: spec::atespace(p),
                name: name.into(),
            },
        )?)
        .await
    {
        Ok(actual) => ensure!(
            actual.into_inner() == expected,
            "AX workspace specification mismatch"
        ),
        Err(e) if e.code() == Code::NotFound => {
            client
                .update_workspace(request(
                    p,
                    UpdateWorkspaceRequest {
                        workspace: Some(expected),
                    },
                )?)
                .await
                .map_err(rpc)?;
        }
        Err(e) => return Err(rpc(e)),
    }
    let expected = spec::gateway(p, name, bootstrap)?;
    match client
        .get_gateway(request(
            p,
            GetGatewayRequest {
                atespace: spec::atespace(p),
                name: name.into(),
            },
        )?)
        .await
    {
        Ok(actual) => ensure!(
            actual.into_inner() == expected,
            "AX gateway specification mismatch"
        ),
        Err(e) if e.code() == Code::NotFound => {
            client
                .update_gateway(request(
                    p,
                    UpdateGatewayRequest {
                        gateway: Some(expected),
                    },
                )?)
                .await
                .map_err(rpc)?;
        }
        Err(e) => return Err(rpc(e)),
    }
    Ok(())
}
fn running(task: &Task) -> bool {
    task.status.as_ref().is_some_and(|status| {
        status.phase == "Running"
            && status
                .conditions
                .iter()
                .any(|c| c.r#type == "Ready" && c.status == "True")
    })
}
async fn bootstrap(p: &Profile, name: &str, packet: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(packet)?;
    ensure!(bytes.len() <= 128 * 1024, "AX bootstrap packet too large");
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()?;
    let response = client
        .post(format!(
            "{}/bootstrap",
            p.ax_router_endpoint.trim_end_matches('/')
        ))
        .header("ate-target-actor", format!("{}/{name}", spec::atespace(p)))
        .header("Content-Type", "application/json")
        .body(bytes)
        .send()
        .await
        .context("AX runner bootstrap request failed")?;
    ensure!(
        [reqwest::StatusCode::OK, reqwest::StatusCode::ACCEPTED].contains(&response.status()),
        "AX runner bootstrap returned {}",
        response.status()
    );
    Ok(())
}
/// Public for backend contract tests; normal callers use durable fleet operations.
#[doc(hidden)]
pub async fn provision(
    db: &Store,
    p: &Profile,
    id: &str,
    bootstrap_packet: Option<&Value>,
) -> Result<String> {
    validate(p)?;
    let packet = bootstrap_packet.context("AX requires controller enrollment")?;
    ensure!(
        packet["id"] == id && packet["project"]["id"] == p.project,
        "AX bootstrap identity mismatch"
    );
    let owner = db.atomic(|| {
        let key = format!("ax_owner:{id}");
        if let Some(value) = management::value(db, &key)? {
            return Ok(value);
        }
        let value = crate::store::id();
        management::set(db, &key, &value)?;
        Ok(value)
    })?;
    let name = resource(id, &owner);
    // Commit the recovery handle before the first external write.
    ensure!(db.conn.execute("UPDATE managed_runtimes SET resource=? WHERE id=? AND (resource IS NULL OR resource=?)",rusqlite::params![name,id,name])? == 1, "AX managed resource identity mismatch");
    let expected = spec::task(p, id, &name, &owner);
    let mut client = client(p).await?;
    if let Some(existing) = get(&mut client, p, &name).await? {
        spec::verify_task(&existing, &expected)?;
    }
    ensure_aux(&mut client, p, &name, packet).await?;
    if get(&mut client, p, &name).await?.is_none() {
        client
            .update_task(request(
                p,
                UpdateTaskRequest {
                    task: Some(expected.clone()),
                },
            )?)
            .await
            .map_err(rpc)?;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        let task = get(&mut client, p, &name)
            .await?
            .context("AX task disappeared during provisioning")?;
        spec::verify_task(&task, &expected)?;
        if running(&task) {
            bootstrap(p, &name, packet).await?;
            return Ok(name);
        }
        ensure!(
            task.status.as_ref().is_none_or(|s| s.phase != "Failed"),
            "AX task failed; inspect AX and reconcile retained resource"
        );
        ensure!(
            tokio::time::Instant::now() < deadline,
            "AX task is not ready; reconcile retained resource"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
fn waiting(reason: &str) -> Value {
    json!({"lifecycle_pending":true,"state":"waiting","reason":reason})
}
async fn manage(db: &Store, id: &str, action: &str) -> Result<Value> {
    crate::federation::call(
        &crate::federation::config(db)?,
        id,
        "manage",
        json!({"action":action}),
    )
    .await
}
fn new_delete_request(db: &Store, id: &str, terminating: bool) -> Result<bool> {
    let rows = db.rows("SELECT id FROM runtime_operations WHERE runtime=? AND action='runtime_destroy' AND state='running' ORDER BY created DESC,rowid DESC LIMIT 1", &[&id])?;
    let Some(request) = rows.first().and_then(|row| row["id"].as_str()) else {
        return Ok(!terminating);
    };
    let key = format!("ax_delete_request:{id}");
    if management::value(db, &key)?.as_deref() == Some(request) {
        return Ok(false);
    }
    // Persist intent before sending. A lost reply is reconciled rather than replayed.
    management::set(db, &key, request)?;
    Ok(true)
}
async fn delete_aux(client: &mut Client, p: &Profile, name: &str, bootstrap: &Value) -> Result<()> {
    // The task's deletion does not authorize deleting replacement resources.
    let expected = spec::gateway(p, name, bootstrap)?;
    match client
        .get_gateway(request(
            p,
            GetGatewayRequest {
                atespace: spec::atespace(p),
                name: name.into(),
            },
        )?)
        .await
    {
        Ok(actual) => ensure!(
            actual.into_inner() == expected,
            "AX gateway specification mismatch"
        ),
        Err(e) if e.code() == Code::NotFound => {}
        Err(e) => return Err(rpc(e)),
    }
    let expected = spec::workspace(p, name);
    match client
        .get_workspace(request(
            p,
            GetWorkspaceRequest {
                atespace: spec::atespace(p),
                name: name.into(),
            },
        )?)
        .await
    {
        Ok(actual) => ensure!(
            actual.into_inner() == expected,
            "AX workspace specification mismatch"
        ),
        Err(e) if e.code() == Code::NotFound => {}
        Err(e) => return Err(rpc(e)),
    }
    let a = spec::atespace(p);
    match client
        .delete_gateway(request(
            p,
            DeleteGatewayRequest {
                atespace: a.clone(),
                name: name.into(),
            },
        )?)
        .await
    {
        Ok(_) => {}
        Err(e) if e.code() == Code::NotFound => {}
        Err(e) => return Err(rpc(e)),
    }
    match client
        .delete_workspace(request(
            p,
            DeleteWorkspaceRequest {
                atespace: a,
                name: name.into(),
            },
        )?)
        .await
    {
        Ok(_) => {}
        Err(e) if e.code() == Code::NotFound => {}
        Err(e) => return Err(rpc(e)),
    }
    Ok(())
}
#[doc(hidden)]
pub async fn lifecycle(
    db: &Store,
    p: &Profile,
    id: &str,
    name: &str,
    action: &str,
) -> Result<Value> {
    validate(p)?;
    let owner = owner(db, id)?;
    ensure!(
        resource(id, &owner) == name,
        "AX resource ownership mismatch"
    );
    let expected = spec::task(p, id, name, &owner);
    let mut client = client(p).await?;
    let task = get(&mut client, p, name).await?;
    if let Some(task) = &task {
        spec::verify_task(task, &expected)?;
    }
    if action == "runtime_reconcile" {
        let Some(task) = task else {
            return Ok(json!({"resource":name,"state":"uncertain","absent":true}));
        };
        if running(&task) {
            bootstrap(p, name, &packet(db, p, id)?).await?;
        }
        let state = if task.status.as_ref().is_some_and(|s| s.phase == "Suspended") {
            "stopped"
        } else if running(&task) {
            "provisioned"
        } else {
            "uncertain"
        };
        return Ok(
            json!({"resource":name,"state":state,"phase":task.status.map(|s|s.phase),"isolation":"gvisor"}),
        );
    }
    if action == "runtime_destroy" && task.is_none() {
        delete_aux(&mut client, p, name, &packet(db, p, id)?).await?;
        return Ok(json!({"resource":name,"state":"removed"}));
    }
    let task = task.context("AX task is absent; reconcile before continuing")?;
    let atespace = spec::atespace(p);
    match action {
        "runtime_stop" | "runtime_destroy" => {
            management::set(db, &format!("ax_hold:{id}"), "true")?;
            let suspended = task.status.as_ref().is_some_and(|s| s.phase == "Suspended");
            let terminating = task
                .status
                .as_ref()
                .is_some_and(|s| s.phase == "Terminating");
            let never_activated = db.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM runtime_enrollments WHERE runtime=? AND state='pending')",
                [id], |row| row.get::<_, bool>(0),
            )?;
            if !suspended && !terminating && !(action == "runtime_destroy" && never_activated) {
                match manage(db, id, "runtime_drain").await {
                    Ok(status) if status["drained"] == true => {}
                    _ => return Ok(waiting("waiting for authenticated Horde worker to drain")),
                }
            }
            if action == "runtime_stop" {
                if suspended {
                    return Ok(json!({"resource":name,"state":"stopped"}));
                }
                if task.spec.as_ref().is_some_and(|spec| spec.suspend) {
                    return Ok(waiting("waiting for AX suspension"));
                }
                client
                    .suspend_task(request(
                        p,
                        SuspendTaskRequest {
                            atespace,
                            name: name.into(),
                        },
                    )?)
                    .await
                    .map_err(rpc)?;
                Ok(waiting("waiting for AX suspension"))
            } else {
                if new_delete_request(db, id, terminating)? {
                    client
                        .delete_task(request(
                            p,
                            DeleteTaskRequest {
                                atespace,
                                name: name.into(),
                            },
                        )?)
                        .await
                        .map_err(rpc)?;
                }
                Ok(waiting("waiting for AX actor deletion"))
            }
        }
        "runtime_start" => {
            management::set(db, &format!("ax_hold:{id}"), "true")?;
            if task.spec.as_ref().is_some_and(|s| s.suspend) {
                client
                    .resume_task(request(
                        p,
                        ResumeTaskRequest {
                            atespace,
                            name: name.into(),
                        },
                    )?)
                    .await
                    .map_err(rpc)?;
                return Ok(waiting("waiting for AX resume"));
            }
            if !running(&task) {
                return Ok(waiting("waiting for AX runner readiness"));
            }
            bootstrap(p, name, &packet(db, p, id)?).await?;
            let status = match manage(db, id, "runtime_resume").await {
                Ok(v) => v,
                Err(_) => return Ok(waiting("waiting for authenticated Horde worker readiness")),
            };
            ensure!(
                status["pid"].as_u64().is_some_and(|pid| pid > 0),
                "AX worker did not report authenticated readiness"
            );
            management::set(db, &format!("ax_hold:{id}"), "false")?;
            Ok(json!({"resource":name,"state":"provisioned"}))
        }
        _ => bail!("unsupported AX lifecycle operation"),
    }
}
