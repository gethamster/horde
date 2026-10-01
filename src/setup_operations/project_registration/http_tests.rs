use super::*;
use crate::projects;

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repos: PathBuf,
    base: String,
    client: reqwest::Client,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) { self.server.abort(); }
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        database(&root).unwrap();
        let repos = dir.path().join("repos");
        std::fs::create_dir(&repos).unwrap();
        // Captured synchronously at construction; no live installation configuration.
        let state = {
            static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let _guard = ENV.lock().unwrap();
            unsafe { std::env::set_var("HORDE_EXECUTION_WORKSPACE_ROOT", &repos); }
            let admin = Admin::new(root.clone(), "fixture-administrator-token");
            unsafe { std::env::remove_var("HORDE_EXECUTION_WORKSPACE_ROOT"); }
            Arc::new(admin)
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1/setup", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
        Self { _dir: dir, root, repos, base, client: reqwest::Client::new(), server }
    }
    fn repo(&self, slug: &str) {
        let path = self.repos.join(slug);
        std::fs::create_dir(&path).unwrap();
        assert!(std::process::Command::new("git").env_clear().env("PATH", "/usr/bin:/bin").args(["init", "--quiet"]).arg(path).status().unwrap().success());
    }
    async fn put_raw(&self, id: &str, body: &str) -> reqwest::Response {
        self.client.put(format!("{}/operations/{id}", self.base)).bearer_auth("fixture-administrator-token").header("content-type", "application/json").body(body.to_owned()).send().await.unwrap()
    }
    async fn put(&self, id: &str, body: &Value) -> reqwest::Response { self.put_raw(id, &body.to_string()).await }
    async fn get(&self, id: &str) -> Value {
        self.client.get(format!("{}/operations/{id}", self.base)).bearer_auth("fixture-administrator-token").send().await.unwrap().json().await.unwrap()
    }
}
fn payload(slug: &str, id: &str) -> Value {
    json!({"kind":"project-registration","config":{"schema_version":1,"scope":"foundry","projects":[{"id":id,"slug":slug,"name":"Deliver","tenant_id":"foundry","concurrency":1,"isolation":"native","repository_slug":slug,"runtime":"local"}]}})
}
const FIRST: &str = "11111111-1111-4111-8111-111111111111";
const SECOND: &str = "22222222-2222-4222-8222-222222222222";

#[tokio::test]
async fn registration_http_create_reconcile_independent_replay() {
    let f = Fixture::new().await;
    f.repo("deliver"); f.repo("signals");
    let caps: Value = f.client.get(format!("{}/capabilities", f.base)).bearer_auth("fixture-administrator-token").send().await.unwrap().json().await.unwrap();
    assert!(caps["operations"].as_array().unwrap().contains(&json!("project-registration")));
    let body = payload("deliver", FIRST);
    let response = f.put("a", &body).await;
    assert_eq!(response.status(), 200);
    let first: Value = response.json().await.unwrap();
    assert_eq!(first["state"], "succeeded");
    assert_eq!(first["result"]["projects"][0]["disposition"], "created");
    let repo_id = first["result"]["projects"][0]["repository"]["id"].as_str().unwrap();
    assert_eq!(uuid::Uuid::parse_str(repo_id).unwrap().to_string(), repo_id);
    let db = database(&f.root).unwrap();
    assert_eq!(projects::tenant(&db, FIRST).unwrap(), "foundry");
    assert!(projects::runtime_allowed(&db, FIRST, "local").unwrap());
    let reconciled: Value = f.put("again", &body).await.json().await.unwrap();
    assert_eq!(reconciled["result"]["projects"][0]["disposition"], "reconciled");
    assert_eq!(reconciled["result"]["projects"][0]["repository"]["id"], repo_id);
    assert_eq!(f.put("b", &payload("signals", SECOND)).await.status(), 200);
    assert_eq!(f.get("a").await, first);
    let mut changed = body.clone(); changed["config"]["projects"][0]["runtime"] = json!("remote");
    assert_eq!(f.put("a", &changed).await.status(), 409);
    projects::dispatch(&db, "project_runtime_revoke", &json!({"project":FIRST,"runtime":"local"})).unwrap();
    std::fs::remove_dir_all(f.repos.join("deliver")).unwrap();
    assert_eq!(f.put("a", &body).await.json::<Value>().await.unwrap(), first);
    assert!(!projects::runtime_allowed(&db, FIRST, "local").unwrap());
}

#[tokio::test]
async fn registration_http_strict_validation_before_claim() {
    let f = Fixture::new().await;
    f.repo("deliver");
    let body = payload("deliver", FIRST);
    for (field, value) in [("runtime", json!("remote")), ("concurrency", json!(1.0)), ("name", json!("bad\nname")), ("repository_slug", json!("../deliver")), ("id", json!("AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA")), ("unknown", json!(true))] {
        let mut bad = body.clone(); bad["config"]["projects"][0][field] = value;
        let response = f.put("invalid", &bad).await;
        assert_eq!(response.status(), 422, "field {field}");
        assert_eq!(f.get("invalid").await["error"], "operation not found");
    }
    for duplicate in [body.to_string().replace("\"runtime\":\"local\"", "\"runtime\":\"remote\",\"runtime\":\"local\""), body.to_string().replace("\"schema_version\":1", "\"schema_version\":2,\"schema_version\":1")] {
        assert_eq!(f.put_raw("duplicate", &duplicate).await.status(), 400);
        assert_eq!(f.get("duplicate").await["error"], "operation not found");
    }
    let mut batch = body.clone(); batch["config"]["projects"].as_array_mut().unwrap().push(payload("missing", SECOND)["config"]["projects"][0].clone());
    assert_eq!(f.put("batch", &batch).await.status(), 409);
    let db = database(&f.root).unwrap();
    assert!(projects::resolve(&db, FIRST).is_err());
    assert!(projects::resolve(&db, SECOND).is_err());
}

#[tokio::test]
async fn registration_http_native_binding_conflicts_are_durable() {
    let f = Fixture::new().await; f.repo("deliver");
    let body = payload("deliver", FIRST);
    let db = database(&f.root).unwrap();
    projects::dispatch(&db, "project_create", &body["config"]["projects"][0]).unwrap();
    let response = f.put("native", &body).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.json::<Value>().await.unwrap()["result"]["projects"][0]["disposition"], "reconciled");
    projects::dispatch(&db, "project_runtime_revoke", &json!({"project":FIRST,"runtime":"local"})).unwrap();
    let response = f.put("revoked", &body).await;
    assert_eq!(response.status(), 409);
    let failed: Value = response.json().await.unwrap();
    assert_eq!(failed["state"], "failed");
    assert_eq!(f.get("revoked").await, failed);
    assert_eq!(f.put("revoked", &body).await.status(), 409);
    assert!(!failed.to_string().contains(f.root.to_str().unwrap()));
}
