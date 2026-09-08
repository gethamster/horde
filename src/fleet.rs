//! Provider-owned resources and durable administrative operations.
use crate::{
    management,
    store::{Store, now},
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
    pub provider: String,
    pub endpoint: String,
    pub api_key_env: String,
    pub image: String,
    pub context: String,
    pub namespace: String,
    pub concurrency: usize,
    pub cpus: u32,
    pub memory_mb: u32,
    pub disk_gb: u32,
    pub lifetime_seconds: u32,
    pub peer: Option<String>,
    pub executor_roles: Vec<String>,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            provider: "docker".into(),
            endpoint: String::new(),
            api_key_env: String::new(),
            image: String::new(),
            context: "default".into(),
            namespace: "default".into(),
            concurrency: 4,
            cpus: 2,
            memory_mb: 2048,
            disk_gb: 10,
            lifetime_seconds: 3600,
            peer: None,
            executor_roles: vec![],
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub profiles: BTreeMap<String, Profile>,
    pub capacity_policy: CapacityPolicy,
    pub budgets: Vec<Budget>,
    pub capacity_commands: Vec<Vec<String>>,
    pub issuer_key: Option<PathBuf>,
    pub controller_address: Option<std::net::SocketAddr>,
    pub controller_tls_name: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CapacityPolicy {
    pub warn_percent: f64,
    pub switch_percent: f64,
    pub stale_seconds: i64,
}
impl Default for CapacityPolicy {
    fn default() -> Self {
        Self {
            warn_percent: 80.0,
            switch_percent: 90.0,
            stale_seconds: 300,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub account: String,
    pub since: i64,
    pub reset_at: i64,
    pub tokens: Option<u64>,
    pub usd: Option<f64>,
}
pub fn load() -> Result<Config> {
    let path = crate::config::Settings::user_path().with_file_name("runtimes.toml");
    if !path.exists() {
        return Ok(Config::default());
    }
    let config: Config = toml::from_str(&std::fs::read_to_string(path)?)?;
    let p = &config.capacity_policy;
    ensure!(
        p.warn_percent.is_finite()
            && p.switch_percent.is_finite()
            && p.warn_percent > 0.0
            && p.warn_percent < p.switch_percent
            && p.switch_percent <= 100.0
            && p.stale_seconds > 0,
        "invalid capacity thresholds"
    );
    for b in &config.budgets {
        ensure!(
            b.since >= 0
                && b.reset_at > b.since
                && b.tokens.is_none_or(|v| v > 0)
                && b.usd.is_none_or(|v| v.is_finite() && v > 0.0),
            "invalid account budget"
        );
    }
    Ok(config)
}
impl Profile {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            ["docker", "kubernetes", "e2b", "daytona", "tailscale"]
                .contains(&self.provider.as_str()),
            "unsupported runtime provider"
        );
        ensure!(
            (1..=64).contains(&self.concurrency)
                && self.cpus > 0
                && self.memory_mb >= 64
                && self.disk_gb > 0
                && self.lifetime_seconds > 0,
            "invalid runtime limits"
        );
        ensure!(
            self.provider == "tailscale" || !self.image.is_empty(),
            "runtime image or template required"
        );
        if ["docker", "kubernetes"].contains(&self.provider.as_str()) {
            ensure!(
                self.image.contains("@sha256:"),
                "container image must be pinned by digest"
            );
        }
        if !self.endpoint.is_empty() {
            let u = reqwest::Url::parse(&self.endpoint)?;
            ensure!(
                u.scheme() == "https"
                    || (u.scheme() == "http"
                        && [Some("127.0.0.1"), Some("localhost")].contains(&u.host_str())),
                "provider endpoint requires HTTPS"
            );
            ensure!(
                u.username().is_empty() && u.password().is_none(),
                "credentials belong in api_key_env"
            );
        }
        Ok(())
    }
}
fn identifier(s: &str) -> Result<()> {
    ensure!(
        s != "local"
            && !s.is_empty()
            && s.len() <= 48
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !s.starts_with('-'),
        "runtime ID must be 1-48 lowercase letters, digits or hyphens; local is reserved"
    );
    Ok(())
}
pub fn dispatch(db: &Store, name: &str, args: &Value) -> Result<Option<Value>> {
    if name == "runtime_list" {
        return Ok(Some(json!(db.rows("SELECT r.id,r.profile,r.resource,r.state,r.version,r.created,r.error,p.observed AS last_seen,p.status AS runtime_status FROM managed_runtimes r LEFT JOIN runtime_presence p ON p.runtime=r.id ORDER BY r.created",&[])?)));
    }
    if name == "runtime_inspect" {
        let id = args["id"].as_str().context("id required")?;
        return Ok(Some(
            json!({"runtime":db.rows("SELECT * FROM managed_runtimes WHERE id=?",&[&id])?,"operations":db.rows("SELECT * FROM runtime_operations WHERE runtime=? ORDER BY created",&[&id])?}),
        ));
    }
    if ![
        "runtime_create",
        "runtime_destroy",
        "runtime_restart",
        "runtime_update",
        "runtime_stop",
        "runtime_start",
        "runtime_reconcile",
    ]
    .contains(&name)
    {
        return Ok(None);
    }
    let id = args["id"].as_str().context("id required")?;
    identifier(id)?;
    let operation = args["request_id"]
        .as_str()
        .context("request_id required for retry-safe management")?;
    let body = args.to_string();
    let old = db.rows("SELECT * FROM runtime_operations WHERE id=?", &[&operation])?;
    if let Some(old) = old.first() {
        ensure!(
            old["runtime"] == id && old["action"] == name && old["args"] == body,
            "request ID reused with different operation"
        );
        return Ok(Some(old.clone()));
    }
    db.atomic(||{
        if name=="runtime_create" {
            let profile=args["profile"].as_str().context("profile required")?;let config=load()?;
            let spec=config.profiles.get(profile).context("unknown runtime profile")?;spec.validate()?;ensure!(spec.provider!="tailscale","use horde network add to enroll Tailscale hosts");
            db.conn.execute("INSERT INTO managed_runtimes(id,profile,spec,state,created) VALUES(?,?,?,'requested',?)",params![id,profile,serde_json::to_string(spec)?,now()])?;
        }else{ensure!(!db.rows("SELECT id FROM managed_runtimes WHERE id=?",&[&id])?.is_empty(),"unknown managed runtime");}
        db.conn.execute("INSERT INTO runtime_operations(id,runtime,action,args,state,created) VALUES(?,?,?,?,'pending',?)",params![operation,id,name,body,now()])?;
        management::event(db,"runtime.operation",json!({"id":operation,"runtime":id,"action":name}))?;Ok(())
    })?;
    Ok(Some(json!({"request_id":operation,"state":"pending"})))
}
async fn command(argv: &[String], input: Option<&Value>) -> Result<Value> {
    use tokio::io::AsyncWriteExt;
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn()?;
    if let Some(input) = input {
        child
            .stdin
            .take()
            .context("stdin")?
            .write_all(input.to_string().as_bytes())
            .await?;
    } else {
        drop(child.stdin.take());
    }
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        child.wait_with_output(),
    )
    .await
    .context("provider command timed out")??;
    ensure!(
        output.status.success(),
        "provider command failed ({})",
        output.status
    );
    ensure!(
        output.stdout.len() <= 4 * 1024 * 1024,
        "provider output too large"
    );
    if output.stdout.is_empty() {
        Ok(json!({}))
    } else {
        Ok(serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| json!({"output":String::from_utf8_lossy(&output.stdout).trim()})))
    }
}
fn docker(p: &Profile, rest: Vec<String>) -> Vec<String> {
    let mut a = vec!["docker".into(), "--context".into(), p.context.clone()];
    a.extend(rest);
    a
}
fn kube(p: &Profile, rest: Vec<String>) -> Vec<String> {
    let mut a = vec![
        "kubectl".into(),
        "--context".into(),
        p.context.clone(),
        "--namespace".into(),
        p.namespace.clone(),
    ];
    a.extend(rest);
    a
}
async fn http(
    p: &Profile,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value> {
    let base = if p.endpoint.is_empty() {
        if p.provider == "e2b" {
            "https://api.e2b.app"
        } else {
            "https://app.daytona.io/api"
        }
    } else {
        &p.endpoint
    };
    let key = crate::config::credential(&p.api_key_env)
        .context("provider API credential unavailable in daemon environment")?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let mut request = client.request(method, format!("{}{path}", base.trim_end_matches('/')));
    request = if p.provider == "e2b" {
        request.header("X-API-Key", key)
    } else {
        request.bearer_auth(key)
    };
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await?;
    ensure!(
        response.status().is_success(),
        "{} API returned {}",
        p.provider,
        response.status()
    );
    let bytes = response.bytes().await?;
    ensure!(
        bytes.len() <= 4 * 1024 * 1024,
        "provider response too large"
    );
    if bytes.is_empty() {
        Ok(json!({}))
    } else {
        Ok(serde_json::from_slice(&bytes)?)
    }
}
pub fn kubernetes_manifest(p: &Profile, id: &str) -> Value {
    json!({"apiVersion":"apps/v1","kind":"StatefulSet","metadata":{"name":format!("horde-{id}"),"labels":{"app.kubernetes.io/managed-by":"horde","horde-runtime":id}},"spec":{"serviceName":format!("horde-{id}"),"replicas":1,"selector":{"matchLabels":{"horde-runtime":id}},"template":{"metadata":{"labels":{"horde-runtime":id}},"spec":{"terminationGracePeriodSeconds":60,"securityContext":{"runAsNonRoot":true,"runAsUser":10001,"fsGroup":10001},"containers":[{"name":"horde","image":p.image,"args":["--data-dir","/data","daemon"],"env":[{"name":"HORDE_CONCURRENCY","value":p.concurrency.to_string()}],"resources":{"requests":{"cpu":p.cpus.to_string(),"memory":format!("{}Mi",p.memory_mb)},"limits":{"cpu":p.cpus.to_string(),"memory":format!("{}Mi",p.memory_mb)}},"volumeMounts":[{"name":"data","mountPath":"/data"}]}]}},"volumeClaimTemplates":[{"metadata":{"name":"data"},"spec":{"accessModes":["ReadWriteOnce"],"resources":{"requests":{"storage":format!("{}Gi",p.disk_gb)}}}}]}})
}
async fn provision(db: &Store, p: &Profile, id: &str, bootstrap: Option<&Value>) -> Result<String> {
    let name = format!("horde-{id}");
    match p.provider.as_str() {
        "docker" => {
            let volume = format!("{name}-data");
            command(
                &docker(
                    p,
                    vec![
                        "volume".into(),
                        "create".into(),
                        "--label".into(),
                        format!("task-runtime={id}"),
                        volume.clone(),
                    ],
                ),
                None,
            )
            .await?;
            let mut args = vec![
                "run".into(),
                "--detach".into(),
                "--name".into(),
                name.clone(),
                "--label".into(),
                format!("task-runtime={id}"),
                "--restart".into(),
                "unless-stopped".into(),
                "--cpus".into(),
                p.cpus.to_string(),
                "--memory".into(),
                format!("{}m", p.memory_mb),
                "--mount".into(),
                format!("type=volume,src={volume},dst=/data"),
            ];
            let env_file = db.root.join(format!("bootstrap-{}", crate::store::id()));
            let env = bootstrap_env(p, bootstrap);
            let mut lines = String::new();
            for (k, v) in env.as_object().context("bootstrap environment")? {
                lines.push_str(&format!(
                    "{k}={}\n",
                    v.as_str().context("environment value")?
                ));
            }
            crate::secrets::write_private(&env_file, lines.as_bytes())?;
            args.extend([
                "--env-file".into(),
                env_file.to_string_lossy().into_owned(),
                p.image.clone(),
                "--data-dir".into(),
                "/data".into(),
                "daemon".into(),
            ]);
            let result = command(&docker(p, args), None).await;
            std::fs::remove_file(env_file)?;
            result?;
            Ok(name)
        }
        "kubernetes" => {
            let mut manifest = kubernetes_manifest(p, id);
            if let Some(packet) = bootstrap {
                let secret = json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":format!("{name}-bootstrap"),"labels":{"task-runtime":id}},"type":"Opaque","stringData":{"bootstrap":packet.to_string()}});
                command(
                    &kube(p, vec!["create".into(), "-f".into(), "-".into()]),
                    Some(&secret),
                )
                .await?;
                manifest["spec"]["template"]["spec"]["containers"][0]["env"].as_array_mut().context("container environment")?.push(json!({"name":"HORDE_BOOTSTRAP_JSON","valueFrom":{"secretKeyRef":{"name":format!("{name}-bootstrap"),"key":"bootstrap"}}}));
                manifest["spec"]["template"]["spec"]["containers"][0]["env"].as_array_mut().context("container environment")?.push(json!({"name":"HORDE_BOOTSTRAP_JSON","valueFrom":{"secretKeyRef":{"name":format!("{name}-bootstrap"),"key":"bootstrap"}}}));
            }
            command(
                &kube(
                    p,
                    vec![
                        "create".into(),
                        "-f".into(),
                        "-".into(),
                        "-o".into(),
                        "json".into(),
                    ],
                ),
                Some(&manifest),
            )
            .await?;
            Ok(name)
        }
        "e2b" => {
            let result=http(p,reqwest::Method::POST,"/sandboxes",Some(json!({"templateID":p.image,"timeout":p.lifetime_seconds,"autoPause":true,"secure":true,"metadata":{"task-runtime":id},"envVars":bootstrap_env(p,bootstrap)}))).await?;
            Ok(result["sandboxID"]
                .as_str()
                .context("sandbox ID missing")?
                .into())
        }
        "daytona" => {
            let result=http(p,reqwest::Method::POST,"/sandbox",Some(json!({"name":name,"snapshot":p.image,"cpu":p.cpus,"memory":p.memory_mb.div_ceil(1024),"disk":p.disk_gb,"autoStopInterval":p.lifetime_seconds.div_ceil(60),"labels":{"task-runtime":id},"env":bootstrap_env(p,bootstrap)}))).await?;
            Ok(result["id"].as_str().context("sandbox ID missing")?.into())
        }
        _ => bail!("unsupported provider"),
    }
}
async fn lifecycle(p: &Profile, id: &str, resource: &str, action: &str) -> Result<Value> {
    ensure!(
        !resource.is_empty()
            && resource.len() <= 128
            && resource
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "invalid provider resource ID"
    );
    match p.provider.as_str() {
        "docker" => {
            let object = command(&docker(p, vec!["inspect".into(), resource.into()]), None).await?;
            ensure!(
                object[0]["Config"]["Labels"]["task-runtime"] == id,
                "resource ownership label mismatch"
            );
            if action == "runtime_reconcile" {
                return Ok(object);
            }
            let verb = match action {
                "runtime_destroy" => "rm",
                "runtime_stop" => "stop",
                "runtime_start" => "start",
                _ => bail!("operation requires enrolled runtime"),
            };
            let mut args = vec![verb.into()];
            if verb == "rm" {
                args.push("--force".into());
            }
            args.push(resource.into());
            command(&docker(p, args), None).await
        }
        "kubernetes" => {
            let object = command(
                &kube(
                    p,
                    vec![
                        "get".into(),
                        "statefulset".into(),
                        resource.into(),
                        "-o".into(),
                        "json".into(),
                    ],
                ),
                None,
            )
            .await?;
            ensure!(
                object["metadata"]["labels"]["task-runtime"] == id,
                "resource ownership label mismatch"
            );
            if action == "runtime_reconcile" {
                return Ok(object);
            }
            if action == "runtime_destroy" {
                command(
                    &kube(
                        p,
                        vec![
                            "delete".into(),
                            "statefulset".into(),
                            resource.into(),
                            "--wait=true".into(),
                        ],
                    ),
                    None,
                )
                .await
            } else {
                let replicas = match action {
                    "runtime_start" => "1",
                    "runtime_stop" => "0",
                    _ => bail!("operation requires enrolled runtime"),
                };
                command(
                    &kube(
                        p,
                        vec![
                            "scale".into(),
                            "statefulset".into(),
                            resource.into(),
                            format!("--replicas={replicas}"),
                        ],
                    ),
                    None,
                )
                .await
            }
        }
        "e2b" | "daytona" => {
            let prefix = if p.provider == "e2b" {
                "sandboxes"
            } else {
                "sandbox"
            };
            let path = format!("/{prefix}/{resource}");
            let info = http(p, reqwest::Method::GET, &path, None).await?;
            ensure!(
                info["metadata"]["task-runtime"] == id || info["labels"]["task-runtime"] == id,
                "resource ownership label mismatch"
            );
            if action == "runtime_reconcile" {
                return Ok(info);
            }
            let (method, suffix) = match (p.provider.as_str(), action) {
                (_, "runtime_destroy") => (reqwest::Method::DELETE, ""),
                ("e2b", "runtime_stop") => (reqwest::Method::POST, "/pause"),
                ("e2b", "runtime_start") => (reqwest::Method::POST, "/resume"),
                (_, "runtime_stop") => (reqwest::Method::POST, "/stop"),
                (_, "runtime_start") => (reqwest::Method::POST, "/start"),
                _ => bail!("operation requires enrolled runtime"),
            };
            http(p, method, &format!("{path}{suffix}"), None).await
        }
        _ => bail!("unsupported provider"),
    }
}
pub fn recover_operations(db: &Store) -> Result<()> {
    db.atomic(|| {
        let interrupted_update: bool = db.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM runtime_operations WHERE state='running' AND runtime!='local' AND action='runtime_update')", [], |r| r.get(0))?;
        if interrupted_update {
            management::set(db, "fleet_updates_paused", "true")?;
        }
        db.conn.execute("UPDATE runtime_operations SET state='uncertain' WHERE state='running' AND runtime!='local'", [])?;
        Ok(())
    })
}

