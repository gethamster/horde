//! Reverse mTLS control transport. Durable identities remain in federation and management stores.
use crate::{
    federation::{PeerIdentity, wire},
    network::NetworkConfig,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
type Reply = oneshot::Sender<Result<Value>>;
struct Command {
    method: String,
    args: Value,
    reply: Reply,
}
#[derive(Clone)]
struct Connection {
    generation: String,
    sender: mpsc::Sender<Command>,
}
type Registry = Mutex<HashMap<(String, String), Connection>>;
fn registry() -> &'static Registry {
    static CONNECTIONS: OnceLock<Registry> = OnceLock::new();
    CONNECTIONS.get_or_init(|| Mutex::new(HashMap::new()))
}
pub fn connected(config: &NetworkConfig, peer: &str) -> Result<bool> {
    Ok(registry()
        .lock()
        .map_err(|_| anyhow::anyhow!("control registry unavailable"))?
        .get(&(config.runtime_id.clone(), peer.into()))
        .is_some_and(|connection| !connection.sender.is_closed()))
}

pub async fn call(
    config: &NetworkConfig,
    peer: &str,
    method: &str,
    args: &Value,
) -> Result<Option<Value>> {
    let connection = registry()
        .lock()
        .map_err(|_| anyhow::anyhow!("control registry unavailable"))?
        .get(&(config.runtime_id.clone(), peer.into()))
        .cloned();
    let Some(connection) = connection else {
        return Ok(None);
    };
    let (reply, receive) = oneshot::channel();
    tokio::time::timeout(Duration::from_secs(config.timeout_seconds), async {
        connection
            .sender
            .send(Command {
                method: method.into(),
                args: args.clone(),
                reply,
            })
            .await
            .context("remote control disconnected")?;
        receive.await.context("remote control disconnected")?
    })
    .await
    .context("remote control timed out")?
    .map(Some)
}
pub type Stream =
    tokio_stream::wrappers::ReceiverStream<std::result::Result<wire::CallReply, tonic::Status>>;
