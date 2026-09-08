//! Runtime-internal federation. Public callers continue to use CLI/MCP.
use crate::{network::NetworkConfig, store::Store};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::params;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
pub mod wire {
    tonic::include_proto!("task.federation.v1");
}
#[derive(Clone)]
pub struct PeerIdentity(pub String);
#[derive(Clone)]
pub struct Service {
    root: PathBuf,
    config: NetworkConfig,
}
pub fn service(
    root: PathBuf,
    config: NetworkConfig,
    guard: impl tonic::service::Interceptor + Clone + Send + 'static,
) -> tonic::service::interceptor::InterceptedService<
    wire::federation_server::FederationServer<Service>,
    impl tonic::service::Interceptor + Clone + Send + 'static,
> {
    tonic::service::interceptor::InterceptedService::new(
        wire::federation_server::FederationServer::new(Service { root, config })
            .max_decoding_message_size(64 * 1024 * 1024)
            .max_encoding_message_size(64 * 1024 * 1024),
        guard,
    )
}
pub fn config(db: &Store) -> Result<NetworkConfig> {
    let mut config = NetworkConfig::load(Some(&db.root.join("network-runtime.toml")))?;
    for row in db.rows(
        "SELECT runtime FROM runtime_enrollments WHERE state='active'",
        &[],
    )? {
        let id = row["runtime"].as_str().context("runtime")?.to_owned();
        if !config.delegate_peers.contains(&id) {
            config.delegate_peers.push(id);
        }
    }
    Ok(config)
}
pub fn configure(root: &Path, config: &NetworkConfig) -> Result<()> {
    let file = root.join("network-runtime.toml");
    let temp = root.join(format!("network-{}.tmp", crate::store::id()));
    crate::secrets::write_private(&temp, toml::to_string(config)?.as_bytes())?;
    std::fs::rename(temp, file)?;
    Ok(())
}
pub async fn call(config: &NetworkConfig, peer: &str, method: &str, args: Value) -> Result<Value> {
    if let Some(value) = crate::control::call(config, peer, method, &args).await? {
        return Ok(value);
    }
    let channel = crate::network::channel(config, peer).await?;
    let mut client = wire::federation_client::FederationClient::new(channel)
        .max_decoding_message_size(64 * 1024 * 1024)
        .max_encoding_message_size(64 * 1024 * 1024);
    let reply = client
        .call(wire::CallRequest {
            method: method.into(),
            json: serde_json::to_string(&args)?,
        })
        .await?
        .into_inner();
    let value: Value = serde_json::from_str(&reply.json)?;
    if let Some(e) = value.get("error") {
        bail!("{}", e.as_str().unwrap_or("remote error"));
    }
    Ok(value)
}
pub fn call_sync(
    config: NetworkConfig,
    peer: String,
    method: String,
    args: Value,
) -> Result<Value> {
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(call(&config, &peer, &method, args))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("federation client interrupted"))?
}
pub fn forward(db: &Store, oid: &str, name: &str, args: &Value) -> Result<Option<Value>> {
    let origin = db.rows("SELECT * FROM remote_origins WHERE task=?", &[&oid])?;
    let Some(origin) = origin.first() else {
        return Ok(None);
    };
    if name == "integrate_child" {
        return Ok(Some(import_foreign_child(db, oid, args, origin)?));
    }
    let caller_snapshot = if name == "delegate_task" {
        let workspace = if let Some(worker) = args["worker"].as_str() {
            let worker = db.worker(worker)?;
            worker["workspace"]
                .as_str()
                .map(PathBuf::from)
                .unwrap_or(crate::git::task_workspace(db, oid)?)
        } else {
            crate::git::task_workspace(db, oid)?
        };
        ensure!(
            crate::git::run(&workspace, &["status", "--porcelain"])?.is_empty(),
            "commit parent work before delegating"
        );
        Some(snapshot(&workspace)?)
    } else {
        None
    };
    let caller_worker = args["worker"].clone();
    let mut args = args.clone();
    if name == "request_question" && args["id"].is_null() {
        args["id"] = json!(crate::store::hash(
            format!("{}:{}:{}", oid, args["worker"], args["question"]).as_bytes()
        ));
    }
    if name == "add_knowledge" {
        args["provenance"] =
            json!({"remote_task":oid,"remote_step":args["step"],"source":args["provenance"]});
        args.as_object_mut().context("arguments")?.remove("step");
    }
    args.as_object_mut().context("arguments")?.remove("worker");
    args["task"] = origin["owner_task"].clone();
    let value = call_sync(
        config(db)?,
        origin["owner_peer"].as_str().context("owner peer")?.into(),
        "caller_operation".into(),
        json!({"task":origin["owner_task"],"method":name,"args":args,"snapshot":caller_snapshot}),
    )?;
    if name == "delegate_task" && caller_worker.is_string() {
        let key = format!(
            "delegation.caller:{}",
            value["id"].as_str().context("delegated child ID")?
        );
        db.conn.execute(
            "INSERT INTO external_ops VALUES(?,?,'registered',?) ON CONFLICT(task,name) DO NOTHING",
            params![oid, key, json!({"worker":caller_worker}).to_string()],
        )?;
    }
    Ok(Some(value))
}
fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = crate::executor::clean_command("git")
        .args(args)
        .current_dir(repo)
        .output()?;
    ensure!(out.status.success(), "repository transfer command failed");
    ensure!(
        out.stdout.len() <= 24 * 1024 * 1024,
        "repository transfer exceeds 24 MiB limit"
    );
    Ok(out.stdout)
}
pub fn snapshot(repo: &Path) -> Result<Value> {
    let commit = String::from_utf8(git(repo, &["rev-parse", "HEAD"])?)?
        .trim()
        .to_owned();
    let paths = String::from_utf8(git(repo, &["ls-tree", "-r", "--name-only", &commit])?)?;
    ensure!(
        !paths.lines().any(|p| Path::new(p)
            .file_name()
            .is_some_and(|n| n == ".env" || n.to_string_lossy().ends_with(".key"))),
        "repository snapshot contains a tracked secret file"
    );
    let bytes = git(repo, &["archive", "--format=tar", &commit])?;
    Ok(json!({"commit":commit,"hash":crate::store::hash(&bytes),"archive":hex::encode(bytes)}))
}
pub fn unpack(snapshot: &Value, path: &Path) -> Result<()> {
    let bytes = hex::decode(snapshot["archive"].as_str().context("repository archive")?)?;
    ensure!(
        bytes.len() <= 24 * 1024 * 1024 && snapshot["hash"] == crate::store::hash(&bytes),
        "snapshot integrity or size failure"
    );
    std::fs::create_dir_all(path)?;
    let mut archive = tar::Archive::new(bytes.as_slice());
    let mut size = 0u64;
    for entry in archive.entries()? {
        let mut e = entry?;
        if e.header().entry_type().is_pax_global_extensions() {
            continue;
        }
        size += e.size();
        ensure!(
            size <= 64 * 1024 * 1024,
            "expanded snapshot exceeds size limit"
        );
        ensure!(
            e.header().entry_type().is_file() || e.header().entry_type().is_dir(),
            "snapshot links and special files are unsupported"
        );
        let p = e.path()?.into_owned();
        let s = p.to_str().context("UTF-8 repository paths required")?;
        crate::store::scope(s)?;
        ensure!(
            !p.components().any(|p| p.as_os_str() == ".git"),
            "snapshot cannot contain Git metadata"
        );
        ensure!(e.unpack_in(path)?, "snapshot path escapes repository");
    }
    git(path, &["init", "-b", "main"])?;
    git(path, &["config", "user.name", "Horde"])?;
    git(path, &["config", "user.email", "task@localhost"])?;
    git(path, &["add", "."])?;
    git(
        path,
        &["commit", "--allow-empty", "-m", "Delegated source snapshot"],
    )?;
    Ok(())
}
fn bundle_packet(db: &Store, oid: &str, peer: &str, config: &NetworkConfig) -> Result<Value> {
    let mut packets = serde_json::Map::new();
    for bundle in db.rows(
        "SELECT name,version FROM task_bundles WHERE task=?",
        &[&oid],
    )? {
        let name = bundle["name"].as_str().context("bundle")?;
        ensure!(
            config
                .share_bundles
                .get(peer)
                .is_some_and(|v| v.iter().any(|n| n == name)),
            "remote runtime is not approved to receive selected bundle"
        );
        let (version, values) = crate::secrets::load_bundle(name)?;
        ensure!(
            bundle["version"] == version,
            "application bundle changed; refresh it explicitly"
        );
        packets.insert(
            name.into(),
            json!({"version":bundle["version"],"values":values}),
        );
    }
    Ok(Value::Object(packets))
}
fn save_bundles(
    db: &Store,
    oid: &str,
    peer: &str,
    config: &NetworkConfig,
    bundles: &Value,
) -> Result<()> {
    for (name, packet) in bundles.as_object().context("bundles")? {
        ensure!(
            config
                .receive_bundles
                .get(peer)
                .is_some_and(|v| v.contains(name)),
            "caller is not approved to supply bundle"
        );
        let values: BTreeMap<String, String> = serde_json::from_value(packet["values"].clone())?;
        ensure!(
            values.keys().all(|k| !k.starts_with("HORDE_")),
            "reserved environment key"
        );
        let dir = db.root.join("remote-secrets").join(oid);
        std::fs::create_dir_all(&dir)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        let file = dir.join(crate::store::hash(name.as_bytes()));
        let tmp = dir.join(crate::store::id());
        crate::secrets::write_private(&tmp, serde_json::to_string(packet)?.as_bytes())?;
        std::fs::rename(tmp, file)?;
        db.conn.execute("INSERT INTO task_bundles VALUES(?,?,?) ON CONFLICT(task,name) DO UPDATE SET version=excluded.version",params![oid,name,packet["version"].as_str()])?;
    }
    Ok(())
}
#[tonic::async_trait]
impl wire::federation_server::Federation for Service {
    type ControlStream = crate::control::Stream;
    async fn control(
        &self,
        request: tonic::Request<tonic::Streaming<wire::CallRequest>>,
    ) -> std::result::Result<tonic::Response<Self::ControlStream>, tonic::Status> {
        crate::control::accept(&self.root, &self.config, request).await
    }

