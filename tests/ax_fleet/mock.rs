use horde::fleet::ax::wire::*;
use std::sync::{Arc, Mutex};
use tonic::{Request, Response, Status};

#[derive(Default)]
pub struct State {
    pub task: Option<Task>,
    pub gateway: Option<Gateway>,
    pub workspace: Option<Workspace>,
    pub writes: Vec<&'static str>,
    pub fail_update: bool,
    pub retain_delete: bool,
}
#[derive(Clone, Default)]
pub struct Mock(pub Arc<Mutex<State>>);

#[tonic::async_trait]
impl ax_server::Ax for Mock {
    async fn get_task(&self, _: Request<GetTaskRequest>) -> Result<Response<Task>, Status> {
        self.0
            .lock()
            .unwrap()
            .task
            .clone()
            .map(Response::new)
            .ok_or_else(|| Status::not_found("task"))
    }
    async fn update_task(
        &self,
        request: Request<UpdateTaskRequest>,
    ) -> Result<Response<Task>, Status> {
        let mut state = self.0.lock().unwrap();
        let mut task = request.into_inner().task.unwrap();
        task.status = Some(TaskStatus {
            phase: "Running".into(),
            actor: task.metadata.as_ref().unwrap().name.clone(),
            conditions: vec![Condition {
                r#type: "Ready".into(),
                status: "True".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        state.task = Some(task.clone());
        state.writes.push("task");
        if state.fail_update {
            return Err(Status::unavailable("secret remote error body"));
        }
        Ok(Response::new(task))
    }
    async fn delete_task(
        &self,
        _: Request<DeleteTaskRequest>,
    ) -> Result<Response<DeleteTaskResponse>, Status> {
        let mut state = self.0.lock().unwrap();
        if !state.retain_delete {
            state.task = None;
        }
        state.writes.push("delete-task");
        Ok(Response::new(DeleteTaskResponse {}))
    }
    async fn suspend_task(&self, _: Request<SuspendTaskRequest>) -> Result<Response<Task>, Status> {
        let mut state = self.0.lock().unwrap();
        state.writes.push("suspend");
        let task = state.task.as_mut().unwrap();
        task.spec.as_mut().unwrap().suspend = true;
        task.status.as_mut().unwrap().phase = "Suspended".into();
        Ok(Response::new(task.clone()))
    }
    async fn resume_task(&self, _: Request<ResumeTaskRequest>) -> Result<Response<Task>, Status> {
        let mut state = self.0.lock().unwrap();
        state.writes.push("resume");
        let task = state.task.as_mut().unwrap();
        task.spec.as_mut().unwrap().suspend = false;
        task.status.as_mut().unwrap().phase = "Running".into();
        Ok(Response::new(task.clone()))
    }
    async fn get_gateway(
        &self,
        _: Request<GetGatewayRequest>,
    ) -> Result<Response<Gateway>, Status> {
        self.0
            .lock()
            .unwrap()
            .gateway
            .clone()
            .map(Response::new)
            .ok_or_else(|| Status::not_found("gateway"))
    }
    async fn update_gateway(
        &self,
        request: Request<UpdateGatewayRequest>,
    ) -> Result<Response<Gateway>, Status> {
        let mut state = self.0.lock().unwrap();
        let gateway = request.into_inner().gateway.unwrap();
        state.gateway = Some(gateway.clone());
        state.writes.push("gateway");
        Ok(Response::new(gateway))
    }
    async fn delete_gateway(
        &self,
        _: Request<DeleteGatewayRequest>,
    ) -> Result<Response<DeleteGatewayResponse>, Status> {
        let mut state = self.0.lock().unwrap();
        state.gateway = None;
        state.writes.push("delete-gateway");
        Ok(Response::new(DeleteGatewayResponse {}))
    }
    async fn get_workspace(
        &self,
        _: Request<GetWorkspaceRequest>,
    ) -> Result<Response<Workspace>, Status> {
        self.0
            .lock()
            .unwrap()
            .workspace
            .clone()
            .map(Response::new)
            .ok_or_else(|| Status::not_found("workspace"))
    }
    async fn update_workspace(
        &self,
        request: Request<UpdateWorkspaceRequest>,
    ) -> Result<Response<Workspace>, Status> {
        let mut state = self.0.lock().unwrap();
        let workspace = request.into_inner().workspace.unwrap();
        state.workspace = Some(workspace.clone());
        state.writes.push("workspace");
        Ok(Response::new(workspace))
    }
    async fn delete_workspace(
        &self,
        _: Request<DeleteWorkspaceRequest>,
    ) -> Result<Response<DeleteWorkspaceResponse>, Status> {
        let mut state = self.0.lock().unwrap();
        state.workspace = None;
        state.writes.push("delete-workspace");
        Ok(Response::new(DeleteWorkspaceResponse {}))
    }
}
pub async fn server() -> (Mock, String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mock = Mock::default();
    let service = mock.clone();
    let handle = tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(ax_server::AxServer::new(service))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });
    (mock, format!("http://{addr}"), handle)
}
pub async fn router() -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let records = Arc::new(Mutex::new(Vec::new()));
    let saved = records.clone();
    let handle = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut data = Vec::new();
            let mut buf = [0; 8192];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n]);
                if let Some(end) = data.windows(4).position(|b| b == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&data[..end]).to_lowercase();
                    let size = headers
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: "))
                        .unwrap()
                        .parse::<usize>()
                        .unwrap();
                    if data.len() >= end + 4 + size {
                        break;
                    }
                }
            }
            saved.lock().unwrap().push(String::from_utf8(data).unwrap());
            socket
                .write_all(
                    b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        }
    });
    (format!("http://{addr}"), records, handle)
}