pub async fn accept(
    root: &std::path::Path,
    config: &NetworkConfig,
    request: tonic::Request<tonic::Streaming<wire::CallRequest>>,
) -> std::result::Result<tonic::Response<Stream>, tonic::Status> {
    let peer = request
        .extensions()
        .get::<PeerIdentity>()
        .ok_or_else(|| tonic::Status::unauthenticated("enrolled certificate required"))?
        .0
        .clone();
    let fingerprint = request
        .extensions()
        .get::<crate::federation::PeerCertificate>()
        .ok_or_else(|| tonic::Status::unauthenticated("worker certificate required"))?
        .0
        .clone();
    let mut input = request.into_inner();
    let first = tokio::time::timeout(Duration::from_secs(10), input.message())
        .await
        .map_err(|_| tonic::Status::deadline_exceeded("enrollment handshake timed out"))??
        .ok_or_else(|| tonic::Status::invalid_argument("initial control frame missing"))?;
    let hello: Value = serde_json::from_str(&first.json)
        .map_err(|_| tonic::Status::invalid_argument("invalid control handshake"))?;
    let db = crate::store::Store::open(root)
        .map_err(|_| tonic::Status::internal("store unavailable"))?;
    let managed = crate::enrollment::activate(&db, &peer, hello["enrollment_token"].as_str())
        .map_err(|_| tonic::Status::permission_denied("enrollment rejected"))?;
    if !config.delegate_peers.contains(&peer) && !managed {
        return Err(tonic::Status::permission_denied(
            "peer is not approved for reverse delegation",
        ));
    }

    let heartbeat_ack_requested = hello["heartbeat_ack"].as_bool() == Some(true);
    let key = (config.runtime_id.clone(), peer);
    let generation = crate::store::id();
    let (commands, mut receive) = mpsc::channel::<Command>(32);
    let (send, output) = mpsc::channel(32);
    registry()
        .lock()
        .map_err(|_| tonic::Status::internal("control registry unavailable"))?
        .insert(
            key.clone(),
            Connection {
                generation: generation.clone(),
                sender: commands,
            },
        );
    let presence_root = root.to_owned();
    tokio::spawn(async move {
        let mut pending: BTreeMap<String, Reply> = BTreeMap::new();
        let mut heartbeat = tokio::time::Instant::now();
        let mut timer = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                command=receive.recv()=>{
                    let Some(command)=command else{break};
                    pending.retain(|_,reply|!reply.is_closed());
                    if pending.len()>=32{let _=command.reply.send(Err(anyhow::anyhow!("remote control at capacity")));continue;}
                    let id=crate::store::id();pending.insert(id.clone(),command.reply);
                    if send.send(Ok(wire::CallReply{json:json!({"id":id,"method":command.method,"args":command.args}).to_string()})).await.is_err(){break;}
                },
                frame=input.message()=>{
                    let Ok(Some(frame))=frame else{break};heartbeat=tokio::time::Instant::now();
                    if frame.method=="heartbeat"
                        && let Ok(Some(name))=presence(&presence_root,&key.1,&frame.json)
                        && heartbeat_ack_requested {
                            let Ok(packet)=heartbeat_ack(&presence_root,&key.1,&name) else {continue;};
                            if send.send(Ok(wire::CallReply{json:packet.to_string()})).await.is_err(){break;}
                        }

                    if frame.method=="reply"
                        && let Ok(v)=serde_json::from_str::<Value>(&frame.json)
                            && let Some(id)=v["id"].as_str()&& let Some(reply)=pending.remove(id){
                                let result=if let Some(error)=v["result"].get("error"){Err(anyhow::anyhow!("{}",error.as_str().unwrap_or("remote failure")))}else{Ok(v["result"].clone())};let _=reply.send(result);
                            }
                },
                _=timer.tick()=>{
                    if heartbeat.elapsed()>Duration::from_secs(30){break;}
                    if managed {let active=crate::store::Store::open(&presence_root).and_then(|db|Ok(crate::fleet_enrollment::authority::is_active(&db,&key.1)? && crate::enrollment::identity(&db,&fingerprint)?.as_deref()==Some(key.1.as_str()))).unwrap_or(false);if !active{break;}}
                }
            }
        }
        if let Ok(mut registry) = registry().lock()
            && registry
                .get(&key)
                .is_some_and(|c| c.generation == generation)
        {
            registry.remove(&key);
        }
    });
    Ok(tonic::Response::new(
        tokio_stream::wrappers::ReceiverStream::new(output),
    ))
}
fn heartbeat_ack(root: &std::path::Path, runtime: &str, name: &str) -> Result<Value> {
    let db = crate::store::Store::open(root)?;
    let conflict = match crate::runtime_directory::resolve(&db, name) {
        Ok(resolved) if resolved == runtime => None,
        Ok(_) => Some(format!(
            "Runtime name {name} selects another worker; choose a unique --name or rename this runtime on the controller"
        )),
        Err(error) => Some(error.to_string()),
    };
    Ok(
        json!({"heartbeat_ack":{"name":name,"runtime":runtime,"name_usable":conflict.is_none(),"name_conflict":conflict}}),
    )
}