    async fn call(
        &self,
        request: tonic::Request<wire::CallRequest>,
    ) -> std::result::Result<tonic::Response<wire::CallReply>, tonic::Status> {
        let peer = request
            .extensions()
            .get::<PeerIdentity>()
            .ok_or_else(|| tonic::Status::unauthenticated("enrolled certificate required"))?
            .0
            .clone();
        let request = request.into_inner();
        let args: Value = serde_json::from_str(&request.json)
            .map_err(|_| tonic::Status::invalid_argument("invalid JSON"))?;
        let db =
            Store::open(&self.root).map_err(|_| tonic::Status::internal("store unavailable"))?;
        let value = match self.handle(&db, &peer, &request.method, &args) {
            Ok(v) => v,
            Err(e) => json!({"error":e.to_string()}),
        };
        Ok(tonic::Response::new(wire::CallReply {
            json: value.to_string(),
        }))
    }
}
impl Service {
    fn handle(&self, db: &Store, peer: &str, method: &str, args: &Value) -> Result<Value> {
        match method {
            "manage" => {
                ensure!(
                    self.config.management_clients.iter().any(|p| p == peer),
                    "caller is not approved for runtime management"
                );
                crate::management::remote_command(db, peer, args)
            }

            "capabilities" => Ok(
                json!({"runtime":self.config.runtime_id,"execution_available":self.config.execution_clients.iter().any(|p|p==peer),"protocol":1}),
            ),
            "accept" => {
                ensure!(
                    self.config.execution_clients.iter().any(|p| p == peer),
                    "caller is not approved for execution"
                );
                let owner = args["task"].as_str().context("owner task")?;
                let old = db.rows(
                    "SELECT task FROM remote_origins WHERE owner_peer=? AND owner_task=?",
                    &[&peer, &owner],
                )?;
                let mut immutable = args.clone();
                immutable
                    .as_object_mut()
                    .context("packet")?
                    .remove("bundles");
                let digest = crate::store::hash(serde_json::to_string(&immutable)?.as_bytes());
                if let Some(old) = old.first() {
                    let saved: String = db.conn.query_row(
                        "SELECT data FROM external_ops WHERE task=? AND name='federation.request'",
                        [old["task"].as_str()],
                        |r| r.get(0),
                    )?;
                    ensure!(
                        saved == digest,
                        "remote submission ID reused with different assignment"
                    );
                    return Ok(json!({"id":old["task"],"duplicate":true}));
                }
                let repo = db
                    .root
                    .join("remote-repositories")
                    .join(crate::store::hash(format!("{peer}:{owner}").as_bytes()));
                if !repo.join(".git").exists() {
                    unpack(&args["snapshot"], &repo)?;
                }
                let mut settings = crate::config::Settings::load(&db.root)?;
                let inherited: crate::config::Settings =
                    serde_json::from_value(args["settings"].clone())?;
                settings.allow_commands &= inherited.allow_commands;
                settings.autonomy = inherited.autonomy;
                settings.delivery = inherited.delivery;
                settings.concurrency = 1;
                settings.limits.workers = 1;
                settings.secret_bundles.clear();
                let plan: crate::template::Plan = serde_json::from_value(args["plan"].clone())?;
                crate::template::validate(&plan.steps)?;
                db.atomic(|| {
                    let oid = db.submit(
                        args["objective"].as_str().context("objective")?,
                        &repo,
                        &settings,
                        &plan,
                    )?;
                    db.conn.execute(
                        "INSERT INTO remote_origins VALUES(?,?,?)",
                        params![oid, peer, owner],
                    )?;
                    db.conn.execute(
                        "INSERT INTO external_ops VALUES(?,'federation.request','accepted',?)",
                        params![oid, digest],
                    )?;
                    save_bundles(db, &oid, peer, &self.config, &args["bundles"])?;
                    sync_context(db, &oid, &args["context"])?;
                    db.conn.execute(
                        "INSERT INTO remote_context VALUES(?,?)",
                        params![oid, json!({"context":args["context"]}).to_string()],
                    )?;
                    Ok(json!({"id":oid}))
                })
            }
            "status" | "cancel" | "revise" => {
                let oid = args["task"].as_str().context("task")?;
                ensure!(
                    !db.rows(
                        "SELECT task FROM remote_origins WHERE task=? AND owner_peer=?",
                        &[&oid, &peer]
                    )?
                    .is_empty(),
                    "task belongs to another caller"
                );
                if method == "revise" {
                    let key = format!(
                        "federation.revise:{}",
                        crate::store::hash(args["steps"].to_string().as_bytes())
                    );
                    return db.atomic(|| {
                        if let Some(old) = db
                            .rows(
                                "SELECT data FROM external_ops WHERE task=? AND name=?",
                                &[&oid, &key],
                            )?
                            .first()
                        {
                            return Ok(serde_json::from_str(
                                old["data"].as_str().context("revision receipt")?,
                            )?);
                        }
                        crate::protocol::dispatch(
                            db,
                            "add_steps",
                            json!({"task":oid,"steps":args["steps"]}),
                            None,
                        )?;
                        let result =
                            crate::protocol::dispatch(db, "resume", json!({"task":oid}), None)?;
                        db.conn.execute(
                            "INSERT INTO external_ops VALUES(?,?,'done',?)",
                            params![oid, key, result.to_string()],
                        )?;
                        Ok(result)
                    });
                }
                if method == "cancel" {
                    return crate::protocol::dispatch(db, "cancel", json!({"task":oid}), None);
                }
                let o = db.task(oid)?;
                let questions = db.rows(
                    "SELECT q.id,q.question,q.answer FROM questions q WHERE q.task=?",
                    &[&oid],
                )?;
                let snapshot = if o["status"] == "succeeded" {
                    Some(snapshot(&crate::git::task_workspace(db, oid)?)?)
                } else {
                    None
                };
                Ok(
                    json!({"task":o,"questions":questions,"snapshot":snapshot,"steps":db.steps(oid)?,"metrics":crate::metrics::report(db,oid)?,"context_version":crate::delegation::tree(db,oid)?["version"],"cleanup_complete":db.rows("SELECT id FROM app_environments WHERE task=? AND state!='removed'",&[&oid])?.is_empty()}),
                )
            }
            "caller_operation" | "sync" | "environment_lease" | "child_result"
            | "child_verified" => {
                let oid = args["task"].as_str().context("task")?;
                ensure!(
                    !db.rows(
                        "SELECT task FROM remote_links WHERE task=? AND peer=?",
                        &[&oid, &peer]
                    )?
                    .is_empty(),
                    "caller does not own this delegated task"
                );
                if method == "child_result" || method == "child_verified" {
                    let child = args["child"].as_str().context("child")?;
                    ensure!(
                        crate::delegation::tree(db, child)?["parent"] == oid,
                        "child belongs to another parent"
                    );
                    ensure!(
                        db.task(child)?["status"] == "succeeded",
                        "child is not complete"
                    );
                    if method == "child_verified" {
                        let version = crate::delegation::tree(db, oid)?["version"]
                            .as_i64()
                            .context("version")?;
                        ensure!(
                            args["version"] == version,
                            "parent context changed during integration"
                        );
                        db.conn.execute("INSERT INTO child_acceptance VALUES(?,?,?,?) ON CONFLICT(parent,child) DO UPDATE SET version=excluded.version,evidence=excluded.evidence",params![oid,child,version,args["evidence"].to_string()])?;
                        return Ok(json!({"verified":true}));
                    }
                    let (snapshot, base) = child_snapshot(db, child)?;
                    return Ok(
                        json!({"snapshot":snapshot,"base":base,"version":crate::delegation::tree(db,oid)?["version"]}),
                    );
                }
                if method == "environment_lease" {
                    return db.atomic(|| {
                        let present: i64 = db.conn.query_row(
                            "SELECT COUNT(*) FROM remote_environment_leases WHERE task=?",
                            [oid],
                            |r| r.get(0),
                        )?;
                        if args["acquire"] == true && present == 0 {
                            ensure!(
                                crate::environment::available(db, oid)?,
                                "root environment limit reached"
                            );
                            db.conn.execute(
                                "INSERT INTO remote_environment_leases VALUES(?)",
                                [oid],
                            )?;
                        }
                        if args["acquire"] == false {
                            db.conn.execute(
                                "DELETE FROM remote_environment_leases WHERE task=?",
                                [oid],
                            )?;
                        }
                        Ok(json!({"granted":true}))
                    });
                }
                if method == "sync" {
                    return Ok(
                        json!({"context":crate::delegation::mandatory(db,oid)?,"questions":db.rows("SELECT q.* FROM questions q WHERE q.task=?",&[&oid])?,"status":db.task(oid)?["status"],"bundles":if args["need_bundles"]==false {json!({})} else {bundle_packet(db,oid,peer,&self.config)?},"children":crate::delegation::dispatch(db,oid,"list_children",&json!({}))?,"pending_questions":crate::delegation::dispatch(db,oid,"pending_questions",&json!({}))?}),
                    );
                }
                let name = args["method"].as_str().context("method")?;
                ensure!(
                    [
                        "delegate_task",
                        "add_knowledge",
                        "knowledge",
                        "link_knowledge",
                        "list_children",
                        "read_context",
                        "pending_questions",
                        "request_question",
                        "answer_question",
                        "escalate_question"
                    ]
                    .contains(&name),
                    "operation unavailable to remote caller"
                );
                let mut input = args["args"].clone();
                input["task"] = json!(oid);
                input.as_object_mut().context("arguments")?.remove("worker");
                if name == "delegate_task" && args["snapshot"].is_object() {
                    input["_snapshot"] = args["snapshot"].clone();
                }
                if name == "answer_question" {
                    let q = db.rows(
                        "SELECT human_only FROM question_routes WHERE question=?",
                        &[&input["question"].as_str()],
                    )?;
                    ensure!(
                        !q.first().is_some_and(|q| q["human_only"] == 1),
                        "remote agent cannot attest a human answer"
                    );
                    input["human"] = json!(false);
                }
                crate::protocol::dispatch(db, name, input, None)
            }
            _ => bail!("unknown federation operation"),
        }
    }
}
fn sync_context(db: &Store, oid: &str, context: &Value) -> Result<()> {
    let version = context["version"].as_i64().context("context version")?;
    db.conn
        .execute("DELETE FROM context_records WHERE root=?", [oid])?;
    for record in context["records"].as_array().context("context records")? {
        db.conn.execute(
            "INSERT INTO context_records VALUES(?,?,?,?,?,?,1)",
            params![
                format!("{}:{}", oid, record["id"].as_str().context("source ID")?),
                oid,
                record["version"].as_i64(),
                record["kind"].as_str(),
                record["content"].as_str(),
                record["provenance"].as_str()
            ],
        )?;
    }
    db.conn.execute(
        "UPDATE task_tree SET version=? WHERE task=?",
        params![version, oid],
    )?;
    Ok(())
}
pub async fn tick(db: &Store) -> Result<()> {
    let links = db.rows(
        "SELECT l.*,o.status FROM remote_links l JOIN tasks o ON o.id=l.task WHERE l.state!='done'",
        &[],
    )?;
    let origins = db.rows("SELECT * FROM remote_origins", &[])?;
    if links.is_empty() && origins.is_empty() {
        return Ok(());
    }
    let config = match config(db) {
        Ok(config) => config,
        Err(_) => {
            db.conn.execute("UPDATE tasks SET status='blocked' WHERE status='running' AND id IN (SELECT task FROM remote_origins)",[])?;
            return Ok(());
        }
    };
    for row in origins {
        let oid = row["task"].as_str().context("task")?;
        let peer = row["owner_peer"].as_str().context("peer")?;
        let terminal = ["succeeded", "failed", "cancelled"]
            .iter()
            .any(|s| db.task(oid).is_ok_and(|o| o["status"] == *s));
        if terminal
            && db
                .rows(
                    "SELECT id FROM app_environments WHERE task=? AND state!='removed'",
                    &[&oid],
                )?
                .is_empty()
        {
            let secrets = db.root.join("remote-secrets").join(oid);
            if secrets.exists() {
                std::fs::remove_dir_all(secrets)?;
            }
            continue;
        }
        match call(
            &config,
            peer,
            "sync",
            json!({"task":row["owner_task"],"need_bundles":!terminal}),
        )
        .await
        {
            Ok(reply) => {
                let synced = db.atomic(||{
     db.conn.execute("UPDATE tasks SET status='running' WHERE id=? AND status='blocked' AND EXISTS(SELECT 1 FROM external_ops WHERE task=? AND name='federation.sync' AND state='blocked') AND NOT EXISTS(SELECT 1 FROM attempts a JOIN steps t ON t.id=a.step WHERE t.task=? AND a.state='uncertain')",params![oid,oid,oid])?;
     db.conn.execute("DELETE FROM external_ops WHERE task=? AND name='federation.sync'",[oid])?;
     db.conn.execute("INSERT INTO remote_context VALUES(?,?) ON CONFLICT(task) DO UPDATE SET packet=excluded.packet",params![oid,json!({"context":reply["context"],"children":reply["children"],"pending_questions":reply["pending_questions"]}).to_string()])?;
     sync_context(db,oid,&reply["context"])?;save_bundles(db,oid,peer,&config,&reply["bundles"])?;
     apply_remote_answers(db,oid,&reply["questions"])?;
     if reply["status"]=="cancelled"{crate::protocol::dispatch(db,"cancel",json!({"task":oid}),None)?;}
     Ok(())});
                if synced.is_err() {
                    block_sync(db, oid)?;
                }
            }
            Err(_) => {
                block_sync(db, oid)?;
            }
        }
    }
    for link in links {
        let oid = link["task"].as_str().context("task")?;
        let peer = link["peer"].as_str().context("peer")?;
        let result:Result<()>=async {
   if link["state"]=="pending" && link["status"]=="cancelled" {
    db.conn.execute("UPDATE remote_links SET state='done' WHERE task=?",[oid])?;return Ok(());
   }
   if link["state"]=="pending"||link["state"]=="sending"{
    if link["state"]=="pending"&&!crate::delegation::capacity(db,oid)?{return Ok(());}
    let mut packet=prepared_packet(db,oid)?;
    db.conn.execute("UPDATE remote_links SET state='sending',base=? WHERE task=?",params![packet["snapshot"]["commit"].as_str(),oid])?;
    packet["bundles"]=bundle_packet(db,oid,peer,&config)?;
    let reply=call(&config,peer,"accept",packet).await?;
    db.conn.execute("UPDATE remote_links SET state='running',remote_id=? WHERE task=?",params![reply["id"].as_str(),oid])?;return Ok(());
   }
   let remote=link["remote_id"].as_str().context("remote task")?;
   if link["status"]=="cancelled"{call(&config,peer,"cancel",json!({"task":remote})).await?;}
   let reply=call(&config,peer,"status",json!({"task":remote})).await?;
   let status=reply["task"]["status"].as_str().context("remote status")?;
   if status=="succeeded" && !crate::delegation::child_completion(db,oid)? {return Ok(());}
   if status=="succeeded" && reply["context_version"]!=crate::delegation::tree(db,oid)?["version"]{
    db.conn.execute("UPDATE tasks SET status='blocked' WHERE id=?",[oid])?;return Ok(());
   }
   if ["succeeded","failed","cancelled"].contains(&status) && reply["cleanup_complete"]==true{
    db.atomic(||{
    db.conn.execute("UPDATE tasks SET status=? WHERE id=?",params![status,oid])?;
    db.conn.execute("UPDATE remote_links SET state='done' WHERE task=?",[oid])?;
    db.conn.execute("INSERT INTO external_ops VALUES(?,'federation.metrics','done',?) ON CONFLICT(task,name) DO UPDATE SET data=excluded.data",params![oid,reply["metrics"].to_string()])?;
    db.conn.execute("UPDATE steps SET state=?,result=? WHERE task=?",params![if status=="succeeded"{"succeeded"}else{"failed"},json!({"accepted":status=="succeeded","result":"remote result requires parent integration and verification","remote":reply["steps"]}).to_string(),oid])?;
    if status=="succeeded"{let bytes=serde_json::to_vec(&reply["snapshot"])?;db.artifact(oid,None,"remote-snapshot",&bytes,&json!({"base":link["base"]}),false)?;}
    let parent=crate::delegation::tree(db,oid)?["parent"].as_str().context("parent")?.to_owned();db.event(&parent,"child.finished",json!({"child":oid,"status":status,"verification_required":true}))?;Ok(())})?;
   }
   Ok(())
  }.await;
        if let Err(error) = result {
            db.event(
                oid,
                "federation.retry_pending",
                json!({"error":error.to_string()}),
            )?;
        }
    }
    Ok(())
}
/// Import a child snapshot as a commit based on its recorded source, then use the existing integration queue.
pub fn integrate_child(db: &Store, parent: &str, args: &Value) -> Result<Value> {
    let child = args["child"].as_str().context("child")?;
    ensure!(
        crate::delegation::tree(db, child)?["parent"] == parent,
        "not an immediate child"
    );
    ensure!(
        db.task(child)?["status"] == "succeeded",
        "child is not complete"
    );
    let validation: Vec<String> = serde_json::from_value(args["validation"].clone())
        .context("parent verification command is required")?;
    ensure!(
        !validation.is_empty(),
        "parent verification command is required"
    );
    let version = crate::delegation::tree(db, parent)?["version"]
        .as_i64()
        .context("context version")?;
    let accepted = db.rows(
        "SELECT evidence FROM child_acceptance WHERE parent=? AND child=? AND version=?",
        &[&parent, &child, &version],
    )?;
    if let Some(a) = accepted.first() {
        return Ok(serde_json::from_str(
            a["evidence"].as_str().context("evidence")?,
        )?);
    }
    let links = db.rows("SELECT * FROM remote_links WHERE task=?", &[&child])?;
    let (snapshot, base) = if let Some(link) = links.first() {
        let artifact=db.rows("SELECT hash FROM artifact_links WHERE task=? AND name='remote-snapshot' ORDER BY rowid DESC LIMIT 1",&[&child])?.into_iter().next().context("child snapshot missing")?;
        let hash = artifact["hash"].as_str().context("artifact hash")?;
        let bytes = std::fs::read(db.root.join("artifacts").join(hash))?;
        ensure!(crate::store::hash(&bytes) == hash, "child artifact corrupt");
        (
            serde_json::from_slice::<Value>(&bytes)?,
            link["base"].as_str().context("child base")?.to_owned(),
        )
    } else {
        let workspace = crate::git::task_workspace(db, child)?;
        let _parent_workspace = crate::git::task_workspace(db, parent)?;
        (
            snapshot(&workspace)?,
            db.conn.query_row(
                "SELECT base FROM local_child_bases WHERE task=?",
                [child],
                |r| r.get::<_, String>(0),
            )?,
        )
    };
    let child_plan = db.task(child)?["plan"].clone();
    let worker = if let Some(w) = args["worker"].as_str() {
        w.to_owned()
    } else {
        db.atomic(|| {
            if let Some(row)=db.rows("SELECT data FROM external_ops WHERE task=? AND name='federation.integration_worker'",&[&parent])?.first() {return Ok(row["data"].as_str().context("integration worker")?.to_owned());}
            let worker=db.register(parent,None)?["id"].as_str().context("worker")?.to_owned();
            db.conn.execute("INSERT INTO external_ops VALUES(?,'federation.integration_worker','registered',?)",params![parent,worker])?;
            Ok(worker)
        })?
    };
    let workspace = crate::git::allocate(db, parent, &worker)?;
    db.claim(parent, &worker, &[".".into()])?;
    ensure!(
        crate::git::run(&workspace, &["status", "--porcelain"])?.is_empty(),
        "commit parent work before importing child"
    );
    let scratch = db.root.join("child-imports").join(crate::store::id());
    unpack(&snapshot, &scratch)?;
    let index = db.root.join(format!("import-index-{}", crate::store::id()));
    let operation = (|| -> Result<Value> {
        let mut cmd = crate::executor::clean_command("git");
        cmd.current_dir(&workspace)
            .env("GIT_INDEX_FILE", &index)
            .args(["read-tree", "--empty"]);
        ensure!(cmd.status()?.success(), "cannot create import index");
        let mut cmd = crate::executor::clean_command("git");
        cmd.current_dir(&workspace)
            .env("GIT_INDEX_FILE", &index)
            .arg(format!("--work-tree={}", scratch.display()))
            .args(["add", "--all", "--force"]);
        ensure!(cmd.status()?.success(), "cannot stage child snapshot");
        let mut cmd = crate::executor::clean_command("git");
        cmd.current_dir(&workspace)
            .env("GIT_INDEX_FILE", &index)
            .arg("write-tree");
        let out = cmd.output()?;
        ensure!(out.status.success(), "cannot write child tree");
        let tree = String::from_utf8(out.stdout)?;
        let commit = crate::git::run(
            &workspace,
            &[
                "commit-tree",
                tree.trim(),
                "-p",
                &base,
                "-m",
                &format!("Integrate child {child}"),
            ],
        )?;
        let unchanged =
            crate::git::run(&workspace, &["diff", "--name-only", &base, &commit])?.is_empty();
        if !unchanged && let Err(error) = crate::git::run(&workspace, &["cherry-pick", &commit]) {
            let conflicts =
                crate::git::run(&workspace, &["diff", "--name-only", "--diff-filter=U"])?;
            let started =
                crate::git::run(&workspace, &["rev-parse", "--verify", "CHERRY_PICK_HEAD"])
                    .is_ok_and(|head| head == commit);
            if !started {
                return Err(error);
            }
            let empty = conflicts.is_empty()
                && crate::git::run(&workspace, &["diff", "--cached", "--name-only"])?.is_empty();
            let _ = crate::git::run(&workspace, &["cherry-pick", "--abort"]);
            if !empty {
                db.event(
                    parent,
                    "child.integration_conflict",
                    json!({"child":child,"conflicts":conflicts}),
                )?;
                bail!("child integration conflict: {conflicts}");
            }
        }
        let result = crate::git::integrate(db, parent, &worker, &validation)?;
        db.atomic(|| {
            let current=db.task(child)?;
            ensure!(current["status"]=="succeeded" && current["plan"]==child_plan && crate::delegation::tree(db,parent)?["version"]==version,"child or caller changed during integration; revalidation required");
            db.conn.execute("INSERT INTO child_acceptance VALUES(?,?,?,?) ON CONFLICT(parent,child) DO UPDATE SET version=excluded.version,evidence=excluded.evidence",params![parent,child,version,result.to_string()])?;
            Ok(())
        })?;
        if args["worker"].is_null() {
            db.conn
                .execute("DELETE FROM claims WHERE worker=?", [&worker])?;
        }
        db.event(
            parent,
            "child.integrated",
            json!({"child":child,"evidence":result}),
        )?;
        Ok(result)
    })();
    let _ = std::fs::remove_file(index);
    let _ = std::fs::remove_dir_all(scratch);
    operation
}

