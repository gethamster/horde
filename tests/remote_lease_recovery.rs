use horde::{accounts, federation, network::NetworkConfig, project_runtime, store::Store};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tonic::{
    Request, Response, Status,
    transport::{Identity, Server, ServerTlsConfig},
};

struct Controller {
    root: std::path::PathBuf,
    account: String,
    lose_method: &'static str,
    lose_before_commit: bool,
    lost: AtomicBool,
    disconnected: Arc<AtomicBool>,
    acquires: Arc<AtomicUsize>,
}

fn rotate(db: &Store, account: &str, secret: &str) {
    accounts::set_credential(
        db,
        "default",
        account,
        &accounts::Credential {
            kind: "api_key".into(),
            secret: secret.into(),
            expires_at: None,
            metadata: json!({}),
        },
    )
    .unwrap();
}

#[tonic::async_trait]
impl federation::wire::federation_server::Federation for Controller {
    async fn call(
        &self,
        request: Request<federation::wire::CallRequest>,
    ) -> Result<Response<federation::wire::CallReply>, Status> {
        let request = request.into_inner();
        if self.lost.load(Ordering::SeqCst) && self.disconnected.load(Ordering::SeqCst) {
            return Err(Status::unavailable("controller disconnected"));
        }
        if request.method.ends_with("_acquire") {
            self.acquires.fetch_add(1, Ordering::SeqCst);
        }
        if self.lose_before_commit
            && request.method == self.lose_method
            && !self.lost.swap(true, Ordering::SeqCst)
        {
            rotate(
                &Store::open(&self.root).unwrap(),
                &self.account,
                "second-key",
            );
            return Err(Status::unavailable("request lost before commit"));
        }
        let args: Value = serde_json::from_str(&request.json).unwrap();
        let response = federation::handle_control(
            self.root.clone(),
            &self.root.join("config"),
            NetworkConfig::default(),
            "worker",
            &json!({"method":request.method,"args":args}),
        )
        .unwrap_or_else(|error| json!({"error":error.to_string()}));
        if request.method == self.lose_method && !self.lost.swap(true, Ordering::SeqCst) {
            assert!(
                response.get("error").is_none(),
                "controller operation failed before simulated reply loss"
            );
            rotate(
                &Store::open(&self.root).unwrap(),
                &self.account,
                "second-key",
            );
            return Err(Status::unavailable("reply lost after commit"));
        }
        Ok(Response::new(federation::wire::CallReply {
            json: response.to_string(),
        }))
    }

    type ControlStream =
        tokio_stream::wrappers::ReceiverStream<Result<federation::wire::CallReply, Status>>;

    async fn control(
        &self,
        _: Request<tonic::Streaming<federation::wire::CallRequest>>,
    ) -> Result<Response<Self::ControlStream>, Status> {
        Err(Status::unimplemented("not used"))
    }
}

fn task(db: &Store, id: &str) {
    let settings = serde_json::to_string(&horde::config::Settings::default()).unwrap();
    db.conn
        .execute(
            "INSERT INTO tasks VALUES(?,'work','repo','running',?,'{}',0)",
            [id, &settings],
        )
        .unwrap();
    db.conn
        .execute("INSERT INTO task_projects VALUES(?,'default',NULL)", [id])
        .unwrap();
}