struct ControllerPresence {
    root: PathBuf,
    peer: String,
    generation: String,
}
impl ControllerPresence {
    fn new(root: &std::path::Path, peer: &str) -> Result<Self> {
        let presence = Self {
            root: root.into(),
            peer: peer.into(),
            generation: crate::store::id(),
        };
        let db = crate::store::Store::open(root)?;
        db.atomic(|| {
            db.conn.execute("DELETE FROM runtime_settings WHERE key IN ('controller_connected_at','controller_connected_pid','controller_connected_peer','controller_connected_name','controller_connected_name_usable','controller_connected_name_conflict')",[])?;
            crate::management::set(&db,"controller_connected_generation",&presence.generation)
        })?;
        Ok(presence)
    }
    fn acknowledge(&self, packet: &Value, runtime: &str) -> Result<()> {
        ensure!(
            packet["runtime"].as_str() == Some(runtime),
            "controller acknowledged a different worker identity"
        );
        let name = packet["name"]
            .as_str()
            .context("controller acknowledgement missing worker name")?;
        crate::runtime_directory::validate_name(name)?;
        let usable = packet["name_usable"].as_bool().unwrap_or(false);
        let conflict = if usable {
            ""
        } else {
            packet["name_conflict"].as_str().unwrap_or(
                "Controller did not confirm this name is usable; update the controller and retry",
            )
        };
        ensure!(
            conflict.len() <= 1024 && !conflict.chars().any(char::is_control),
            "invalid controller naming acknowledgement"
        );
        let db = crate::store::Store::open(&self.root)?;
        db.atomic(|| {
            ensure!(
                crate::management::value(&db, "controller_connected_generation")?.as_deref()
                    == Some(&self.generation),
                "controller acknowledgement belongs to an old connection"
            );
            for (key, value) in [
                ("controller_connected_at", crate::store::now().to_string()),
                ("controller_connected_pid", std::process::id().to_string()),
                ("controller_connected_peer", self.peer.clone()),
                ("controller_connected_name", name.into()),
                ("controller_connected_name_usable", usable.to_string()),
                ("controller_connected_name_conflict", conflict.into()),
                ("controller_connected_generation", self.generation.clone()),
            ] {
                crate::management::set(&db, key, &value)?;
            }
            Ok(())
        })
    }
    fn clear(&self) -> Result<()> {
        let db = crate::store::Store::open(&self.root)?;
        db.atomic(|| {
            if crate::management::value(&db,"controller_connected_generation")?.as_deref()==Some(&self.generation) {
                db.conn.execute("DELETE FROM runtime_settings WHERE key IN ('controller_connected_at','controller_connected_pid','controller_connected_peer','controller_connected_name','controller_connected_name_usable','controller_connected_name_conflict','controller_connected_generation')",[])?;
            }
            Ok(())
        })
    }
}
impl Drop for ControllerPresence {
    fn drop(&mut self) {
        if let Err(error) = self.clear() {
            eprintln!("Could not clear controller connection status: {error:#}");
        }
    }
}

pub async fn connect(root: PathBuf, config: NetworkConfig) -> Result<()> {
    let peer = config
        .controller_peer
        .clone()
        .context("controller peer missing")?;
    ensure!(
        config.execution_clients.contains(&peer) || config.management_clients.contains(&peer),
        "controller needs an explicit execution or management grant"
    );
    let channel = crate::network::channel(&config, &peer).await?;
    let mut client = wire::federation_client::FederationClient::new(channel)
        .max_decoding_message_size(64 * 1024 * 1024)
        .max_encoding_message_size(64 * 1024 * 1024);
    let (send, receive) = mpsc::channel(32);
    send.send(wire::CallRequest {
        method: "heartbeat".into(),
        json: json!({"enrollment_token":config.enrollment_token,"heartbeat_ack":true}).to_string(),
    })
    .await?;
    let mut stream = client
        .control(tokio_stream::wrappers::ReceiverStream::new(receive))
        .await?
        .into_inner();
    let presence = ControllerPresence::new(&root, &peer)?;
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            _=interval.tick()=>{
                if root.join("fleet-worker.json").exists() {
                    let current = NetworkConfig::load(Some(&root.join("managed-network.toml")))?;
                    ensure!(current.identity_cert == config.identity_cert, "worker certificate renewed; reconnecting");
                }
                send.send(wire::CallRequest{method:"heartbeat".into(),json:heartbeat_packet(&root).unwrap_or_else(|_|"{}".into())}).await?;
            },
            frame=stream.message()=>{
                let frame=frame?.context("controller disconnected")?;let packet:Value=serde_json::from_str(&frame.json)?;
                if let Some(ack)=packet.get("heartbeat_ack") {
                    presence.acknowledge(ack,&config.runtime_id)?;
                    continue;
                }
                let root=root.clone();let config=config.clone();let peer=peer.clone();let send=send.clone();
                tokio::spawn(async move {
                    let id=packet["id"].clone();
                    let result=tokio::task::spawn_blocking(move||crate::federation::handle_control(root,config,&peer,&packet)).await;
                    let value=match result{Ok(Ok(v))=>v,Ok(Err(e))=>json!({"error":e.to_string()}),Err(_)=>json!({"error":"remote control handler failed"})};
                    let _=send.send(wire::CallRequest{method:"reply".into(),json:json!({"id":id,"result":value}).to_string()}).await;
                });
            }
        }
    }
}