pub async fn environment_lease(db: &Store, oid: &str, acquire: bool) -> Result<()> {
    let rows = db.rows("SELECT * FROM remote_origins WHERE task=?", &[&oid])?;
    if let Some(origin) = rows.first() {
        call(
            &config(db)?,
            origin["owner_peer"].as_str().context("owner peer")?,
            "environment_lease",
            json!({"task":origin["owner_task"],"acquire":acquire}),
        )
        .await?;
    }
    Ok(())
}

pub fn revise_child(db: &Store, oid: &str, steps: &Value) -> Result<Option<Value>> {
    let rows = db.rows("SELECT * FROM remote_links WHERE task=?", &[&oid])?;
    if let Some(link) = rows.first() {
        let key = format!(
            "federation.revise:{}",
            crate::store::hash(steps.to_string().as_bytes())
        );
        if let Some(receipt) = db
            .rows(
                "SELECT data FROM external_ops WHERE task=? AND name=?",
                &[&oid, &key],
            )?
            .first()
        {
            return Ok(Some(serde_json::from_str(
                receipt["data"].as_str().context("revision receipt")?,
            )?));
        }
        let mut plan: crate::template::Plan =
            serde_json::from_str(db.task(oid)?["plan"].as_str().context("plan")?)?;
        plan.steps
            .extend(serde_json::from_value::<Vec<crate::template::Step>>(
                steps.clone(),
            )?);
        crate::template::validate(&plan.steps)?;
        let result = call_sync(
            config(db)?,
            link["peer"].as_str().context("peer")?.into(),
            "revise".into(),
            json!({"task":link["remote_id"],"steps":steps}),
        )?;
        db.atomic(|| {
            if db
                .rows(
                    "SELECT data FROM external_ops WHERE task=? AND name=?",
                    &[&oid, &key],
                )?
                .is_empty()
            {
                crate::delegation::invalidate_acceptance(db, oid)?;
                let revision: i64 = db.conn.query_row(
                    "SELECT COALESCE(MAX(revision),0)+1 FROM revisions WHERE task=?",
                    [oid],
                    |r| r.get(0),
                )?;
                let serialized = serde_json::to_string(&plan)?;
                db.conn.execute(
                    "INSERT INTO revisions VALUES(?,?,?,?)",
                    params![oid, revision, serialized, crate::store::now()],
                )?;
                db.conn.execute(
                    "UPDATE remote_links SET state='running' WHERE task=?",
                    [oid],
                )?;
                db.conn.execute(
                    "UPDATE tasks SET plan=?,status='remote' WHERE id=?",
                    params![serialized, oid],
                )?;
                db.conn.execute(
                    "INSERT INTO external_ops VALUES(?,?,'done',?)",
                    params![oid, key, result.to_string()],
                )?;
            }
            Ok(())
        })?;
        return Ok(Some(result));
    }
    Ok(None)
}

