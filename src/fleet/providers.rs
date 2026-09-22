//! Provider resource construction and ownership-checked lifecycle commands.
use super::*;
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
pub(super) async fn provision(
    db: &Store,
    p: &Profile,
    id: &str,
    bootstrap: Option<&Value>,
) -> Result<String> {
    let name = format!("horde-{id}");
    match p.provider.as_str() {
        "ax" => super::ax::provision(db, p, id, bootstrap).await,
        "lima" => crate::lima::provision(db, p, id, bootstrap).await,
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
pub(super) async fn lifecycle(
    db: &Store,
    p: &Profile,
    id: &str,
    resource: &str,
    action: &str,
) -> Result<Value> {
    ensure!(
        !resource.is_empty()
            && resource.len() <= 128
            && resource
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "invalid provider resource ID"
    );
    match p.provider.as_str() {
        "ax" => super::ax::lifecycle(db, p, id, resource, action).await,
        "lima" => crate::lima::lifecycle(db, p, id, resource, action).await,
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
