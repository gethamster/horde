use horde::{network::NetworkConfig, remote_submit, store::Store};
use serde_json::json;

fn fixture() -> (tempfile::TempDir, Store, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@localhost"],
        vec!["commit", "--allow-empty", "-m", "initial"],
    ] {
        horde::git::run(&repo, &args).unwrap();
    }
    let db = Store::open(&dir.path().join("data")).unwrap();
    let config = NetworkConfig {
        delegate_peers: vec!["apollo".into()],
        ..Default::default()
    };
    horde::federation::configure(&db.root, &config).unwrap();
    db.conn.execute("INSERT INTO managed_runtimes(id,profile,spec,state,created) VALUES('apollo','test','{}','ready',0)", []).unwrap();
    (dir, db, repo)
}

#[test]
fn submission_creates_one_durable_remote_root_with_pinned_source() {
    let (_dir, db, repo) = fixture();
    let result = remote_submit::submit(
        &db,
        &json!({"on":"apollo","objective":"remote work","repo":repo,"template":"simulated"}),
    )
    .unwrap();
    let id = result["id"].as_str().unwrap();
    assert_eq!(db.task(id).unwrap()["status"], "remote");
    let tree = horde::delegation::tree(&db, id).unwrap();
    assert!(tree["parent"].is_null());
    assert_eq!(tree["root"], id);
    assert_eq!(db.rows("SELECT id FROM tasks", &[]).unwrap().len(), 1);
    assert_eq!(
        db.rows("SELECT state,peer FROM remote_links WHERE task=?", &[&id])
            .unwrap()[0]["peer"],
        "apollo"
    );
    assert_eq!(
        db.rows(
            "SELECT hash FROM artifact_links WHERE task=? AND name='caller-snapshot'",
            &[&id]
        )
        .unwrap()
        .len(),
        1
    );
    assert!(horde::runtime::ready(&db, id).unwrap().is_empty());
}

#[test]
fn dirty_source_and_unknown_runtime_create_no_task() {
    let (_dir, db, repo) = fixture();
    let args = json!({"on":"missing","objective":"remote work","repo":repo,"template":"simulated"});
    assert!(remote_submit::submit(&db, &args).is_err());
    std::fs::write(repo.join("uncommitted"), "do not lose this").unwrap();
    assert!(
        remote_submit::submit(
            &db,
            &json!({"on":"apollo","objective":"remote work","repo":repo,"template":"simulated"})
        )
        .is_err()
    );
    assert!(db.rows("SELECT id FROM tasks", &[]).unwrap().is_empty());
}