pub fn clear_remote_secrets(db: &Store) -> Result<()> {
    let dir = db.root.join("remote-secrets");
    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    Ok(())
}

fn prepared_packet(db: &Store, oid: &str) -> Result<Value> {
    let old=db.rows("SELECT hash FROM artifact_links WHERE task=? AND name='federation-input' ORDER BY rowid LIMIT 1",&[&oid])?;
    if let Some(old) = old.first() {
        let hash = old["hash"].as_str().context("input hash")?;
        let bytes = std::fs::read(db.root.join("artifacts").join(hash))?;
        ensure!(
            crate::store::hash(&bytes) == hash,
            "delegation input artifact corrupt"
        );
        return Ok(serde_json::from_slice(&bytes)?);
    }
    let o = db.task(oid)?;
    let tree = crate::delegation::tree(db, oid)?;
    let parent = tree["parent"].as_str().context("parent")?;
    let source = crate::git::task_workspace(db, parent)?;
    let caller=db.rows("SELECT hash FROM artifact_links WHERE task=? AND name='caller-snapshot' ORDER BY rowid LIMIT 1",&[&oid])?;
    let snapshot = if let Some(row) = caller.first() {
        let hash = row["hash"].as_str().context("snapshot hash")?;
        let bytes = std::fs::read(db.root.join("artifacts").join(hash))?;
        ensure!(
            crate::store::hash(&bytes) == hash,
            "caller snapshot corrupt"
        );
        serde_json::from_slice::<Value>(&bytes)?
    } else {
        snapshot(&source)?
    };
    let packet = json!({"task":oid,"objective":o["objective"],"snapshot":snapshot,"context":crate::delegation::mandatory(db,oid)?,"settings":serde_json::from_str::<Value>(o["settings"].as_str().context("settings")?)?,"plan":serde_json::from_str::<Value>(o["plan"].as_str().context("plan")?)?});
    db.artifact(
        oid,
        None,
        "federation-input",
        &serde_json::to_vec(&packet)?,
        &json!({"context_version":packet["context"]["version"]}),
        false,
    )?;
    Ok(packet)
}