async fn scenario(
    lose_method: &'static str,
    initially_disconnected: bool,
    lose_before_commit: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let controller = Store::open(&directory.path().join("controller")).unwrap();
    let worker_root = directory.path().join("worker");
    let worker = Store::open(&worker_root).unwrap();
    task(&controller, "owner");
    task(&worker, "remote");
    controller.conn.execute("INSERT INTO remote_links(task,peer,remote_id,state,request,base) VALUES('owner','worker','remote','running','assignment',NULL)",[]).unwrap();
    worker
        .conn
        .execute(
            "INSERT INTO remote_origins VALUES('remote','controller','owner')",
            [],
        )
        .unwrap();
    worker.conn.execute("INSERT INTO steps(id,task,name,spec,state) VALUES('step','remote','work','{}','pending')",[]).unwrap();
    controller
        .conn
        .execute("UPDATE projects SET concurrency=1 WHERE id='default'", [])
        .unwrap();
    let account = accounts::dispatch(&controller, "account_create", &json!({"project":"default","name":"shared","provider":"tuara","auth_mode":"api","base_url":"https://tuara.com/router/v1","concurrency":1})).unwrap().unwrap()["id"].as_str().unwrap().to_owned();
    rotate(&controller, &account, "first-key");

    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(vec!["controller.test".into()]).unwrap();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca = params.self_signed(&key).unwrap();
    let issuer = rcgen::Issuer::from_ca_cert_pem(&ca.pem(), key).unwrap();
    let server_key = rcgen::KeyPair::generate().unwrap();
    let server_cert = rcgen::CertificateParams::new(vec!["controller.test".into()])
        .unwrap()
        .signed_by(&server_key, &issuer)
        .unwrap();
    let client_key = rcgen::KeyPair::generate().unwrap();
    let client_cert = rcgen::CertificateParams::new(vec!["worker.test".into()])
        .unwrap()
        .signed_by(&client_key, &issuer)
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = NetworkConfig {
        provider: horde::network::Provider::Direct,
        runtime_id: "worker".into(),
        peers: std::collections::BTreeMap::from([(
            "controller".into(),
            horde::network::DirectPeer {
                address: listener.local_addr().unwrap(),
                tls_name: "controller.test".into(),
            },
        )]),
        ca_cert: directory.path().join("ca.pem"),
        identity_cert: directory.path().join("worker.pem"),
        identity_key: directory.path().join("worker.key"),
        ..Default::default()
    };
    std::fs::write(&config.ca_cert, ca.pem()).unwrap();
    std::fs::write(&config.identity_cert, client_cert.pem()).unwrap();
    horde::secrets::write_private(&config.identity_key, client_key.serialize_pem().as_bytes())
        .unwrap();
    federation::configure(&worker.root, &config).unwrap();
    let disconnected = Arc::new(AtomicBool::new(initially_disconnected));
    let acquires = Arc::new(AtomicUsize::new(0));
    let service = Controller {
        root: controller.root.clone(),
        account: account.clone(),
        lose_method,
        lose_before_commit,
        lost: AtomicBool::new(false),
        disconnected: disconnected.clone(),
        acquires: acquires.clone(),
    };
    let server = tokio::spawn(async move {
        Server::builder()
            .tls_config(ServerTlsConfig::new().identity(Identity::from_pem(
                server_cert.pem(),
                server_key.serialize_pem(),
            )))
            .unwrap()
            .add_service(federation::wire::federation_server::FederationServer::new(
                service,
            ))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    let mut row =
        json!({"id":"step","task":"remote","dispatch_role":"worker","managed_remote_account":true});
    assert!(
        !project_runtime::acquire_remote(&worker, &mut row)
            .await
            .unwrap()
    );
    let previous = worker
        .rows("SELECT request_id,state FROM remote_account_leases", &[])
        .unwrap()[0]
        .clone();
    assert_eq!(
        previous["state"],
        if initially_disconnected {
            "release_pending"
        } else {
            "released"
        }
    );
    assert!(
        worker
            .rows("SELECT id FROM attempts", &[])
            .unwrap()
            .is_empty()
    );
    drop(worker);
    // Restart must retain the pending release and must not reserve more capacity.
    let worker = Store::open(&worker_root).unwrap();
    if initially_disconnected {
        let before = acquires.load(Ordering::SeqCst);
        assert!(
            !project_runtime::acquire_remote(&worker, &mut row)
                .await
                .unwrap()
        );
        assert_eq!(acquires.load(Ordering::SeqCst), before);
        assert_eq!(
            project_runtime::active(&controller, "default").unwrap(),
            i64::from(!lose_before_commit)
        );
        disconnected.store(false, Ordering::SeqCst);
        project_runtime::reconcile_releases(&worker).await.unwrap();
    }
    assert_eq!(project_runtime::active(&controller, "default").unwrap(), 0);
    assert!(
        controller
            .rows(
                "SELECT request_id FROM account_remote_reservations WHERE state!='released'",
                &[]
            )
            .unwrap()
            .is_empty()
    );
    assert!(
        project_runtime::acquire_remote(&worker, &mut row)
            .await
            .unwrap()
    );
    let current = worker
        .rows("SELECT request_id,state FROM remote_account_leases", &[])
        .unwrap()[0]
        .clone();
    assert_ne!(previous["request_id"], current["request_id"]);
    assert_eq!(current["state"], "active");
    assert_eq!(row["account_binding"]["credential_version"], 2);
    assert_eq!(
        accounts::credential(&worker, "default", &account)
            .unwrap()
            .secret,
        "second-key"
    );
    assert_eq!(project_runtime::active(&controller, "default").unwrap(), 1);
    // Cleanup must stay conservative while process termination is uncertain.
    worker
        .conn
        .execute(
            "INSERT INTO attempts(id,step,state,started) VALUES('attempt','step','uncertain',0)",
            [],
        )
        .unwrap();
    project_runtime::release_remote(&worker, "step")
        .await
        .unwrap();
    assert_eq!(project_runtime::active(&controller, "default").unwrap(), 1);
    worker
        .conn
        .execute("UPDATE attempts SET state='failed' WHERE id='attempt'", [])
        .unwrap();
    project_runtime::release_remote(&worker, "step")
        .await
        .unwrap();
    assert_eq!(project_runtime::active(&controller, "default").unwrap(), 0);
    server.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn lost_account_reply_and_rotation_release_capacity_before_retry() {
    scenario("account_acquire", false, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn lost_account_reply_survives_disconnect_and_restart() {
    scenario("account_acquire", true, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn lost_project_reply_survives_disconnect_and_restart() {
    scenario("project_acquire", true, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn lost_project_request_before_commit_does_not_wedge_cleanup() {
    scenario("project_acquire", true, true).await;
}
