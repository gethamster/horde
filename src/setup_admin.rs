//! Private administrative setup transport. Receipts never contain request secrets.
use crate::store::Store;
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, sync::Arc};

struct Admin {
    root: PathBuf,
    token: [u8; 32],
    requests: std::sync::Mutex<(std::time::Instant, u32)>,
}
impl Admin {
    fn new(root: PathBuf, token: &str) -> Self {
        Self {
            root,
            token: Sha256::digest(token.as_bytes()).into(),
            requests: std::sync::Mutex::new((std::time::Instant::now(), 0)),
        }
    }
}
async fn guard(
    State(state): State<Arc<Admin>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let allowed = state
        .requests
        .lock()
        .map(|mut counter| {
            if counter.0.elapsed() >= std::time::Duration::from_secs(1) {
                *counter = (std::time::Instant::now(), 0);
            }
            counter.1 = counter.1.saturating_add(1);
            counter.1 <= 120
        })
        .unwrap_or(false);
    if !allowed {
        return error(StatusCode::TOO_MANY_REQUESTS, "request rate exceeded").into_response();
    }
    if !authorized(&state, request.headers()) {
        return error(
            StatusCode::UNAUTHORIZED,
            "administrative credential required",
        )
        .into_response();
    }
    next.run(request).await
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    kind: String,
    config: Value,
}
type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;
fn error(code: StatusCode, text: &str) -> (StatusCode, Json<Value>) {
    (code, Json(json!({"error":text})))
}
fn authorized(state: &Admin, headers: &HeaderMap) -> bool {
    let supplied = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    supplied.is_some_and(|v| {
        let digest: [u8; 32] = Sha256::digest(v.as_bytes()).into();
        digest
            .iter()
            .zip(state.token)
            .fold(0u8, |a, (b, c)| a | (b ^ c))
            == 0
    })
}
fn database(root: &std::path::Path) -> Result<Store> {
    let db = Store::open(root)?;
    db.conn.execute_batch("CREATE TABLE IF NOT EXISTS setup_receipts(id TEXT PRIMARY KEY,kind TEXT NOT NULL,digest TEXT NOT NULL,state TEXT NOT NULL,result TEXT);")?;
    Ok(db)
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn receipt(db: &Store, id: &str) -> Result<Option<Value>> {
    Ok(db.conn.query_row("SELECT a.kind,CASE WHEN EXISTS(SELECT 1 FROM setup_receipts b WHERE b.kind=a.kind AND b.rowid>a.rowid) THEN 'superseded' ELSE a.state END,a.result FROM setup_receipts a WHERE a.id=?",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?))).optional()?.map(|(kind,state,result)|json!({"id":id,"kind":kind,"state":state,"result":result.and_then(|v|serde_json::from_str::<Value>(&v).ok())})))
}
async fn capabilities(State(s): State<Arc<Admin>>, h: HeaderMap) -> Reply {
    if !authorized(&s, &h) {
        return Err(error(
            StatusCode::UNAUTHORIZED,
            "administrative credential required",
        ));
    }
    Ok(Json(
        json!({"version":1,"operations":["execution-profile","workspace","account-pool","storage"],"idempotency":true}),
    ))
}
async fn status(State(s): State<Arc<Admin>>, h: HeaderMap, Path(id): Path<String>) -> Reply {
    if !authorized(&s, &h) {
        return Err(error(
            StatusCode::UNAUTHORIZED,
            "administrative credential required",
        ));
    }
    if !valid_id(&id) {
        return Err(error(StatusCode::BAD_REQUEST, "invalid operation ID"));
    }
    let db = database(&s.root)
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "receipt unavailable"))?;
    receipt(&db, &id)
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "receipt unavailable"))?
        .map(Json)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "operation not found"))
}
async fn apply(
    State(s): State<Arc<Admin>>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<Request>,
) -> Reply {
    if !authorized(&s, &h) {
        return Err(error(
            StatusCode::UNAUTHORIZED,
            "administrative credential required",
        ));
    }
    if !valid_id(&id)
        || !["execution-profile", "workspace", "account-pool", "storage"]
            .contains(&request.kind.as_str())
    {
        return Err(error(StatusCode::BAD_REQUEST, "invalid operation"));
    }
    // Blocking work survives HTTP disconnects. Claim persists before any side effect.
    tokio::task::spawn_blocking(move || apply_blocking(&s.root, &id, &request))
        .await
        .map_err(|_| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "operation interrupted; inspect receipt",
            )
        })?
}
fn apply_blocking(root: &std::path::Path, id: &str, request: &Request) -> Reply {
    use fs2::FileExt;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("setup-admin.lock"))
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "setup lock unavailable"))?;
    lock.try_lock_exclusive().map_err(|_| {
        error(
            StatusCode::TOO_MANY_REQUESTS,
            "another setup operation is active",
        )
    })?;
    let db = database(root)
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "receipt unavailable"))?;
    let digest = hex::encode(Sha256::digest(serde_json::to_vec(request).unwrap()));
    let inserted = db
        .conn
        .execute(
            "INSERT OR IGNORE INTO setup_receipts(id,kind,digest,state) VALUES(?,?,?,'running')",
            params![id, request.kind, digest],
        )
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "receipt unavailable"))?;
    if inserted == 0 {
        let previous: String = db
            .conn
            .query_row("SELECT digest FROM setup_receipts WHERE id=?", [id], |r| {
                r.get(0)
            })
            .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "receipt unavailable"))?;
        if previous != digest {
            return Err(error(
                StatusCode::CONFLICT,
                "operation ID already has a different request",
            ));
        }
    } else {
        let result = execute(&db, request);
        let (state, value) = match result {
            Ok(v) => ("succeeded", v),
            Err(_) => (
                "failed",
                json!({"error":"setup failed; reconcile configuration and component state before another operation"}),
            ),
        };
        db.conn
            .execute(
                "UPDATE setup_receipts SET state=?,result=? WHERE id=?",
                params![state, value.to_string(), id],
            )
            .map_err(|_| {
                error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "operation outcome unavailable; inspect receipt",
                )
            })?;
    }
    let result = receipt(&db, id)
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "receipt unavailable"))?
        .unwrap();
    // Explicit unlock also releases a temporarily inherited descriptor during fork/exec.
    fs2::FileExt::unlock(&lock).map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "setup lock release failed",
        )
    })?;
    Ok(Json(result))
}
fn execute(db: &Store, r: &Request) -> Result<Value> {
    match r.kind.as_str() {
        "execution-profile" => crate::execution_setup::configure(db, &r.config),
        "workspace" => crate::setup_operations::workspaces(db, &r.config),
        "account-pool" => crate::setup_operations::account_pool(db, &r.config),
        "storage" => {
            crate::storage::dispatch(db, "runtime_storage_configure", &r.config)?
                .context("storage operation missing")?;
            Ok(json!({"configured":true}))
        }
        _ => anyhow::bail!("unsupported operation"),
    }
}
pub async fn serve(root: PathBuf, listen: std::net::SocketAddr) -> Result<()> {
    let path = std::env::var("HORDE_SETUP_ADMIN_TOKEN_FILE")
        .context("HORDE_SETUP_ADMIN_TOKEN_FILE required")?;
    let token = std::fs::read_to_string(path)?;
    let token = token.trim();
    ensure!(
        token.len() >= 32,
        "admin credential must have at least 32 characters"
    );
    let state = Arc::new(Admin::new(root, token));
    let app = Router::new()
        .route("/v1/setup/capabilities", get(capabilities))
        .route("/v1/setup/operations/{id}", get(status).put(apply))
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024))
        .layer(axum::middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state.clone());
    axum::serve(tokio::net::TcpListener::bind(listen).await?, app).await?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_project_credentials() {
        let s = Admin::new(PathBuf::new(), "administrator");
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer project-grant".parse().unwrap());
        assert!(!authorized(&s, &h));
    }
    #[test]
    fn rejects_traversal() {
        assert!(!valid_id("../../x"));
        assert!(valid_id("operation-123"));
    }
    #[test]
    fn interrupted_receipt_never_replays_and_payload_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let db = database(dir.path()).unwrap();
        let r = Request {
            kind: "workspace".into(),
            config: json!({"secret":"not-in-receipt"}),
        };
        let digest = hex::encode(Sha256::digest(serde_json::to_vec(&r).unwrap()));
        db.conn
            .execute(
                "INSERT INTO setup_receipts VALUES('test','workspace',?,'running',NULL)",
                [digest],
            )
            .unwrap();
        assert_eq!(
            apply_blocking(dir.path(), "test", &r).unwrap().0["state"],
            "running"
        );
        assert!(
            !receipt(&db, "test")
                .unwrap()
                .unwrap()
                .to_string()
                .contains("not-in-receipt")
        );
        assert_eq!(
            apply_blocking(
                dir.path(),
                "test",
                &Request {
                    kind: "storage".into(),
                    config: json!({})
                }
            )
            .unwrap_err()
            .0,
            StatusCode::CONFLICT
        );
    }
    #[tokio::test]
    async fn http_auth_apply_replay_and_supersession() {
        let dir = tempfile::tempdir().unwrap();
        let token = "test-administrator-token-not-project";
        let state = Arc::new(Admin::new(dir.path().into(), token));
        let app = Router::new()
            .route("/v1/setup/capabilities", get(capabilities))
            .route("/v1/setup/operations/{id}", get(status).put(apply))
            .layer(axum::middleware::from_fn_with_state(state.clone(), guard))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::new();
        let base = format!("http://{address}/v1/setup");
        assert_eq!(
            client
                .get(format!("{base}/capabilities"))
                .bearer_auth("project-grant")
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            client
                .get(format!("{base}/capabilities"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let request = json!({"kind":"storage","config":{"automatic_cleanup":false}});
        let first: Value = client
            .put(format!("{base}/operations/a"))
            .bearer_auth(token)
            .json(&request)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(first["state"], "succeeded");
        let replay: Value = client
            .put(format!("{base}/operations/a"))
            .bearer_auth(token)
            .json(&request)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(replay, first);
        assert_eq!(
            client
                .put(format!("{base}/operations/a"))
                .bearer_auth(token)
                .json(&json!({"kind":"storage","config":{"automatic_cleanup":true}}))
                .send()
                .await
                .unwrap()
                .status(),
            409
        );
        let _: Value = client
            .put(format!("{base}/operations/b"))
            .bearer_auth(token)
            .json(&request)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let old: Value = client
            .get(format!("{base}/operations/a"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(old["state"], "superseded");
        *state.requests.lock().unwrap() = (std::time::Instant::now(), 120);
        assert_eq!(
            client
                .get(format!("{base}/capabilities"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .status(),
            429
        );
        server.abort();
    }
    #[tokio::test]
    async fn validates_requests_and_retains_failed_receipts() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(Admin::new(dir.path().into(), "administrator-token"));
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            "Bearer administrator-token".parse().unwrap(),
        );
        assert_eq!(
            status(
                State(state.clone()),
                headers.clone(),
                Path("missing".into())
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(State(state.clone()), headers.clone(), Path("../bad".into()))
                .await
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            status(State(state.clone()), HeaderMap::new(), Path("id".into()))
                .await
                .unwrap_err()
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            capabilities(State(state.clone()), HeaderMap::new())
                .await
                .unwrap_err()
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            apply(
                State(state.clone()),
                headers.clone(),
                Path("id".into()),
                Json(Request {
                    kind: "invalid".into(),
                    config: json!({})
                })
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            apply(
                State(state.clone()),
                HeaderMap::new(),
                Path("id".into()),
                Json(Request {
                    kind: "storage".into(),
                    config: json!({})
                })
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::UNAUTHORIZED
        );
        for kind in ["workspace", "account-pool", "execution-profile"] {
            let request = Request {
                kind: kind.into(),
                config: json!({}),
            };
            let receipt = apply_blocking(dir.path(), kind, &request).unwrap().0;
            assert_eq!(receipt["state"], "failed");
            assert_eq!(
                apply_blocking(dir.path(), kind, &request).unwrap().0,
                receipt
            );
        }
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .open(dir.path().join("setup-admin.lock"))
            .unwrap();
        fs2::FileExt::lock_exclusive(&lock).unwrap();
        assert_eq!(
            apply_blocking(
                dir.path(),
                "locked",
                &Request {
                    kind: "storage".into(),
                    config: json!({})
                }
            )
            .unwrap_err()
            .0,
            StatusCode::TOO_MANY_REQUESTS
        );
        fs2::FileExt::unlock(&lock).unwrap();
    }
}