fn child_snapshot(db: &Store, child: &str) -> Result<(Value, String)> {
    let remote = db.rows("SELECT base FROM remote_links WHERE task=?", &[&child])?;
    if let Some(remote) = remote.first() {
        let artifact=db.rows("SELECT hash FROM artifact_links WHERE task=? AND name='remote-snapshot' ORDER BY rowid DESC LIMIT 1",&[&child])?.into_iter().next().context("child artifact")?;
        let hash = artifact["hash"].as_str().context("hash")?;
        let bytes = std::fs::read(db.root.join("artifacts").join(hash))?;
        ensure!(crate::store::hash(&bytes) == hash, "child artifact corrupt");
        Ok((
            serde_json::from_slice(&bytes)?,
            remote["base"].as_str().context("base")?.into(),
        ))
    } else {
        Ok((
            snapshot(&crate::git::task_workspace(db, child)?)?,
            db.conn.query_row(
                "SELECT base FROM local_child_bases WHERE task=?",
                [child],
                |r| r.get(0),
            )?,
        ))
    }
}
fn import_foreign_child(db: &Store, parent: &str, args: &Value, origin: &Value) -> Result<Value> {
    let remote_child = args["child"].as_str().context("child")?;
    let peer = origin["owner_peer"].as_str().context("owner")?;
    let reply = call_sync(
        config(db)?,
        peer.into(),
        "child_result".into(),
        json!({"task":origin["owner_task"],"child":remote_child}),
    )?;
    let rows = db.rows(
        "SELECT local_child FROM foreign_children WHERE parent=? AND remote_child=?",
        &[&parent, &remote_child],
    )?;
    let local = if let Some(row) = rows.first() {
        row["local_child"].as_str().context("child")?.to_owned()
    } else {
        db.atomic(||{
  let o=db.task(parent)?;let mut settings:crate::config::Settings=serde_json::from_str(o["settings"].as_str().context("settings")?)?;settings.secret_bundles.clear();
  let empty=crate::template::Plan{steps:vec![],pins:Default::default(),outputs:Default::default()};
  let child=db.submit("Verified child snapshot",Path::new(o["repo"].as_str().context("repo")?),&settings,&empty)?;
  let t=crate::delegation::tree(db,parent)?;
  db.conn.execute("UPDATE task_tree SET root=?,parent=?,version=? WHERE task=?",params![t["root"].as_str(),parent,reply["version"].as_i64(),child])?;
  db.conn.execute("UPDATE tasks SET status='succeeded' WHERE id=?",[&child])?;
  db.conn.execute("INSERT INTO foreign_children VALUES(?,?,?)",params![parent,remote_child,child])?;
  db.conn.execute("INSERT INTO remote_links(task,peer,remote_id,state,request,base) VALUES(?,?,?,'done',?,?)",params![child,peer,remote_child,remote_child,reply["base"].as_str()])?;
  Ok(child)
 })?
    };
    let previous=db.rows("SELECT hash FROM artifact_links WHERE task=? AND name='remote-snapshot' ORDER BY rowid DESC LIMIT 1",&[&local])?;
    if previous.first().is_some_and(|p| {
        p["hash"] != crate::store::hash(&serde_json::to_vec(&reply["snapshot"]).unwrap_or_default())
    }) {
        crate::delegation::invalidate_acceptance(db, &local)?;
        db.conn
            .execute("UPDATE tasks SET status='succeeded' WHERE id=?", [&local])?;
    }
    db.artifact(
        &local,
        None,
        "remote-snapshot",
        &serde_json::to_vec(&reply["snapshot"])?,
        &json!({"base":reply["base"]}),
        false,
    )?;
    let mut input = args.clone();
    input["child"] = json!(local);
    let result = integrate_child(db, parent, &input)?;
    call_sync(
        config(db)?,
        peer.into(),
        "child_verified".into(),
        json!({"task":origin["owner_task"],"child":remote_child,"version":reply["version"],"evidence":result}),
    )?;
    Ok(result)
}