fn heartbeat_packet(root: &std::path::Path) -> Result<String> {
    let db = crate::store::Store::open(root)?;
    let configured = crate::config::Settings::load_user()?;
    let explicit: std::collections::BTreeSet<_> = configured
        .executors
        .values()
        .filter_map(|c| c.account.clone())
        .collect();
    let mut accounts = vec![];
    for row in db.rows("SELECT * FROM account_capacity", &[])? {
        let id = row["account"].as_str().context("account")?;
        accounts.push(json!({"snapshot":{"account":id,"provider":row["provider"],"window":row["window"],"used_percent":row["used"],"reset_at":row["reset"],"observed_at":row["observed"],"source":row["source"]},"shared":explicit.contains(id)}));
    }
    Ok(json!({"status":crate::management::status(&db)?,"accounts":accounts,"name":crate::runtime_directory::local_name(&db)?,"capabilities":crate::capabilities::local(&db)?}).to_string())
}
fn presence(root: &std::path::Path, peer: &str, packet: &str) -> Result<Option<String>> {
    ensure!(packet.len() <= 128 * 1024, "heartbeat too large");
    let value: Value = serde_json::from_str(packet)?;
    let db = crate::store::Store::open(root)?;
    if let Some(name) = value.get("name").and_then(Value::as_str) {
        crate::runtime_directory::observe_name(&db, peer, name)?;
    }
    if let Some(capabilities) = value.get("capabilities") {
        crate::capabilities::observe(&db, peer, capabilities)?;
    }
    if let Some(status) = value.get("status") {
        db.conn.execute("INSERT INTO runtime_presence VALUES(?,?,?) ON CONFLICT(runtime) DO UPDATE SET observed=excluded.observed,status=excluded.status",rusqlite::params![peer,crate::store::now(),status.to_string()])?;
        if let Some(version) = status["version"].as_str() {
            db.conn.execute(
                "UPDATE managed_runtimes SET version=? WHERE id=?",
                rusqlite::params![version, peer],
            )?;
        }
    }
    for entry in value["accounts"].as_array().into_iter().flatten().take(128) {
        let mut snapshot: crate::capacity::Snapshot =
            serde_json::from_value(entry["snapshot"].clone())?;
        if entry["shared"] != true {
            snapshot.account = format!("remote:{peer}:{}", snapshot.account);
        }
        crate::capacity::observe(&db, &snapshot)?;
    }
    Ok(value
        .get("name")
        .and_then(Value::as_str)
        .filter(|_| value.get("status").is_some())
        .map(str::to_owned))
}