#[test]
fn root_result_records_execution_and_snapshot_without_claiming_local_merge() {
    let (_dir, db, repo) = fixture();
    let result = remote_submit::submit(
        &db,
        &json!({"on":"apollo","objective":"remote work","repo":repo,"template":"simulated"}),
    )
    .unwrap();
    let id = result["id"].as_str().unwrap();
    let remote_repo = db.root.join("result-source");
    horde::git::run(
        &repo,
        &[
            "clone",
            "--no-hardlinks",
            repo.to_str().unwrap(),
            remote_repo.to_str().unwrap(),
        ],
    )
    .unwrap();
    std::fs::write(remote_repo.join("result.txt"), "remote result\n").unwrap();
    horde::git::run(&remote_repo, &["add", "."]).unwrap();
    horde::git::run(
        &remote_repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@localhost",
            "commit",
            "-m",
            "result",
        ],
    )
    .unwrap();
    let snapshot = horde::federation::snapshot(&remote_repo).unwrap();
    let reply = json!({"task":{"status":"succeeded"},"snapshot":snapshot,"steps":[],"metrics":{},"outputs":{"summary":"finished remotely"}});
    remote_submit::complete(&db, id, &reply, &json!("base")).unwrap();
    assert_eq!(db.task(id).unwrap()["status"], "succeeded");
    assert!(
        db.rows("SELECT task FROM integrations WHERE task=?", &[&id])
            .unwrap()
            .is_empty()
    );
    let results = db
        .rows(
            "SELECT data FROM external_ops WHERE task=? AND name='federation.result'",
            &[&id],
        )
        .unwrap();
    let evidence: serde_json::Value =
        serde_json::from_str(results[0]["data"].as_str().unwrap()).unwrap();
    assert_eq!(evidence["local_changes_applied"], false);
    assert!(evidence["snapshot_artifact"].is_string());
    let review = remote_submit::result(&db, id).unwrap();
    let review_path = std::path::Path::new(review["result_workspace"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(review_path.join("result.txt")).unwrap(),
        "remote result\n"
    );
    assert!(!repo.join("result.txt").exists());
    assert_eq!(remote_submit::result(&db, id).unwrap(), review);
    std::fs::write(review_path.join("result.txt"), "keep my review edits").unwrap();
    assert!(
        remote_submit::result(&db, id)
            .unwrap_err()
            .to_string()
            .contains("edits")
    );
    assert_eq!(
        std::fs::read_to_string(review_path.join("result.txt")).unwrap(),
        "keep my review edits"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn remote_root_runs_over_tls_and_returns_results_without_a_parent() {
    remote_root_flow(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn selected_remote_model_executes_once_with_local_provider_settings() {
    remote_root_flow(true).await;
}

async fn remote_root_flow(scoped: bool) {
    use horde::network::{DirectPeer, Provider};
    use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let (dir, db, repo) = fixture();
    std::fs::write(repo.join(".horde.toml"), "knowledge_topics = ['bench']\n").unwrap();
    horde::git::run(&repo, &["add", "."]).unwrap();
    horde::git::run(&repo, &["commit", "-m", "pin notebook topics"]).unwrap();
    let remote = dir.path().join("remote");
    Store::open(&remote).unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let key = KeyPair::generate().unwrap();
    let ca = params.self_signed(&key).unwrap();
    let issuer = Issuer::from_ca_cert_pem(&ca.pem(), key).unwrap();
    let mut configurations = vec![];
    for name in ["controller", "apollo"] {
        let key = KeyPair::generate().unwrap();
        let cert = CertificateParams::new(vec![format!("{name}.test")])
            .unwrap()
            .signed_by(&key, &issuer)
            .unwrap();
        let config = NetworkConfig {
            provider: Provider::Direct,
            runtime_id: name.into(),
            ca_cert: dir.path().join(format!("{name}.ca")),
            identity_cert: dir.path().join(format!("{name}.cert")),
            identity_key: dir.path().join(format!("{name}.key")),
            timeout_seconds: 2,
            ..Default::default()
        };
        std::fs::write(&config.ca_cert, ca.pem()).unwrap();
        std::fs::write(&config.identity_cert, cert.pem()).unwrap();
        horde::secrets::write_private(&config.identity_key, key.serialize_pem().as_bytes())
            .unwrap();
        configurations.push((config, horde::store::hash(cert.der())));
    }
    let (mut controller, controller_fingerprint) = configurations.remove(0);
    let (mut worker, worker_fingerprint) = configurations.remove(0);
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    worker.port = socket.local_addr().unwrap().port();
    drop(socket);
    worker.execution_clients.push("controller".into());
    worker
        .allowed_clients
        .insert(controller_fingerprint, "controller".into());
    controller
        .allowed_clients
        .insert(worker_fingerprint, "apollo".into());
    controller.delegate_peers.push("apollo".into());
    controller.peers.insert(
        "apollo".into(),
        DirectPeer {
            address: format!("127.0.0.1:{}", worker.port).parse().unwrap(),
            tls_name: "apollo.test".into(),
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    controller.port = listener.local_addr().unwrap().port();
    worker.peers.insert(
        "controller".into(),
        DirectPeer {
            address: listener.local_addr().unwrap(),
            tls_name: "controller.test".into(),
        },
    );
    horde::federation::configure(&db.root, &controller).unwrap();
    let controller_root = db.root.clone();
    let listener_task = tokio::spawn(async move {
        horde::network::serve_runtime(
            &controller,
            listener,
            std::future::pending(),
            controller_root,
        )
        .await
        .unwrap();
    });
    std::fs::write(
        remote.join("managed-network.toml"),
        toml::to_string(&worker).unwrap(),
    )
    .unwrap();
    let user = dir.path().join("user");
    std::fs::create_dir_all(user.join("horde")).unwrap();
    if scoped {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.path().join("mock-harness");
        let invocation_log = dir.path().join("invoked-models");
        std::fs::write(&program, format!("#!/bin/sh\nmodel=\nwhile [ $# -gt 0 ]; do\n if [ \"$1\" = --model ]; then shift; model=$1; fi\n shift\ndone\ncat > '{}' || exit 1\nprintf '%s\\n' \"$model\" >> '{}'\nprintf '%s\\n' \"$model\" > chosen-model.txt\ngit -c core.hooksPath=/dev/null add chosen-model.txt || exit 1\ngit -c core.hooksPath=/dev/null -c user.name=Test -c user.email=test@localhost commit -q -m result || exit 1\nprintf '%s\\n' '{{\"result\":\"{{\\\"accepted\\\":true,\\\"result\\\":\\\"mock worker completed\\\"}}\",\"usage\":{{}}}}'\n", dir.path().join("received-prompt").display(), invocation_log.display())).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(user.join("horde/config.toml"), format!("autonomy = true\n[providers.claude]\nprogram = {:?}\nmodel = 'opus-test'\n[executors.codex]\nprovider = 'claude'\nmodel = 'astra-test'\n[executors.glm]\nprovider = 'claude'\nmodel = 'glm-5.3'\n", program)).unwrap();
        let templates = repo.join(".horde/templates");
        std::fs::create_dir_all(&templates).unwrap();
        // Exceed common pipe buffers so the fake provider must consume stdin,
        // just as a real harness does, before returning its result.
        let instructions = format!(
            "Perform the supplied task: {}PROMPT-END",
            "bounded context ".repeat(8192)
        );
        std::fs::write(templates.join("selected.toml"), format!("name = 'selected'\nversion = '1'\ninputs = ['task']\n[[steps]]\nid = 'work'\nkind = 'agent'\nrole = 'worker'\ninstructions = {instructions:?}\nscope = ['.']\n")).unwrap();
        horde::git::run(&repo, &["add", "."]).unwrap();
        horde::git::run(&repo, &["commit", "-m", "selected worker fixture"]).unwrap();
    } else {
        std::fs::write(user.join("horde/config.toml"), "autonomy = true\n").unwrap();
    }
    let child = Command::new(env!("CARGO_BIN_EXE_horde"))
        .arg("--data-dir")
        .arg(&remote)
        .arg("daemon")
        .env("XDG_CONFIG_HOME", &user)
        .env_remove("HORDE_ENROLLMENT_FILE")
        .env_remove("HORDE_ENROLLMENT_JSON")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    struct Daemon(std::process::Child);
    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _daemon = Daemon(child);
    let submission = if scoped {
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::os::unix::net::UnixStream::connect(remote.join("daemon.sock")).is_err() {
            assert!(Instant::now() < deadline, "worker daemon did not start");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let advertised = Command::new(env!("CARGO_BIN_EXE_horde"))
            .arg("--data-dir")
            .arg(&remote)
            .args(["call", "runtime_capabilities", "{}"])
            .env("XDG_CONFIG_HOME", &user)
            .output()
            .unwrap();
        assert!(
            advertised.status.success(),
            "{}",
            String::from_utf8_lossy(&advertised.stderr)
        );
        let inventory: serde_json::Value = serde_json::from_slice(&advertised.stdout).unwrap();
        horde::capabilities::observe(&db, "apollo", &inventory["runtimes"][0]).unwrap();
        json!({"request_id":"remote-model-choice","on":"apollo","objective":"remote work","repo":repo,"template":"selected","execution":{"allowed":[{"runtime":"apollo","capabilities":["claude","codex","glm"]}],"selected":{"runtime":"apollo","capability":"glm"}}})
    } else {
        json!({"on":"apollo","objective":"remote work","repo":repo,"template":"simulated"})
    };
    let result = horde::protocol::dispatch(&db, "submit_task", submission.clone(), None).unwrap();
    if scoped {
        assert_eq!(
            horde::protocol::dispatch(&db, "submit_task", submission.clone(), None).unwrap(),
            result
        );
    }
    let id = result["id"].as_str().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    std::fs::write(
        repo.join("later-local.txt"),
        "created after remote submission",
    )
    .unwrap();
    horde::git::run(&repo, &["add", "."]).unwrap();
    horde::git::run(&repo, &["commit", "-m", "later local work"]).unwrap();
    loop {
        horde::federation::tick(&db).await.unwrap();
        let status = db.task(id).unwrap()["status"].clone();
        if status == "succeeded" {
            break;
        }
        assert!(
            Instant::now() < deadline && !["failed", "blocked", "cancelled"].iter().any(|terminal| status == *terminal),
            "remote task did not complete ({status}): {:?}",
            db.rows("SELECT kind,json_extract(data,'$.remote_status') AS remote_status,json_extract(data,'$.remote_steps[0].result') AS remote_result FROM events WHERE task=? AND kind='task.finished'", &[&id])
                .unwrap()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(db.rows("SELECT id FROM attempts", &[]).unwrap().is_empty());
    assert_eq!(
        db.rows("SELECT state FROM remote_links WHERE task=?", &[&id])
            .unwrap()[0]["state"],
        "done"
    );
    assert_eq!(
        db.rows(
            "SELECT hash FROM artifact_links WHERE task=? AND name='remote-snapshot'",
            &[&id]
        )
        .unwrap()
        .len(),
        1
    );
    assert_eq!(
        horde::git::run(&repo, &["status", "--porcelain"]).unwrap(),
        ""
    );
    let result = remote_submit::result(&db, id).unwrap();
    assert!(
        !std::path::Path::new(result["result_workspace"].as_str().unwrap())
            .join("later-local.txt")
            .exists()
    );
    assert!(repo.join("later-local.txt").exists());
    if scoped {
        assert_eq!(
            std::fs::read_to_string(dir.path().join("invoked-models")).unwrap(),
            "glm-5.3\n"
        );
        let prompt = std::fs::read_to_string(dir.path().join("received-prompt")).unwrap();
        assert!(prompt.len() > 128 * 1024, "large prompt was not delivered");
        assert!(
            prompt.contains("PROMPT-END"),
            "prompt delivery was truncated"
        );
        let review = std::path::Path::new(result["result_workspace"].as_str().unwrap());
        assert_eq!(
            std::fs::read_to_string(review.join("chosen-model.txt")).unwrap(),
            "glm-5.3\n"
        );
        assert!(!repo.join("chosen-model.txt").exists());
        let worker_db = Store::open(&remote).unwrap();
        assert_eq!(
            worker_db
                .rows("SELECT id FROM attempts", &[])
                .unwrap()
                .len(),
            1
        );
        let selected = worker_db
            .rows("SELECT policy FROM task_execution_policy", &[])
            .unwrap();
        let policy: serde_json::Value =
            serde_json::from_str(selected[0]["policy"].as_str().unwrap()).unwrap();
        assert_eq!(policy["selected"]["capability"], "glm");
        assert_eq!(policy["selected"]["model"], "glm-5.3");
        assert_eq!(
            horde::protocol::dispatch(&db, "submit_task", submission, None).unwrap()["id"],
            id
        );
    }
    // A remotely submitted root uses the same notebook authority as remote
    // descendants, even though its controller-side task has no parent.
    let shared = horde::protocol::dispatch(&db, "add_knowledge", json!({"task":id,"scope":"family","topic":"bench","kind":"evidence","content":"controller measurement","provenance":{}}), None).unwrap();
    let unrelated = horde::protocol::dispatch(
        &db,
        "submit_task",
        json!({"repo":repo,"objective":"unrelated task","template":"simulated"}),
        None,
    )
    .unwrap();
    horde::protocol::dispatch(&db, "add_knowledge", json!({"task":unrelated["id"],"scope":"family","topic":"bench","kind":"fact","content":"unrelated measurement","provenance":{}}), None).unwrap();
    let context_version = horde::delegation::tree(&db, id).unwrap()["version"].clone();
    let remote_root = remote.clone();
    let owner = id.to_owned();
    let shared_id = shared["id"].clone();
    let (token, remote_id, claim) = tokio::task::spawn_blocking(move || {
        let remote_db = Store::open(&remote_root).unwrap();
        let remote_id = remote_db.rows("SELECT task FROM remote_origins WHERE owner_task=?", &[&owner]).unwrap()[0]["task"].as_str().unwrap().to_owned();
        let step = remote_db.steps(&remote_id).unwrap()[0]["id"].as_str().unwrap().to_owned();
        let worker = remote_db.register(&remote_id, Some(&step)).unwrap();
        let token = worker["token"].as_str().unwrap().to_owned();
        let call = |name, args| horde::protocol::dispatch(&remote_db, name, args, Some(&token)).unwrap();
        let options = call("knowledge_options", json!({}));
        assert_eq!(options["schemas"]["add_knowledge"]["properties"]["topic"]["enum"], json!(["bench"]));
        let page = call("knowledge", json!({"scope":"family","topic":"bench","query":"measurement"}));
        assert_eq!(page["root"], owner);
        assert_eq!(page["records"].as_array().unwrap().len(), 1);
        assert_eq!(page["records"][0]["id"], shared_id);
        let args = json!({"id":"remote-root-claim","scope":"family","topic":"bench","kind":"evidence","content":"remote root measurement","provenance":{"run":"fixture"},"verified":true});
        let claim = call("add_knowledge", args.clone());
        assert_eq!(call("add_knowledge", args)["id"], claim["id"]);
        call("link_knowledge", json!({"source":claim["id"],"target":shared_id,"relation":"supports"}));
        let edges = call("knowledge_edges", json!({"source":claim["id"]}));
        assert!(edges.to_string().contains(shared_id.as_str().unwrap()));
        (token, remote_id, claim)
    }).await.unwrap();
    let published = horde::protocol::dispatch(
        &db,
        "knowledge",
        json!({"task":id,"scope":"family","query":"remote"}),
        None,
    )
    .unwrap();
    assert_eq!(published["records"].as_array().unwrap().len(), 1);
    let record = &published["records"][0];
    assert_eq!(record["id"], claim["id"]);
    assert_eq!(record["task"], id);
    assert_eq!(record["origin"]["remote_task"], remote_id);
    assert_eq!(record["verified"], 0);
    tokio::task::spawn_blocking(move || {
        let remote_db = Store::open(&remote).unwrap();
        horde::protocol::dispatch(
            &remote_db,
            "retract_knowledge",
            json!({"id":claim["id"],"reason":"withdrawn","provenance":{}}),
            Some(&token),
        )
        .unwrap();
    })
    .await
    .unwrap();
    assert_eq!(
        horde::protocol::dispatch(
            &db,
            "knowledge",
            json!({"task":id,"scope":"family","query":"remote"}),
            None
        )
        .unwrap()["records"],
        json!([])
    );
    assert_eq!(
        horde::delegation::tree(&db, id).unwrap()["version"],
        context_version
    );
    assert_eq!(db.task(id).unwrap()["status"], "succeeded");
    listener_task.abort();
}

#[test]
fn cancellation_committed_during_status_request_is_not_overwritten() {
    let (_dir, db, repo) = fixture();
    let result = remote_submit::submit(
        &db,
        &json!({"on":"apollo","objective":"remote work","repo":repo,"template":"simulated"}),
    )
    .unwrap();
    let id = result["id"].as_str().unwrap();
    horde::protocol::dispatch(&db, "cancel", json!({"task":id}), None).unwrap();
    let snapshot = horde::federation::snapshot(&repo).unwrap();
    remote_submit::complete(
        &db,
        id,
        &json!({"task":{"status":"succeeded"},"snapshot":snapshot,"steps":[],"metrics":{}}),
        &json!("base"),
    )
    .unwrap();
    assert_eq!(db.task(id).unwrap()["status"], "cancelled");
    assert!(
        db.steps(id)
            .unwrap()
            .iter()
            .all(|step| step["state"] == "cancelled")
    );
    assert!(remote_submit::result(&db, id).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn required_submission_confirmation_holds_remote_dispatch() {
    let (_dir, db, repo) = fixture();
    std::fs::write(repo.join(".horde.toml"), "autonomy = false\n").unwrap();
    horde::git::run(&repo, &["add", "."]).unwrap();
    horde::git::run(&repo, &["commit", "-m", "require confirmation"]).unwrap();
    let result = remote_submit::submit(
        &db,
        &json!({"on":"apollo","objective":"remote work","repo":repo,"template":"simulated"}),
    )
    .unwrap();
    let id = result["id"].as_str().unwrap();
    horde::federation::tick(&db).await.unwrap();
    assert_eq!(db.task(id).unwrap()["status"], "waiting");
    assert_eq!(
        db.rows("SELECT state FROM remote_links WHERE task=?", &[&id])
            .unwrap()[0]["state"],
        "pending"
    );
    assert!(
        db.rows(
            "SELECT hash FROM artifact_links WHERE task=? AND name='federation-input'",
            &[&id]
        )
        .unwrap()
        .is_empty()
    );
    assert!(horde::runtime::ready(&db, id).unwrap().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn tick_preserves_cancel_received_with_stale_success_and_reconciles_it_next_time() {
    use horde::federation::wire;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tonic::{
        Request, Response, Status,
        transport::{Identity, Server, ServerTlsConfig},
    };
    struct Mock {
        root: std::path::PathBuf,
        task: String,
        reply: serde_json::Value,
        cancellations: Arc<AtomicUsize>,
    }
    #[tonic::async_trait]
    impl wire::federation_server::Federation for Mock {
        async fn call(
            &self,
            request: Request<wire::CallRequest>,
        ) -> Result<Response<wire::CallReply>, Status> {
            if request.get_ref().method == "status" {
                let db = Store::open(&self.root).unwrap();
                horde::protocol::dispatch(&db, "cancel", json!({"task":self.task}), None).unwrap();
                Ok(Response::new(wire::CallReply {
                    json: self.reply.to_string(),
                }))
            } else {
                assert_eq!(request.get_ref().method, "cancel");
                self.cancellations.fetch_add(1, Ordering::SeqCst);
                Ok(Response::new(wire::CallReply { json: "{}".into() }))
            }
        }
        type ControlStream =
            tokio_stream::wrappers::ReceiverStream<Result<wire::CallReply, Status>>;
        async fn control(
            &self,
            _: Request<tonic::Streaming<wire::CallRequest>>,
        ) -> Result<Response<Self::ControlStream>, Status> {
            Err(Status::unimplemented("not used"))
        }
    }
    let (dir, db, repo) = fixture();
    let result = remote_submit::submit(
        &db,
        &json!({"on":"apollo","objective":"remote work","repo":repo,"template":"simulated"}),
    )
    .unwrap();
    let id = result["id"].as_str().unwrap();
    db.conn
        .execute(
            "UPDATE remote_links SET state='running',remote_id='remote-task' WHERE task=?",
            [id],
        )
        .unwrap();
    let snapshot = horde::federation::snapshot(&repo).unwrap();
    let cancellations = Arc::new(AtomicUsize::new(0));
    let service = Mock {
        root: db.root.clone(),
        task: id.into(),
        reply: json!({"task":{"status":"succeeded"},"snapshot":snapshot,"context_version":999,"cleanup_complete":true,"steps":[],"metrics":{}}),
        cancellations: cancellations.clone(),
    };
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(vec!["apollo.test".into()]).unwrap();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca = params.self_signed(&key).unwrap();
    let issuer = rcgen::Issuer::from_ca_cert_pem(&ca.pem(), key).unwrap();
    let server_key = rcgen::KeyPair::generate().unwrap();
    let server_cert = rcgen::CertificateParams::new(vec!["apollo.test".into()])
        .unwrap()
        .signed_by(&server_key, &issuer)
        .unwrap();
    let client_key = rcgen::KeyPair::generate().unwrap();
    let client_cert = rcgen::CertificateParams::new(vec!["controller.test".into()])
        .unwrap()
        .signed_by(&client_key, &issuer)
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = NetworkConfig {
        provider: horde::network::Provider::Direct,
        peers: std::collections::BTreeMap::from([(
            "apollo".into(),
            horde::network::DirectPeer {
                address: listener.local_addr().unwrap(),
                tls_name: "apollo.test".into(),
            },
        )]),
        ca_cert: dir.path().join("ca.pem"),
        identity_cert: dir.path().join("client.pem"),
        identity_key: dir.path().join("client.key"),
        ..Default::default()
    };
    std::fs::write(&config.ca_cert, ca.pem()).unwrap();
    std::fs::write(&config.identity_cert, client_cert.pem()).unwrap();
    horde::secrets::write_private(&config.identity_key, client_key.serialize_pem().as_bytes())
        .unwrap();
    horde::federation::configure(&db.root, &config).unwrap();
    let server = tokio::spawn(async move {
        Server::builder()
            .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(
                server_cert.pem(),
                server_key.serialize_pem(),
            )))
            .unwrap()
            .add_service(wire::federation_server::FederationServer::new(service))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    horde::federation::tick(&db).await.unwrap();
    assert_eq!(db.task(id).unwrap()["status"], "cancelled");
    assert_eq!(
        db.rows("SELECT state FROM remote_links WHERE task=?", &[&id])
            .unwrap()[0]["state"],
        "running"
    );
    horde::federation::tick(&db).await.unwrap();
    assert_eq!(cancellations.load(Ordering::SeqCst), 1);
    assert_eq!(db.task(id).unwrap()["status"], "cancelled");
    assert_eq!(
        db.rows("SELECT state FROM remote_links WHERE task=?", &[&id])
            .unwrap()[0]["state"],
        "done"
    );
    server.abort();
}

#[test]
fn snapshot_rejects_case_aliases_of_git_metadata_before_running_hooks() {
    for entry in [
        ".GiT/hooks/pre-commit",
        "nested/.GIT/config",
        ".git/hooks/pre-commit",
        ".g\u{200c}it/config",
        ".\u{feff}git/hooks/pre-commit",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("result");
        let body = b"#!/bin/sh\ntouch HOOK_RAN\n";
        let mut header = tar::Header::new_gnu();
        header.set_path(entry).unwrap();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        let mut archive = tar::Builder::new(Vec::new());
        archive.append(&header, body.as_slice()).unwrap();
        let bytes = archive.into_inner().unwrap();
        let snapshot = json!({"archive":hex::encode(&bytes),"hash":horde::store::hash(&bytes)});
        let failure = horde::federation::unpack(&snapshot, &destination).unwrap_err();
        assert!(failure.to_string().contains("Git metadata"));
        assert!(!destination.join("HOOK_RAN").exists());
    }
}

#[test]
fn snapshot_import_ignores_host_git_hooks_templates_and_filters() {
    if let Some(root) = std::env::var_os("HORDE_TEST_IMPORT_ROOT") {
        let root = std::path::PathBuf::from(root);
        let db = Store::open(&root.join("data")).unwrap();
        let id = std::fs::read_to_string(root.join("task")).unwrap();
        let result = remote_submit::result(&db, &id).unwrap();
        assert!(!root.join("hook-ran").exists());
        let workspace = std::path::Path::new(result["result_workspace"].as_str().unwrap());
        assert_eq!(
            std::fs::read_to_string(workspace.join("payload.txt")).unwrap(),
            "ordinary source\n"
        );
        assert_eq!(remote_submit::result(&db, &id).unwrap(), result);
        assert!(!root.join("hook-ran").exists());
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let hook = root.join("hook");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\ntouch '{}'\ncat\n",
            root.join("hook-ran").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    let templates = root.join("templates");
    std::fs::create_dir_all(templates.join("hooks")).unwrap();
    std::fs::copy(&hook, templates.join("hooks/pre-commit")).unwrap();
    std::fs::write(root.join(".gitconfig"), format!("[init]\n templateDir = {:?}\n[core]\n hooksPath = {:?}\n fsmonitor = {:?}\n[filter \"attack\"]\n clean = {:?}\n required = true\n", templates, templates.join("hooks"), hook, hook)).unwrap();
    let mut archive = tar::Builder::new(Vec::new());
    for (name, body) in [
        (".gitattributes", "payload.txt filter=attack\n"),
        ("payload.txt", "ordinary source\n"),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_path(name).unwrap();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive.append(&header, body.as_bytes()).unwrap();
    }
    let bytes = archive.into_inner().unwrap();
    std::fs::write(
        root.join("snapshot.json"),
        json!({"archive":hex::encode(&bytes),"hash":horde::store::hash(&bytes)}).to_string(),
    )
    .unwrap();
    let db = Store::open(&root.join("data")).unwrap();
    let id = db
        .submit(
            "review remote source",
            root,
            &horde::config::Settings::default(),
            &horde::template::Plan {
                warnings: vec![],
                steps: vec![],
                pins: Default::default(),
                outputs: Default::default(),
            },
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO remote_links(task,peer,state,request) VALUES(?,'apollo','done',?)",
            [&id, &id],
        )
        .unwrap();
    remote_submit::complete(&db, &id, &json!({"task":{"status":"succeeded"},"snapshot":serde_json::from_slice::<serde_json::Value>(&std::fs::read(root.join("snapshot.json")).unwrap()).unwrap(),"steps":[],"metrics":{}}), &json!(null)).unwrap();
    std::fs::write(root.join("task"), &id).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "snapshot_import_ignores_host_git_hooks_templates_and_filters",
            "--nocapture",
        ])
        .env("HOME", root)
        .env("HORDE_TEST_IMPORT_ROOT", root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!root.join("hook-ran").exists());
}