fn block_sync(db: &Store, oid: &str) -> Result<()> {
    db.atomic(|| {
        if db.task(oid)?["status"]=="running" {
            db.conn.execute("INSERT INTO external_ops VALUES(?,'federation.sync','blocked','{}') ON CONFLICT(task,name) DO UPDATE SET state='blocked'",[oid])?;
            db.conn.execute("UPDATE tasks SET status='blocked' WHERE id=?",[oid])?;
        }
        Ok(())
    })
}

/// Refresh the caller contract at invocation boundaries, so a disconnected or
/// stale remote worker cannot accept a result using an old contract.
pub async fn refresh_origin(db: &Store, oid: &str) -> Result<()> {
    let rows = db.rows("SELECT * FROM remote_origins WHERE task=?", &[&oid])?;
    if let Some(origin) = rows.first() {
        let config = config(db)?;
        let peer = origin["owner_peer"].as_str().context("owner peer")?;
        let reply = call(&config, peer, "sync", json!({"task":origin["owner_task"]})).await?;
        ensure!(reply["status"] != "cancelled", "caller cancelled this task");
        db.atomic(|| {
            sync_context(db,oid,&reply["context"])?;
            apply_remote_answers(db,oid,&reply["questions"])?;
            save_bundles(db,oid,peer,&config,&reply["bundles"])?;
            db.conn.execute("INSERT INTO remote_context VALUES(?,?) ON CONFLICT(task) DO UPDATE SET packet=excluded.packet",params![oid,json!({"context":reply["context"],"children":reply["children"],"pending_questions":reply["pending_questions"]}).to_string()])?;
            Ok(())
        })?;
    }
    Ok(())
}