#[cfg(test)]
mod connection_presence_tests {
    use super::*;
    #[test]
    fn connection_status_is_process_bound_and_removed_on_disconnect() {
        let root = tempfile::tempdir().unwrap();
        let db = crate::store::Store::open(root.path()).unwrap();
        let first = ControllerPresence::new(root.path(), "controller").unwrap();
        assert!(
            crate::management::value(&db, "controller_connected_at")
                .unwrap()
                .is_none()
        );
        first
            .acknowledge(
                &json!({"runtime":"worker","name":"apollo","name_usable":true}),
                "worker",
            )
            .unwrap();
        assert_eq!(
            crate::management::value(&db, "controller_connected_name")
                .unwrap()
                .as_deref(),
            Some("apollo")
        );
        assert_eq!(
            crate::management::value(&db, "controller_connected_pid").unwrap(),
            Some(std::process::id().to_string())
        );
        assert_eq!(
            crate::management::value(&db, "controller_connected_peer")
                .unwrap()
                .as_deref(),
            Some("controller")
        );
        assert_eq!(
            crate::management::value(&db, "controller_connected_name_usable")
                .unwrap()
                .as_deref(),
            Some("true")
        );
        first.acknowledge(&json!({"runtime":"worker","name":"apollo","name_usable":false,"name_conflict":"ambiguous name"}),"worker").unwrap();
        assert_eq!(
            crate::management::value(&db, "controller_connected_name_conflict")
                .unwrap()
                .as_deref(),
            Some("ambiguous name")
        );
        let next = ControllerPresence::new(root.path(), "controller").unwrap();
        assert!(
            crate::management::value(&db, "controller_connected_at")
                .unwrap()
                .is_none()
        );
        assert!(
            next.acknowledge(
                &json!({"runtime":"another-worker","name":"apollo"}),
                "worker"
            )
            .is_err()
        );
        assert!(
            crate::management::value(&db, "controller_connected_at")
                .unwrap()
                .is_none()
        );
        assert!(
            first
                .acknowledge(&json!({"runtime":"worker","name":"old-name"}), "worker")
                .is_err()
        );
        next.acknowledge(
            &json!({"runtime":"worker","name":"new-name","name_usable":true}),
            "worker",
        )
        .unwrap();
        drop(first);
        assert!(
            crate::management::value(&db, "controller_connected_at")
                .unwrap()
                .is_some()
        );
        drop(next);
        assert!(
            crate::management::value(&db, "controller_connected_at")
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn failed_or_unnamed_presence_never_produces_a_readiness_ack() {
        let root = tempfile::tempdir().unwrap();
        let db = crate::store::Store::open(root.path()).unwrap();
        db.conn.execute("INSERT INTO runtime_enrollments VALUES('worker','fingerprint','',9999999999,'active')",[]).unwrap();
        assert!(presence(root.path(), "worker", r#"{"name":"bad name","status":{}}"#).is_err());
        assert_eq!(
            presence(root.path(), "worker", r#"{"status":{}}"#).unwrap(),
            None
        );
        assert!(
            presence(
                root.path(),
                "worker",
                r#"{"name":"apollo","status":{},"accounts":[{"snapshot":false}]}"#
            )
            .is_err()
        );
        assert_eq!(
            presence(root.path(), "worker", r#"{"name":"apollo","status":{}}"#).unwrap(),
            Some("apollo".into())
        );
    }
    #[test]
    fn readiness_ack_reports_duplicate_names_and_controller_alias_conflicts() {
        let root = tempfile::tempdir().unwrap();
        let db = crate::store::Store::open(root.path()).unwrap();
        for id in ["ts-one", "ts-two"] {
            db.conn
                .execute(
                    "INSERT INTO runtime_enrollments VALUES(?1,?1,'',9999999999,'active')",
                    [id],
                )
                .unwrap();
            crate::runtime_directory::observe_name(&db, id, "apollo").unwrap();
        }
        let conflict = heartbeat_ack(root.path(), "ts-one", "apollo").unwrap();
        assert_eq!(conflict["heartbeat_ack"]["name_usable"], false);
        assert!(
            conflict["heartbeat_ack"]["name_conflict"]
                .as_str()
                .unwrap()
                .contains("ambiguous")
        );
        crate::runtime_directory::rename(&db, "ts-one", "build-mac").unwrap();
        assert_eq!(
            heartbeat_ack(root.path(), "ts-one", "apollo").unwrap()["heartbeat_ack"]["name_usable"],
            false
        );
        assert_eq!(
            heartbeat_ack(root.path(), "ts-one", "build-mac").unwrap()["heartbeat_ack"]["name_usable"],
            true
        );
    }
}
