//! Owned, disposable app lifecycles. No application output is persisted before redaction.
use crate::{
    executor::{Invocation, clean_command},
    secrets,
    store::{Store, id, now},
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Environment {
    pub docker_context: Option<String>,
    pub env_file: Option<String>,
    #[schemars(extend("enum" = ["process", "compose"]))]
    pub runner: String,
    /// Start command argv, required for the process runner.
    pub start: Vec<String>,
    /// Test command argv; required for both process and Compose runners.
    #[schemars(length(min = 1))]
    pub test: Vec<String>,
    pub ready_url: String,
    pub compose_file: String,
    pub services: Vec<String>,
    pub ready_service: Option<String>,
    #[schemars(range(min = 1, max = 86400))]
    pub timeout_seconds: u64,
    /// Positive readiness deadline, no greater than timeout_seconds.
    #[schemars(range(min = 1, max = 86400))]
    pub readiness_seconds: u64,
    pub allow_external_resources: bool,
    #[schemars(range(min = 64))]
    pub memory_mb: u64,
    #[schemars(extend("exclusiveMinimum" = 0))]
    pub cpus: f64,
}
impl Default for Environment {
    fn default() -> Self {
        Self {
            docker_context: None,
            env_file: Some(".env".into()),
            runner: "process".into(),
            start: vec![],
            test: vec![],
            ready_url: "http://127.0.0.1:${PORT}/".into(),
            compose_file: "compose.yaml".into(),
            services: vec![],
            ready_service: None,
            timeout_seconds: 1800,
            readiness_seconds: 60,
            allow_external_resources: false,
            memory_mb: 2048,
            cpus: 2.0,
        }
    }
}
impl Environment {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            ["process", "compose"].contains(&self.runner.as_str()),
            "unsupported app runner"
        );
        ensure!(
            self.timeout_seconds > 0
                && self.timeout_seconds <= 86400
                && self.readiness_seconds > 0
                && self.readiness_seconds <= self.timeout_seconds,
            "invalid app timeout"
        );
        ensure!(!self.test.is_empty(), "environment requires a test command");
        ensure!(
            self.runner != "process" || !self.start.is_empty(),
            "process environment requires a start command"
        );
        ensure!(
            self.memory_mb >= 64 && self.cpus.is_finite() && self.cpus > 0.0,
            "invalid environment resource limits"
        );
        crate::store::scope(&self.compose_file)?;
        Ok(())
    }
}
fn substitute(argv: &[String], port: u16) -> Vec<String> {
    argv.iter()
        .map(|a| a.replace("${PORT}", &port.to_string()))
        .collect()
}
async fn bounded(mut read: impl tokio::io::AsyncRead + Unpin) -> String {
    let mut all = Vec::new();
    let mut buf = [0; 8192];
    while let Ok(n) = read.read(&mut buf).await {
        if n == 0 {
            break;
        }
        all.extend_from_slice(&buf[..n]);
        if all.len() > 1024 * 1024 {
            all.drain(..all.len() - 1024 * 1024);
        }
    }
    String::from_utf8_lossy(&all).into_owned()
}
struct Owned {
    root: PathBuf,
    id: String,
    pid: Option<u32>,
    finished: bool,
}
impl Drop for Owned {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        if let Some(pid) = self.pid {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        if let Ok(db) = Store::open(&self.root) {
            let _ = db.conn.execute(
                "UPDATE app_environments SET state='cleanup_pending' WHERE id=?",
                [&self.id],
            );
        }
    }
}
fn compose_argv(
    spec: &Environment,
    project: &str,
    file: &std::path::Path,
    override_file: Option<&std::path::Path>,
) -> Vec<String> {
    let mut a = vec![
        "docker".into(),
        "compose".into(),
        "--project-name".into(),
        project.into(),
        "--file".into(),
        file.to_string_lossy().into_owned(),
    ];
    if let Some(file) = override_file {
        a.extend(["--file".into(), file.to_string_lossy().into_owned()]);
    }
    if let Some(context) = &spec.docker_context {
        a.splice(1..1, ["--context".into(), context.clone()]);
    }
    a
}
async fn command(
    argv: &[String],
    workspace: &std::path::Path,
    values: &BTreeMap<String, String>,
    timeout: u64,
) -> Result<Value> {
    crate::executor::run_command_env(argv, workspace, timeout, None, values).await
}
/// Ports held by app environments that have not finished starting.
///
/// A port is allocated by binding `127.0.0.1:0`, and that listener has to close
/// before the app can bind it, so from then until the app is listening the
/// kernel is free to hand the same port to the next allocation. Remembering the
/// number across that window stops Horde from racing itself. A process outside
/// Horde can still take it, which is diagnosed when the app exits.
static RESERVED_PORTS: std::sync::Mutex<std::collections::BTreeSet<u16>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