pub async fn tick(db: &Store) -> Result<()> {
    let paused =
        i64::from(management::value(db, "fleet_updates_paused")?.as_deref() == Some("true"));
    let operations = db.rows(
        "SELECT * FROM runtime_operations WHERE state IN ('pending','waiting') AND runtime!='local' AND (?=0 OR action!='runtime_update') ORDER BY created LIMIT 1",
        &[&paused],
    )?;
    let Some(op) = operations.first() else {
        return Ok(());
    };
    let request = op["id"].as_str().context("operation")?;
    let id = op["runtime"].as_str().context("runtime")?;
    if db.conn.execute(
        "UPDATE runtime_operations SET state='running' WHERE id=? AND state IN ('pending','waiting')",
        [request],
    )? != 1
    {
        return Ok(());
    }
    let action = op["action"].as_str().context("action")?;
    let operation=async{
        let row=db.rows("SELECT * FROM managed_runtimes WHERE id=?",&[&id])?.remove(0);
        let p:Profile=serde_json::from_str(row["spec"].as_str().context("spec")?)?;p.validate()?;
        if action=="runtime_create"{
            let bootstrap=crate::enrollment::issue(db,id,&p)?;
            let resource=provision(db,&p,id,bootstrap.as_ref()).await?;
            db.conn.execute("UPDATE managed_runtimes SET resource=?,state=CASE WHEN state='ready' THEN state ELSE 'provisioned' END WHERE id=?",params![resource,id])?;
            Ok(json!({"resource":resource,"state":"provisioned","enrollment_required":p.peer.is_none()}))
        }else if ["runtime_update","runtime_restart"].contains(&action){
            let peer=p.peer.clone().or_else(||db.rows("SELECT runtime FROM runtime_enrollments WHERE runtime=? AND state='active'",&[&id]).ok().and_then(|rows|rows.first().map(|_|id.to_owned()))).context("runtime must be enrolled and have a peer mapping before remote management")?;
            let config=crate::federation::config(db)?;
            let args:Value=serde_json::from_str(op["args"].as_str().context("args")?)?;
            if action=="runtime_update"&&["docker","kubernetes"].contains(&p.provider.as_str()) {
                container_update(db,&p,id,&row,op,&config,&peer,&args).await
            }else{
                match crate::federation::call(&config,&peer,"manage",json!({"request_id":request,"action":action,"version":args["version"]})).await {
                    Ok(result)=>Ok(result),
                    Err(error)=>Ok(json!({"state":if now()>op["created"].as_i64().unwrap_or(0)+1800{"blocked"}else{"waiting"},"reason":error.to_string(),"request_id":request})),
                }
            }
        }else{
            let input:Value=serde_json::from_str(op["args"].as_str().context("operation args")?)?;
            let resource=if action=="runtime_reconcile"{input["resource"].as_str().context("resource required for reconciliation")?}else{row["resource"].as_str().context("resource ID unavailable; reconcile provisioning first")?};
            let result=if p.provider=="tailscale" {
                ensure!(action=="runtime_destroy", "Tailscale hosts support update, restart, and destroy (unenroll); host power and provisioning remain user-owned");
                json!({"unenrolled":true,"host_retained":true})
            }else{lifecycle(&p,id,resource,action).await?};
            if action=="runtime_reconcile" {db.conn.execute("UPDATE managed_runtimes SET resource=?,error=NULL WHERE id=?",params![resource,id])?;}
            if action=="runtime_destroy" {db.conn.execute("UPDATE runtime_enrollments SET state='revoked',token_hash='' WHERE runtime=?",[id])?;}
            db.conn.execute("UPDATE managed_runtimes SET state=? WHERE id=?",params![match action{"runtime_destroy"=>"removed","runtime_stop"=>"stopped",_=>"provisioned"},id])?;Ok(result)
        }
    }.await;
    match operation {
        Ok(result) => {
            let state = if ["runtime_update", "runtime_restart"].contains(&action) {
                match result["state"].as_str() {
                    Some("succeeded") => "succeeded",
                    Some("failed" | "blocked" | "uncertain") => "failed",
                    _ => "waiting",
                }
            } else {
                "succeeded"
            };
            if action == "runtime_update" && state == "failed" {
                management::set(db, "fleet_updates_paused", "true")?;
            }
            db.conn.execute(
                "UPDATE runtime_operations SET state=?,result=? WHERE id=?",
                params![state, result.to_string(), request],
            )?;
            management::event(
                db,
                "runtime.operation_progress",
                json!({"request_id":request,"runtime":id,"state":state}),
            )?;
        }
        Err(error) => {
            if action == "runtime_update" {
                management::set(db, "fleet_updates_paused", "true")?;
            }
            let message = error.to_string();
            db.conn.execute(
                "UPDATE runtime_operations SET state='uncertain',result=? WHERE id=?",
                params![json!({"error":message}).to_string(), request],
            )?;
            db.conn.execute(
                "UPDATE managed_runtimes SET error=? WHERE id=?",
                params![message, id],
            )?;
            management::event(
                db,
                "runtime.operation_uncertain",
                json!({"request_id":request,"runtime":id,"error":message}),
            )?;
        }
    }
    Ok(())
}
pub async fn collect_capacity(db: &Store) -> Result<()> {
    crate::capacity::budgets(db)?;
    crate::capacity::subscriptions(db).await?;
    for argv in load()?.capacity_commands {
        ensure!(!argv.is_empty(), "capacity command cannot be empty");
        match command(&argv, None).await {
            Ok(v) => {
                for s in serde_json::from_value::<Vec<crate::capacity::Snapshot>>(v)? {
                    crate::capacity::observe(db, &s)?;
                }
            }
            Err(_) => {
                management::event(db, "account.refresh_failed", json!({"capacity":"unknown"}))?;
            }
        }
    }
    Ok(())
}
pub fn config_path() -> PathBuf {
    crate::config::Settings::user_path().with_file_name("runtimes.toml")
}

