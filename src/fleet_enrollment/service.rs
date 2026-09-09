//! A separate TLS endpoint admits workers; it exposes no execution or administration RPCs.
use super::{MAX_PACKET, ServerConfig, authority};
use crate::{federation::wire, network::NetworkConfig, store::Store};
use anyhow::{Result, ensure};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tonic::{
    Request, Response, Status,
    transport::{Certificate, Identity, Server, ServerTlsConfig},
};

#[derive(Clone)]
struct Service {
    root: PathBuf,
    network: NetworkConfig,
    server: ServerConfig,
    budget: Arc<Mutex<(Instant, usize)>>,
    slots: Arc<tokio::sync::Semaphore>,
}

impl Service {
    fn admit(&self) -> Result<tokio::sync::OwnedSemaphorePermit, Status> {
        let mut budget = self
            .budget
            .lock()
            .map_err(|_| Status::unavailable("enrollment unavailable"))?;
        if budget.0.elapsed() >= Duration::from_secs(60) {
            *budget = (Instant::now(), 0);
        }
        if budget.1 >= 120 {
            return Err(Status::resource_exhausted(
                "enrollment rate limit; retry later",
            ));
        }
        budget.1 += 1;
        self.slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Status::resource_exhausted("enrollment busy; retry later"))
    }
}

fn fingerprint<T>(request: &Request<T>) -> Result<String, Status> {
    let connection = request
        .extensions()
        .get::<tonic::transport::server::TlsConnectInfo<tonic::transport::server::TcpConnectInfo>>()
        .ok_or_else(|| Status::unauthenticated("worker certificate required"))?;
    let certs = connection
        .peer_certs()
        .ok_or_else(|| Status::unauthenticated("worker certificate required"))?;
    let cert = certs
        .first()
        .ok_or_else(|| Status::unauthenticated("worker certificate required"))?;
    Ok(crate::store::hash(cert.as_ref()))
}

#[tonic::async_trait]
impl wire::enrollment_server::Enrollment for Service {
    async fn register(
        &self,
        request: Request<wire::EnrollmentRequest>,
    ) -> Result<Response<wire::EnrollmentReply>, Status> {
        let permit = self.admit()?;
        let input = request.into_inner();
        if input.key_id.len() > 128 || input.token.len() > 256 || input.csr_pem.len() > 16 * 1024 {
            return Err(Status::invalid_argument("enrollment request too large"));
        }
        let service = self.clone();
        let certificate = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let db = Store::open(&service.root)?;
            authority::register(
                &db,
                &service.network,
                &service.server,
                &input.key_id,
                &input.token,
                &input.csr_pem,
            )
        })
        .await
        .map_err(|_| Status::internal("enrollment unavailable"))?
        .map_err(|_| {
            Status::permission_denied(
                "enrollment rejected; check the fleet credential, expiry, and worker limit",
            )
        })?;
        Ok(Response::new(wire::EnrollmentReply {
            json: serde_json::to_string(&certificate)
                .map_err(|_| Status::internal("enrollment unavailable"))?,
        }))
    }

    async fn renew(
        &self,
        request: Request<wire::RenewalRequest>,
    ) -> Result<Response<wire::EnrollmentReply>, Status> {
        let permit = self.admit()?;
        let fingerprint = fingerprint(&request)?;
        let input = request.into_inner();
        if input.csr_pem.len() > 16 * 1024 {
            return Err(Status::invalid_argument("certificate request too large"));
        }
        let service = self.clone();
        let certificate = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let db = Store::open(&service.root)?;
            authority::renew(
                &db,
                &service.network,
                &service.server,
                &fingerprint,
                &input.csr_pem,
            )
        })
        .await
        .map_err(|_| Status::internal("renewal unavailable"))?
        .map_err(|_| Status::permission_denied("certificate renewal rejected"))?;
        Ok(Response::new(wire::EnrollmentReply {
            json: serde_json::to_string(&certificate)
                .map_err(|_| Status::internal("renewal unavailable"))?,
        }))
    }
}

pub async fn serve(
    network: &NetworkConfig,
    server: &ServerConfig,
    listener: tokio::net::TcpListener,
    root: PathBuf,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    network.validate()?;
    authority::validate_signer(network, server)?;
    ensure!(
        network.provider != crate::network::Provider::Disabled && network.controller_peer.is_none(),
        "only a network controller can serve fleet enrollment"
    );
    ensure!(
        listener.local_addr()? == server.listen
            && !server.listen.ip().is_unspecified()
            && !server.listen.ip().is_multicast(),
        "enrollment listener must match its configured address"
    );
    ensure!(
        std::fs::metadata(&network.identity_key)?
            .permissions()
            .mode()
            & 0o077
            == 0,
        "controller identity key must be private"
    );
    let tls = ServerTlsConfig::new()
        .identity(Identity::from_pem(
            std::fs::read(&network.identity_cert)?,
            std::fs::read(&network.identity_key)?,
        ))
        .client_ca_root(Certificate::from_pem(std::fs::read(&network.ca_cert)?))
        .client_auth_optional(true)
        .timeout(Duration::from_secs(10));
    let service = Service {
        root,
        network: network.clone(),
        server: server.clone(),
        budget: Arc::new(Mutex::new((Instant::now(), 0))),
        slots: Arc::new(tokio::sync::Semaphore::new(8)),
    };
    Server::builder()
        .tls_config(tls)?
        .timeout(Duration::from_secs(15))
        .concurrency_limit_per_connection(4)
        .add_service(
            wire::enrollment_server::EnrollmentServer::new(service)
                .max_decoding_message_size(MAX_PACKET)
                .max_encoding_message_size(MAX_PACKET),
        )
        .serve_with_incoming_shutdown(
            tokio_stream::wrappers::TcpListenerStream::new(listener),
            shutdown,
        )
        .await?;
    Ok(())
}

/// New fleet keys can enable this endpoint while the controller is running.
pub async fn supervise(root: PathBuf, network: NetworkConfig) {
    if network.provider == crate::network::Provider::Disabled || network.controller_peer.is_some() {
        return;
    }
    loop {
        let path = root.join("enrollment-server.toml");
        if path.exists() {
            let attempt = async {
                let server: ServerConfig = toml::from_str(&std::fs::read_to_string(&path)?)?;
                let listener = tokio::net::TcpListener::bind(server.listen).await?;
                eprintln!("Horde fleet enrollment listening on {}", server.listen);
                serve(
                    &network,
                    &server,
                    listener,
                    root.clone(),
                    std::future::pending(),
                )
                .await
            }
            .await;
            if let Err(error) = attempt {
                eprintln!("Fleet enrollment listener: {error:#}");
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