/// Apply each newly received answer once; consumed answers must never reopen retries.
pub fn apply_remote_answers(db: &Store, oid: &str, questions: &Value) -> Result<()> {
    db.atomic(|| {
        for q in questions.as_array().into_iter().flatten() {
            if let Some(answer)=q["answer"].as_str() {
                let changed=db.conn.execute("UPDATE questions SET answer=? WHERE id=? AND task=? AND answer IS NULL",params![answer,q["id"].as_str(),oid])?;
                if changed==0 {continue;}
                db.conn.execute("UPDATE question_context SET purpose='input_answered' WHERE question=? AND purpose='input'",[q["id"].as_str()])?;
                db.conn.execute("UPDATE steps SET state='pending' WHERE state='waiting' AND id IN (SELECT w.step FROM workers w JOIN question_context c ON c.worker=w.id WHERE c.question=? AND NOT EXISTS(SELECT 1 FROM questions q JOIN question_context qc ON qc.question=q.id WHERE qc.worker=w.id AND q.answer IS NULL))",[q["id"].as_str()])?;
            }
        }
        Ok(())
    })
}

pub fn handle_control(
    root: PathBuf,
    config: NetworkConfig,
    peer: &str,
    packet: &Value,
) -> Result<Value> {
    let db = Store::open(&root)?;
    Service { root, config }.handle(
        &db,
        peer,
        packet["method"].as_str().context("method required")?,
        &packet["args"],
    )
}