fn bootstrap_env(p: &Profile, bootstrap: Option<&Value>) -> Value {
    let mut env = json!({"HORDE_CONCURRENCY":p.concurrency.to_string()});
    if let Some(packet) = bootstrap {
        env["HORDE_BOOTSTRAP_JSON"] = json!(packet.to_string());
    }
    env
}

async fn remote_status(
    config: &crate::network::NetworkConfig,
    peer: &str,
    action: &str,
) -> Result<Value> {
    crate::federation::call(config, peer, "manage", json!({"action":action})).await
}
async fn replace_container(db: &Store, p: &Profile, id: &str, resource: &str) -> Result<()> {
    if p.provider == "docker" {
        lifecycle(p, id, resource, "runtime_destroy").await?;
        provision(db, p, id, None).await?;
    } else {
        let object = command(
            &kube(
                p,
                vec![
                    "get".into(),
                    "statefulset".into(),
                    resource.into(),
                    "-o".into(),
                    "json".into(),
                ],
            ),
            None,
        )
        .await?;
        ensure!(
            object["metadata"]["labels"]["task-runtime"] == id,
            "resource ownership mismatch"
        );
        command(
            &kube(
                p,
                vec![
                    "set".into(),
                    "image".into(),
                    format!("statefulset/{resource}"),
                    format!("task={}", p.image),
                ],
            ),
            None,
        )
        .await?;
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
async fn container_update(
    db: &Store,
    p: &Profile,
    id: &str,
    row: &Value,
    op: &Value,
    config: &crate::network::NetworkConfig,
    peer: &str,
    args: &Value,
) -> Result<Value> {
    let version = args["version"].as_str().context("version required")?;
    let request = op["id"].as_str().context("operation")?;
    let mut progress: Value = op["result"]
        .as_str()
        .and_then(|v| serde_json::from_str(v).ok())
        .unwrap_or(json!({}));
    if progress["image"].is_null() {
        let manifest = crate::update::release(version).await?;
        let rollback_safe = manifest.schema_max <= 2;
        let image = manifest.image.context("release has no container image")?;
        let digest = image
            .strip_prefix("ghcr.io/asomervell/horde@sha256:")
            .or_else(|| image.strip_prefix("ghcr.io/asomervell/task@sha256:"))
            .context("untrusted release image")?;
        ensure!(
            digest.len() == 64 && digest.bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid image digest"
        );
        progress = json!({"state":"waiting","phase":"draining","image":image,"rollback_safe":rollback_safe});
        db.conn.execute(
            "UPDATE runtime_operations SET result=? WHERE id=?",
            params![progress.to_string(), request],
        )?;
    }
    let resource = row["resource"].as_str().context("resource ID missing")?;
    if progress["phase"] == "draining" {
        if now() > op["created"].as_i64().unwrap_or(0) + 1800 {
            return Ok(
                json!({"state":"blocked","reason":"drain timed out; runtime remains draining"}),
            );
        }
        match remote_status(config, peer, "runtime_drain").await {
            Ok(status) if status["drained"] == true => {}
            Ok(_) => return Ok(progress),
            Err(error) => {
                progress["last_error"] = json!(error.to_string());
                return Ok(progress);
            }
        }
        let mut next = p.clone();
        next.image = progress["image"].as_str().context("image")?.into();
        progress["phase"] = json!("replacing");
        db.conn.execute(
            "UPDATE runtime_operations SET result=? WHERE id=?",
            params![progress.to_string(), request],
        )?;
        replace_container(db, &next, id, resource).await?;
        progress["phase"] = json!("health");
        progress["replaced_at"] = json!(now());
        return Ok(progress);
    }
    ensure!(
        progress["phase"] == "health",
        "container replacement is uncertain; inspect provider state before reconciliation"
    );
    if let Ok(status) = remote_status(config, peer, "runtime_status").await
        && status["version"] == version
    {
        remote_status(config, peer, "runtime_resume").await?;
        let mut next = p.clone();
        next.image = progress["image"].as_str().context("image")?.into();
        db.conn.execute(
            "UPDATE managed_runtimes SET spec=?,version=? WHERE id=?",
            params![serde_json::to_string(&next)?, version, id],
        )?;
        return Ok(json!({"state":"succeeded","version":version}));
    }
    if now() > progress["replaced_at"].as_i64().unwrap_or(0) + 120 {
        if progress["rollback_safe"] != true {
            return Ok(
                json!({"state":"blocked","reason":"new version failed health check; schema compatibility does not permit automatic rollback; runtime remains held for inspection"}),
            );
        }
        replace_container(db, p, id, resource).await?;
        return Ok(
            json!({"state":"failed","reason":"new version failed health check; previous image restored; runtime remains draining for inspection"}),
        );
    }
    Ok(progress)
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn local_id_is_rejected_before_queuing() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let db = Store::open(temp.path())?;
        assert!(
            dispatch(
                &db,
                "runtime_create",
                &json!({"id":"local","profile":"any","request_id":"create"})
            )
            .unwrap_err()
            .to_string()
            .contains("reserved")
        );
        assert!(db.rows("SELECT * FROM runtime_operations", &[])?.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn interrupted_rollout_pauses_queued_updates_across_recovery() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let db = Store::open(temp.path())?;
        for (id, state) in [("first", "running"), ("next", "pending")] {
            db.conn.execute("INSERT INTO runtime_operations(id,runtime,action,args,state,created) VALUES(?,'worker','runtime_update','{}',?,0)", params![id,state])?;
        }
        recover_operations(&db)?;
        recover_operations(&db)?;
        tick(&db).await?;
        assert_eq!(
            management::value(&db, "fleet_updates_paused")?.as_deref(),
            Some("true")
        );
        let state: String = db.conn.query_row(
            "SELECT state FROM runtime_operations WHERE id='next'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(state, "pending");
        let state: String = db.conn.query_row(
            "SELECT state FROM runtime_operations WHERE id='first'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(state, "uncertain");
        Ok(())
    }

    #[tokio::test]
    async fn incompatible_container_is_held_without_provider_rollback() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let db = Store::open(temp.path())?;
        let p: Profile =
            serde_json::from_value(json!({"provider":"docker","image":"old@sha256:fixture"}))?;
        for rollback_safe in [json!(false), Value::Null] {
            let progress = json!({"image":"new@sha256:fixture","phase":"health","replaced_at":0,"rollback_safe":rollback_safe});
            let op = json!({"id":"update","result":progress.to_string()});
            let config = crate::network::NetworkConfig::default();
            let result = container_update(
                &db,
                &p,
                "worker",
                &json!({"resource":"worker"}),
                &op,
                &config,
                "missing",
                &json!({"version":"0.2.1"}),
            )
            .await?;
            assert_eq!(result["state"], "blocked");
            assert!(
                result["reason"]
                    .as_str()
                    .unwrap()
                    .contains("schema compatibility")
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod branding_tests {
    use super::*;
    #[test]
    fn bootstrap_supports_both_generations_of_remote_images() {
        let packet = json!({"fixture": "bootstrap"});
        let env = bootstrap_env(&Profile::default(), Some(&packet));
        assert_eq!(env["HORDE_CONCURRENCY"], "4");
        assert_eq!(env["HORDE_BOOTSTRAP_JSON"], packet.to_string());
    }
}