fn reserved() -> std::sync::MutexGuard<'static, std::collections::BTreeSet<u16>> {
    // A panic while holding this leaves only a set of numbers behind, and
    // refusing to start every later environment is worse than continuing.
    RESERVED_PORTS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Releases the reservation when the environment has finished starting.
struct ReservedPort(u16);
impl Drop for ReservedPort {
    fn drop(&mut self) {
        reserved().remove(&self.0);
    }
}

/// Bind a loopback port that no other starting environment is holding.
///
/// Losing listeners stay open until a free port is found, so the kernel offers
/// a different one on each attempt rather than the same reserved port.
fn reserve_port() -> Result<(std::net::TcpListener, u16, ReservedPort)> {
    let mut contended = Vec::new();
    for _ in 0..64 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        if reserved().insert(port) {
            return Ok((listener, port, ReservedPort(port)));
        }
        contended.push(listener);
    }
    bail!("no loopback port free of in-flight app environments")
}

pub async fn execute(i: &Invocation<'_>, spec: &Environment) -> Result<Value> {
    ensure!(i.settings.allow_commands, "commands disabled");
    spec.validate()?;
    let r = crate::delegation::root(i.db, i.task)?;
    let tree = crate::delegation::tree(i.db, i.task)?;
    let limits: crate::delegation::Limits =
        serde_json::from_str(tree["limits"].as_str().context("limits")?)?;
    let eid = format!("task-{}", id());
    // Held until this returns, so a concurrent start cannot be handed the same
    // port during the window between closing the listener and the app binding.
    let (listener, port, _reserved) = reserve_port()?;
    let mut values = secrets::values(i.db, i.task)?;
    if spec.runner == "process" {
        values.insert("PORT".into(), port.to_string());
    }
    let mut endpoint = spec.ready_url.replace("${PORT}", &port.to_string());
    let url = reqwest::Url::parse(&endpoint)?;
    ensure!(
        [Some("127.0.0.1"), Some("localhost"), Some("::1")].contains(&url.host_str())
            && ["http", "https"].contains(&url.scheme()),
        "readiness must use a loopback HTTP(S) URL"
    );
    i.db.atomic(||{ensure!(available(i.db,i.task)?,"root environment limit reached");let count:i64=i.db.conn.query_row("SELECT COUNT(*) FROM app_environments e JOIN task_tree t ON t.task=e.task WHERE t.root=? AND e.state NOT IN ('removed')",[&r],|r|r.get(0))?;
 ensure!(count<limits.environments as i64,"root environment limit reached; clean up held environments");
 i.db.conn.execute("INSERT INTO app_environments(id,task,attempt,kind,state,spec,workspace,created,expires) VALUES(?,?,?,?,'preparing',?,?,?,?)",rusqlite::params![eid,i.task,i.attempt,spec.runner,serde_json::to_string(spec)?,i.workspace.to_string_lossy(),now(),now()+spec.timeout_seconds as i64])?;Ok(())})?;
    let mut owned = Owned {
        root: i.db.root.clone(),
        id: eid.clone(),
        pid: None,
        finished: false,
    };
    let scratch = i.db.root.join("environment-private").join(&eid);
    std::fs::create_dir_all(&scratch)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700))?;
    let _env_file = materialize_env(i, &scratch, &values, spec.env_file.as_deref())?;
    let mut compose = None;
    let mut child = None;
    let mut logs = None;
    let operation = async {
        tokio::time::timeout(Duration::from_secs(spec.timeout_seconds), async {
            loop {
                match crate::federation::environment_lease(i.db, i.task, true).await {
                    Ok(()) => break Ok::<_, anyhow::Error>(()),
                    Err(e) if e.to_string().contains("environment limit") => {
                        tokio::time::sleep(Duration::from_millis(200)).await
                    }
                    Err(e) => break Err(e),
                }
            }
        })
        .await
        .context("waiting for root environment capacity timed out")??;

        if spec.runner == "process" {
            let argv = substitute(&spec.start, port);
            let mut cmd = Command::from(clean_command(&argv[0]));
            cmd.args(&argv[1..])
                .current_dir(i.workspace)
                .envs(&values)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            drop(listener);
            let mut c = cmd.spawn().context("start application")?;
            owned.pid = c.id();
            if let Some(identity) = c.id().and_then(process_identity) {
                i.db.conn.execute(
                    "INSERT INTO app_process_identity VALUES(?,?)",
                    rusqlite::params![eid, identity],
                )?;
            }
            i.db.conn.execute(
                "UPDATE app_environments SET state='starting',pid=? WHERE id=?",
                rusqlite::params![c.id(), eid],
            )?;
            logs = Some((
                tokio::spawn(bounded(c.stdout.take().context("app stdout")?)),
                tokio::spawn(bounded(c.stderr.take().context("app stderr")?)),
            ));
            child = Some(c);
        } else {
            drop(listener);
            let file = i.workspace.join(&spec.compose_file).canonicalize()?;
            ensure!(
                file.starts_with(i.workspace.canonicalize()?),
                "Compose file escapes workspace"
            );
            let base = compose_argv(spec, &eid, &file, None);
            let mut check = base.clone();
            check.extend(["config".into(), "--format".into(), "json".into()]);
            let mut config_cmd = clean_command(&check[0]);
            config_cmd
                .args(&check[1..])
                .current_dir(i.workspace)
                .envs(&values);
            let config =
                crate::executor::run_process(config_cmd, None, 60, Some((i.db, i.attempt))).await?;
            ensure!(
                config["success"] == true,
                "Compose config validation failed"
            );
            let config: Value =
                serde_json::from_str(config["stdout"].as_str().context("Compose config")?)?;
            validate_compose(&config, spec)?;
            if let Some(selected) = &spec.ready_service {
                ensure!(
                    config["services"].get(selected).is_some(),
                    "readiness service is absent from Compose configuration"
                );
                ensure!(
                    spec.services.is_empty() || spec.services.contains(selected),
                    "readiness service must be included in selected services"
                );
            }
            let service_count = config["services"]
                .as_object()
                .context("services")?
                .len()
                .max(1) as u64;
            ensure!(
                spec.memory_mb / service_count >= 64,
                "environment memory budget is too small for this service count"
            );
            let mut overrides = serde_json::Map::new();
            for (name, service) in config["services"].as_object().context("Compose services")? {
                // Override named ports to loopback ephemeral allocations, preserve target/container ports.
                let ports=service["ports"].as_array().map(|ports|ports.iter().map(|p|json!({"target":p["target"],"host_ip":"127.0.0.1","published":"0","protocol":p["protocol"].as_str().unwrap_or("tcp")})).collect::<Vec<_>>()).unwrap_or_default();
                let mut environment = service["environment"]
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                for (k, v) in &values {
                    environment.insert(k.clone(), json!(v));
                }
                overrides.insert(name.clone(),json!({"environment":environment,"mem_limit":format!("{}m",spec.memory_mb/service_count),"cpus":spec.cpus/service_count as f64,"ports":ports}));
            }
            // Use the fully resolved configuration as one file, so Compose merge cannot retain fixed ports.
            let mut isolated = config.clone();
            isolated["name"] = json!(eid);
            for (name, over) in overrides {
                for (k, v) in over.as_object().context("override")? {
                    isolated["services"][&name][k] = v.clone();
                }
            }
            for service in isolated["services"]
                .as_object_mut()
                .context("services")?
                .values_mut()
            {
                if service["deploy"]["resources"]["limits"].is_object() {
                    service["deploy"]["resources"]["limits"]["memory"] =
                        json!(format!("{}m", spec.memory_mb / service_count));
                    service["deploy"]["resources"]["limits"]["cpus"] =
                        json!((spec.cpus / service_count as f64).to_string());
                }
            }
            for kind in ["volumes", "networks"] {
                if let Some(items) = isolated.get_mut(kind).and_then(Value::as_object_mut) {
                    for (name, item) in items {
                        if item["external"] != true {
                            item["name"] = json!(format!("{eid}_{name}"));
                        }
                    }
                }
            }
            let resolved = scratch.join("compose.json");
            crate::secrets::write_private(&resolved, serde_json::to_vec(&isolated)?.as_slice())?;
            let base = compose_argv(spec, &eid, &resolved, None);
            compose = Some(base.clone());
            let mut up = base;
            up.extend([
                "up".into(),
                "--detach".into(),
                "--wait".into(),
                "--wait-timeout".into(),
                spec.readiness_seconds.to_string(),
            ]);
            up.extend(spec.services.clone());
            let result =
                invocation_command(i, &up, i.workspace, &values, spec.readiness_seconds + 30)
                    .await?;
            ensure!(
                result["success"] == true,
                "Compose startup failed: {}",
                result
            );
            let target = config["services"]
                .as_object()
                .context("services")?
                .iter()
                .filter(|(name, _)| {
                    spec.ready_service
                        .as_ref()
                        .is_none_or(|selected| selected == *name)
                })
                .find_map(|(name, service)| {
                    service["ports"]
                        .as_array()
                        .and_then(|ports| {
                            ports
                                .iter()
                                .find(|p| p["protocol"].as_str().unwrap_or("tcp") == "tcp")
                        })
                        .map(|p| (name.clone(), p["target"].to_string()))
                });
            if let Some((service, target)) = target {
                let mut port_args = compose.clone().context("Compose invocation")?;
                port_args.extend(["port".into(), service, target.trim_matches('"').into()]);
                let port_result =
                    invocation_command(i, &port_args, i.workspace, &values, 10).await?;
                ensure!(
                    port_result["success"] == true,
                    "cannot discover published app port"
                );
                let address = port_result["stdout"]
                    .as_str()
                    .context("published port")?
                    .lines()
                    .next()
                    .context("no published port")?;
                let socket: std::net::SocketAddr =
                    address.parse().context("unsupported published address")?;
                ensure!(
                    socket.ip().is_loopback(),
                    "published application port must be loopback"
                );
                let mut app_url = reqwest::Url::parse(&endpoint)?;
                app_url
                    .set_port(Some(socket.port()))
                    .map_err(|_| anyhow::anyhow!("invalid app URL port"))?;
                endpoint = app_url.to_string();
            } else {
                endpoint.clear();
            }
        }
        if !endpoint.is_empty() {
            let client = reqwest::Client::builder()
                // Readiness endpoints are validated as loopback; ambient proxies
                // must never intercept requests to a task-owned app.
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(2))
                .build()?;
            tokio::time::timeout(Duration::from_secs(spec.readiness_seconds), async {
                loop {
                    if let Some(c) = child.as_mut()
                        && c.try_wait()?.is_some()
                    {
                        // The allocated port is released just before the app is
                        // started, so a busy machine can take it in between. If
                        // something else holds it now, the app lost that race
                        // rather than failing on its own terms. Say which, so a
                        // caller does not read this as a defect in their app.
                        // TcpListener sets SO_REUSEADDR, so a closed listener of
                        // our own does not report as taken.
                        bail!(
                            if std::net::TcpListener::bind(("127.0.0.1", port)).is_err() {
                                format!(
                                    "application exited before readiness: port {port} was taken by another process between allocation and startup. Retry the step, or give it attempts > 1."
                                )
                            } else {
                                "application exited before readiness".to_owned()
                            }
                        );
                    }
                    if client
                        .get(&endpoint)
                        .send()
                        .await
                        .is_ok_and(|r| r.status().is_success())
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Ok::<_, anyhow::Error>(())
            })
            .await
            .context("application readiness timeout")??;
        }
        i.db.conn.execute(
            "UPDATE app_environments SET state='testing' WHERE id=?",
            [&eid],
        )?;
        let mut test_values = values.clone();
        test_values.insert("HORDE_APP_URL".into(), endpoint.clone());
        test_values.insert("HORDE_COMPOSE_PROJECT".into(), eid.clone());
        if let Some(base) = &compose {
            test_values.insert(
                "HORDE_COMPOSE_FILE".into(),
                base.last().cloned().unwrap_or_default(),
            );
        }
        let result = invocation_command(
            i,
            &substitute(&spec.test, port),
            i.workspace,
            &test_values,
            spec.timeout_seconds,
        )
        .await?;
        ensure!(
            result["success"] == true,
            "application test failed: {}",
            secrets::redact(i.db, i.task, &result)
        );
        Ok::<_, anyhow::Error>(result)
    };
    let result = tokio::time::timeout(Duration::from_secs(spec.timeout_seconds), operation)
        .await
        .map_err(|_| anyhow::anyhow!("application environment lifetime expired"))
        .and_then(|r| r);
    i.db.conn.execute(
        "UPDATE app_environments SET state='cleanup_pending' WHERE id=?",
        [&eid],
    )?;
    let mut evidence = json!({"test":match &result{Ok(v)=>v.clone(),Err(e)=>json!({"error":e.to_string()})},"endpoint":endpoint});
    if let Some(mut c) = child {
        if let Some(pid) = owned.pid {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = c.wait().await;
        owned.pid = None;
    }
    if let Some((out, err)) = logs {
        evidence["stdout"] = json!(out.await.unwrap_or_default());
        evidence["stderr"] = json!(err.await.unwrap_or_default());
    }
    let cleanup = if let Some(base) = compose {
        let mut log = base.clone();
        log.extend([
            "logs".into(),
            "--no-color".into(),
            "--tail".into(),
            "200".into(),
        ]);
        if let Ok(v) = invocation_command(i, &log, i.workspace, &values, 30).await {
            evidence["compose_logs"] = v;
        }
        let mut down = base;
        down.extend(["down".into(), "--volumes".into(), "--remove-orphans".into()]);
        invocation_command(i, &down, i.workspace, &values, 60)
            .await
            .and_then(|v| {
                ensure!(v["success"] == true, "Compose cleanup failed: {}", v);
                Ok(())
            })
    } else {
        Ok(())
    };
    evidence = secrets::redact_json(&evidence, &values);
    i.db.conn.execute(
        "UPDATE app_environments SET state=?,evidence=? WHERE id=?",
        rusqlite::params![
            if cleanup.is_ok() {
                "removed"
            } else {
                "cleanup_pending"
            },
            evidence.to_string(),
            eid
        ],
    )?;
    if cleanup.is_ok() {
        crate::federation::environment_lease(i.db, i.task, false).await?;
        owned.finished = true;
        std::fs::remove_dir_all(&scratch)?;
    }
    cleanup?;
    result?;
    Ok(
        json!({"accepted":true,"result":"Application started, became ready, passed tests, and was removed","environment":eid,"evidence":evidence}),
    )
}
pub fn validate_compose(config: &Value, spec: &Environment) -> Result<()> {
    for service in config["services"]
        .as_object()
        .context("Compose services")?
        .values()
    {
        ensure!(
            service["container_name"].is_null(),
            "Compose container_name conflicts with task isolation"
        );
        ensure!(
            service["network_mode"].is_null() && service["privileged"] != true,
            "host networking and privileged containers are not supported"
        );
        if !spec.allow_external_resources {
            for volume in service["volumes"].as_array().into_iter().flatten() {
                ensure!(
                    volume["type"] != "bind" || volume["read_only"] == true,
                    "writable bind mounts require allow_external_resources"
                );
            }
            ensure!(
                service["devices"].as_array().is_none_or(Vec::is_empty),
                "host devices require allow_external_resources"
            );
        }
    }
    for kind in ["volumes", "networks"] {
        for resource in config[kind]
            .as_object()
            .into_iter()
            .flat_map(|v| v.values())
        {
            ensure!(
                spec.allow_external_resources || resource["external"] != true,
                "external Compose resources require allow_external_resources"
            );
        }
    }
    Ok(())
}
pub async fn reconcile(db: &Store) -> Result<()> {
    reconcile_inner(db, true).await
}
pub async fn cleanup_pending(db: &Store) -> Result<()> {
    reconcile_inner(db, false).await
}
async fn reconcile_inner(db: &Store, startup: bool) -> Result<()> {
    for row in db.rows("SELECT * FROM app_environments WHERE state!='removed' AND (? OR (state IN ('cleanup_pending','held') AND NOT EXISTS(SELECT 1 FROM attempts a WHERE a.id=app_environments.attempt AND a.state='running')))", &[&startup])? {
        let eid = row["id"].as_str().context("environment")?;
        let mut group_held=false;
        for group in db.rows("SELECT pid,identity FROM app_process_groups WHERE environment=?",&[&eid])? {
            let pid=group["pid"].as_i64().context("process group")? as i32;
            if crate::executor::process_alive(pid) {
                if process_identity(pid as u32).is_some_and(|identity|group["identity"]==identity){unsafe{libc::kill(-pid,libc::SIGKILL);}}
                else {group_held=true;}
            }else{unsafe{libc::kill(-pid,libc::SIGKILL);}}
        }
        if group_held{db.conn.execute("UPDATE app_environments SET state='held' WHERE id=?",[eid])?;continue;}

        if row["kind"] == "process" {
            // A shell leader can exit while its children retain the owned group.
            if let Some(pid) = row["pid"].as_i64()
                && !crate::executor::process_alive(pid as i32)
            {
                unsafe { libc::kill(-(pid as i32), libc::SIGKILL); }
            }
            if let Some(pid) = row["pid"].as_i64()
                && crate::executor::process_alive(pid as i32)
            {
                let identities = db.rows(
                    "SELECT identity FROM app_process_identity WHERE environment=?",
                    &[&eid],
                )?;
                if identities.first().is_some_and(|v| {
                    process_identity(pid as u32).is_some_and(|identity| v["identity"] == identity)
                }) {
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                    for _ in 0..10 {
                        if !crate::executor::process_alive(pid as i32) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
                if crate::executor::process_alive(pid as i32) {
                    db.conn
                        .execute("UPDATE app_environments SET state='held' WHERE id=?", [eid])?;
                    continue;
                }
            }
        } else {
            let file = db
                .root
                .join("environment-private")
                .join(eid)
                .join("compose.json");
            if file.exists() {
                let spec: Environment =
                    serde_json::from_str(row["spec"].as_str().context("spec")?)?;
                let mut args = compose_argv(&spec, eid, &file, None);
                args.extend(["down".into(), "--volumes".into(), "--remove-orphans".into()]);
                if !command(
                    &args,
                    std::path::Path::new(row["workspace"].as_str().context("workspace")?),
                    &BTreeMap::new(),
                    60,
                )
                .await
                .is_ok_and(|v| v["success"] == true)
                {
                    continue;
                }
            }
        }
        if crate::federation::environment_lease(
            db,
            row["task"].as_str().context("task")?,
            false,
        )
        .await
        .is_err()
        {
            db.conn.execute(
                "UPDATE app_environments SET state='cleanup_pending' WHERE id=?",
                [eid],
            )?;
            continue;
        }
        let dir = db.root.join("environment-private").join(eid);
        cleanup_env_manifest(&dir)?;
        if dir.exists() {
            std::fs::remove_dir_all(dir)?;
        }
        db.conn.execute(
            "UPDATE app_environments SET state='removed' WHERE id=?",
            [eid],
        )?;
    }
    Ok(())
}

pub fn available(db: &Store, oid: &str) -> Result<bool> {
    let root = crate::delegation::root(db, oid)?;
    let tree = crate::delegation::tree(db, oid)?;
    let limits: crate::delegation::Limits =
        serde_json::from_str(tree["limits"].as_str().context("limits")?)?;
    let local:i64=db.conn.query_row("SELECT COUNT(*) FROM app_environments e JOIN task_tree t ON t.task=e.task WHERE t.root=? AND e.state!='removed'",[&root],|r|r.get(0))?;
    let remote:i64=db.conn.query_row("SELECT COUNT(*) FROM remote_environment_leases e JOIN task_tree t ON t.task=e.task WHERE t.root=?",[&root],|r|r.get(0))?;
    Ok(local + remote < limits.environments as i64)
}

pub(crate) fn process_identity(pid: u32) -> Option<String> {
    let out = clean_command("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart=", "-o", "pgid="])
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    Some(crate::store::hash(&out.stdout))
}
struct Materialized {
    path: PathBuf,
    inode: u64,
}
impl Drop for Materialized {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        if std::fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.ino() == self.inode && m.file_type().is_file())
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
fn materialize_env(
    i: &Invocation<'_>,
    scratch: &std::path::Path,
    values: &BTreeMap<String, String>,
    name: Option<&str>,
) -> Result<Option<Materialized>> {
    use std::os::unix::fs::MetadataExt;
    let Some(name) = name else { return Ok(None) };
    if i.db
        .rows("SELECT name FROM task_bundles WHERE task=?", &[&i.task])?
        .is_empty()
    {
        return Ok(None);
    }
    let relative = crate::store::scope(name)?;
    ensure!(relative != ".", "environment file must name a file");
    let path = i.workspace.join(&relative);
    let mut ancestor = Some(path.as_path());
    while let Some(p) = ancestor {
        if p == i.workspace {
            break;
        }
        if let Ok(m) = std::fs::symlink_metadata(p) {
            ensure!(
                !m.file_type().is_symlink(),
                "environment file cannot traverse a symlink"
            );
        }
        ancestor = p.parent();
    }
    if path.exists() {
        ensure!(path.is_file(), "environment path is not a file");
        return Ok(None);
    }
    std::fs::create_dir_all(path.parent().context("environment parent")?)?;
    let mut bytes = String::new();
    for (k, v) in values {
        bytes.push_str(&format!("{k}={}\n", serde_json::to_string(v)?));
    }
    ensure!(
        relative
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-/".contains(&c)),
        "environment file path contains unsupported characters"
    );
    if let Ok(exclude) = crate::git::run(i.workspace, &["rev-parse", "--git-path", "info/exclude"])
    {
        let exclude = PathBuf::from(exclude);
        let exclude = if exclude.is_absolute() {
            exclude
        } else {
            i.workspace.join(exclude)
        };
        std::fs::create_dir_all(exclude.parent().context("exclude directory")?)?;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(exclude)?;
        use std::io::Write;
        writeln!(file, "\n/{relative}")?;
    }
    crate::secrets::write_private(&path, bytes.as_bytes())?;
    let inode = std::fs::metadata(&path)?.ino();
    let owned = Materialized { path, inode };
    crate::secrets::write_private(
        &scratch.join("env-manifest.json"),
        json!({"path":owned.path,"inode":inode})
            .to_string()
            .as_bytes(),
    )?;
    Ok(Some(owned))
}
fn cleanup_env_manifest(dir: &std::path::Path) -> Result<()> {
    let file = dir.join("env-manifest.json");
    if file.exists() {
        let value: Value = serde_json::from_slice(&std::fs::read(file)?)?;
        drop(Materialized {
            path: PathBuf::from(value["path"].as_str().context("environment path")?),
            inode: value["inode"].as_u64().context("inode")?,
        });
    }
    Ok(())
}

async fn invocation_command(
    i: &Invocation<'_>,
    argv: &[String],
    workspace: &std::path::Path,
    values: &BTreeMap<String, String>,
    timeout: u64,
) -> Result<Value> {
    crate::executor::run_command_env(argv, workspace, timeout, Some((i.db, i.attempt)), values)
        .await
}

#[cfg(test)]
mod port_reservation {
    use super::*;

    /// The window this closes: the allocating listener must be closed before
    /// the app can bind, and until the app is listening the kernel may hand the
    /// same port to the next allocation. Closing the listener while keeping the
    /// reservation reproduces exactly that state.
    #[test]
    fn a_port_stays_reserved_after_its_listener_closes() {
        let (listener, port, reservation) = reserve_port().unwrap();
        drop(listener);
        assert!(reserved().contains(&port));

        let mut held = Vec::new();
        for _ in 0..64 {
            let (listener, next, guard) = reserve_port().unwrap();
            assert_ne!(
                next, port,
                "handed out a port an environment is starting on"
            );
            // Close each listener too, so every attempt competes for the same
            // pool of free ports rather than being kept apart by open sockets.
            drop(listener);
            held.push(guard);
        }

        drop(reservation);
        assert!(
            !reserved().contains(&port),
            "reservation outlived the start"
        );
    }

    /// Whether the kernel happens to re-offer a just-freed port is luck, so
    /// this does not rely on provoking a collision. Each start asserts its own
    /// port is held while it runs, which is the guarantee that makes sharing
    /// impossible, and the ports are checked to be distinct on top of that.
    #[test]
    fn concurrent_starts_never_share_a_port() {
        let threads: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    (0..16)
                        .map(|_| {
                            let (listener, port, guard) = reserve_port().unwrap();
                            drop(listener);
                            assert!(
                                reserved().contains(&port),
                                "start {port} is not holding its port"
                            );
                            (port, guard)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let allocated: Vec<_> = threads
            .into_iter()
            .flat_map(|t| t.join().unwrap())
            .collect();

        let unique: std::collections::BTreeSet<u16> = allocated.iter().map(|(p, _)| *p).collect();
        assert_eq!(
            unique.len(),
            allocated.len(),
            "two starts were given one port"
        );
    }
}
