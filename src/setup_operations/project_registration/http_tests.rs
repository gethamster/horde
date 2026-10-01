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
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        database(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let repos = dir.path().join("repos");
        std::fs::create_dir(&repos).unwrap();
        // macOS exposes temporary directories through /var -> /private/var.
        // Exercise an ordinary checkout while retaining explicit symlink denials.
        let repos = repos.canonicalize().unwrap();
        let mut admin = Admin::new(root.clone(), "fixture-administrator-token");
        admin.execution_root = repos.clone();
        let state = Arc::new(admin);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1/setup", listener.local_addr().unwrap());
        let server =
            tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
        Self {
            _dir: dir,
            root,
            repos,
            base,
            client: reqwest::Client::new(),
            server,
        }
    }
    fn repo(&self, slug: &str) {
        let path = self.repos.join(slug);
        std::fs::create_dir(&path).unwrap();
        assert!(
            std::process::Command::new("git")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .args(["init", "--quiet"])
                .arg(path)
                .status()
                .unwrap()
                .success()
        );
    }
    async fn put_raw(&self, id: &str, body: &str) -> reqwest::Response {
        self.client
            .put(format!("{}/operations/{id}", self.base))
            .bearer_auth("fixture-administrator-token")
            .header("content-type", "application/json")
            .body(body.to_owned())
            .send()
            .await
            .unwrap()
    }
    async fn put(&self, id: &str, body: &Value) -> reqwest::Response {
        self.put_raw(id, &body.to_string()).await
    }
    async fn get(&self, id: &str) -> Value {
        self.client
            .get(format!("{}/operations/{id}", self.base))
            .bearer_auth("fixture-administrator-token")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
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
    f.repo("deliver");
    f.repo("signals");
    let caps: Value = f
        .client
        .get(format!("{}/capabilities", f.base))
        .bearer_auth("fixture-administrator-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        caps["operations"]
            .as_array()
            .unwrap()
            .contains(&json!("project-registration"))
    );
    let body = payload("deliver", FIRST);
    let response = f.put("a", &body).await;
    assert_eq!(response.status(), 200);
    let first: Value = response.json().await.unwrap();
    assert_eq!(first["state"], "succeeded");
    assert_eq!(first["result"]["projects"][0]["disposition"], "created");
    let repo_id = first["result"]["projects"][0]["repository"]["id"]
        .as_str()
        .unwrap();
    assert_eq!(uuid::Uuid::parse_str(repo_id).unwrap().to_string(), repo_id);
    assert_eq!(
        first["result"],
        json!({"schema_version":1,"scope":"foundry","projects":[{"id":FIRST,"slug":"deliver","name":"Deliver","tenant_id":"foundry","concurrency":1,"isolation":"native","repository":{"id":repo_id,"slug":"deliver"},"runtime":"local","disposition":"created"}]})
    );
    let mut unknown_kind = body.clone();
    unknown_kind["kind"] = json!("future-kind");
    assert_eq!(f.put("a", &unknown_kind).await.status(), 409);

    let db = database(&f.root).unwrap();
    assert_eq!(projects::tenant(&db, FIRST).unwrap(), "foundry");
    assert!(projects::runtime_allowed(&db, FIRST, "local").unwrap());
    let reconciled: Value = f.put("again", &body).await.json().await.unwrap();
    assert_eq!(
        reconciled["result"]["projects"][0]["disposition"],
        "reconciled"
    );
    assert_eq!(
        reconciled["result"]["projects"][0]["repository"]["id"],
        repo_id
    );
    assert_eq!(f.put("b", &payload("signals", SECOND)).await.status(), 200);
    assert_eq!(f.get("a").await, first);
    let mut changed = body.clone();
    changed["config"]["projects"][0]["runtime"] = json!("remote");
    assert_eq!(f.put("a", &changed).await.status(), 409);
    projects::dispatch(
        &db,
        "project_runtime_revoke",
        &json!({"project":FIRST,"runtime":"local"}),
    )
    .unwrap();
    std::fs::remove_dir_all(f.repos.join("deliver")).unwrap();
    assert_eq!(
        f.put("a", &body).await.json::<Value>().await.unwrap(),
        first
    );
    assert!(!projects::runtime_allowed(&db, FIRST, "local").unwrap());
}

#[tokio::test]
async fn registration_http_strict_validation_before_claim() {
    let f = Fixture::new().await;
    f.repo("deliver");
    let body = payload("deliver", FIRST);
    for (field, value) in [
        ("runtime", json!("remote")),
        ("concurrency", json!(1.0)),
        ("name", json!("bad\nname")),
        ("repository_slug", json!("../deliver")),
        ("id", json!("AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA")),
        ("unknown", json!(true)),
    ] {
        let mut bad = body.clone();
        bad["config"]["projects"][0][field] = value;
        let response = f.put("invalid", &bad).await;
        assert_eq!(response.status(), 422, "field {field}");
        assert_eq!(f.get("invalid").await["error"], "operation not found");
    }
    for duplicate in [
        body.to_string().replace(
            "\"runtime\":\"local\"",
            "\"runtime\":\"remote\",\"runtime\":\"local\"",
        ),
        body.to_string().replace(
            "\"schema_version\":1",
            "\"schema_version\":2,\"schema_version\":1",
        ),
    ] {
        assert_eq!(f.put_raw("duplicate", &duplicate).await.status(), 400);
        assert_eq!(f.get("duplicate").await["error"], "operation not found");
    }
    let mut batch = body.clone();
    batch["config"]["projects"]
        .as_array_mut()
        .unwrap()
        .push(payload("missing", SECOND)["config"]["projects"][0].clone());
    assert_eq!(f.put("batch", &batch).await.status(), 409);
    let db = database(&f.root).unwrap();
    assert!(projects::resolve(&db, FIRST).is_err());
    assert!(projects::resolve(&db, SECOND).is_err());
}

#[tokio::test]
async fn registration_http_native_binding_conflicts_are_durable() {
    let f = Fixture::new().await;
    f.repo("deliver");
    let body = payload("deliver", FIRST);
    let db = database(&f.root).unwrap();
    projects::dispatch(&db, "project_create", &body["config"]["projects"][0]).unwrap();
    let response = f.put("native", &body).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap()["result"]["projects"][0]["disposition"],
        "reconciled"
    );
    projects::dispatch(
        &db,
        "project_runtime_revoke",
        &json!({"project":FIRST,"runtime":"local"}),
    )
    .unwrap();
    let response = f.put("revoked", &body).await;
    assert_eq!(response.status(), 409);
    let failed: Value = response.json().await.unwrap();
    assert_eq!(failed["state"], "failed");
    assert_eq!(f.get("revoked").await, failed);
    assert_eq!(f.put("revoked", &body).await.status(), 409);
    assert!(!failed.to_string().contains(f.root.to_str().unwrap()));
}

fn snapshot(db: &Store) -> Value {
    let mut result = serde_json::Map::new();
    for row in db.rows("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '%fts%' AND name!='setup_receipts' ORDER BY name", &[]).unwrap() {
        let name = row["name"].as_str().unwrap();
        result.insert(name.into(), json!(db.rows(&format!("SELECT * FROM \"{name}\" ORDER BY rowid"), &[]).unwrap()));
    }
    Value::Object(result)
}
fn seed_running(f: &Fixture, id: &str, body: &Value) -> String {
    use crate::setup_operations::project_registration as registration;
    let db = database(&f.root).unwrap();
    let request: Request = serde_json::from_value(body.clone()).unwrap();
    let config = registration::validate(&request.config).unwrap();
    let context = registration::context(&db, &f.repos, &config)
        .unwrap()
        .fingerprint()
        .unwrap();
    let digest = hex::encode(Sha256::digest(serde_json::to_vec(&request).unwrap()));
    db.conn
        .execute(
            "INSERT INTO setup_receipts VALUES(?,'project-registration',?,'running',?)",
            params![id, digest, json!({"context_hash":context}).to_string()],
        )
        .unwrap();
    context
}

#[tokio::test]
async fn registration_http_running_context_restart_and_uncertain_commit() {
    let f = Fixture::new().await;
    f.repo("deliver");
    let body = payload("deliver", FIRST);
    let hash = seed_running(&f, "resume", &body);
    let running = f.get("resume").await;
    assert_eq!(running["state"], "running");
    assert_eq!(running["result"], json!({"context_hash":hash}));
    let db = database(&f.root).unwrap();
    assert!(projects::resolve(&db, FIRST).is_err());
    crate::management::set(&db, "runtime.identity", "changed-local-identity").unwrap();
    assert_eq!(f.put("resume", &body).await.status(), 409);
    assert_eq!(f.get("resume").await, running);
    assert!(projects::resolve(&db, FIRST).is_err());
    crate::management::set(&db, "runtime.identity", "local").unwrap();
    let request: Request = serde_json::from_value(body.clone()).unwrap();
    let different = tempfile::tempdir().unwrap();
    assert_eq!(
        apply_at(&f.root, different.path(), "resume", &request)
            .unwrap_err()
            .0,
        StatusCode::CONFLICT
    );
    // Inject a database failure at receipt persistence, after binding writes.
    db.conn.execute_batch("CREATE TRIGGER registration_fail BEFORE UPDATE ON setup_receipts WHEN NEW.state='succeeded' BEGIN SELECT RAISE(ABORT,'test private SQL path token'); END;").unwrap();
    let response = f.put("resume", &body).await;
    assert_eq!(response.status(), 500);
    let error: Value = response.json().await.unwrap();
    assert!(!error.to_string().contains("private SQL"));
    assert!(projects::resolve(&db, FIRST).is_err());
    assert_eq!(f.get("resume").await, running);
    db.conn
        .execute_batch("DROP TRIGGER registration_fail;")
        .unwrap();
    let success = f.put("resume", &body).await;
    assert_eq!(success.status(), 200);
    let receipt: Value = success.json().await.unwrap();
    // Model an error reported after COMMIT: fresh durable success wins, even over
    // a deterministic error. The helper also covers real Store::atomic failures.
    assert_eq!(
        registration_outcome(
            &db,
            "resume",
            Err(
                crate::setup_operations::project_registration::Conflict("repository_conflict")
                    .into()
            )
        )
        .unwrap()
        .0,
        receipt
    );
    drop(db);
    // New Store/handler instance models process restart without retained memory.
    assert_eq!(
        apply_at(&f.root, &f.repos, "resume", &request).unwrap().0,
        receipt
    );
    let db = database(&f.root).unwrap();
    let count: i64 = db
        .conn
        .query_row(
            "SELECT count(*) FROM project_repositories WHERE project=?",
            [FIRST],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn registration_http_protects_task_bearing_legacy_and_accounts() {
    const LEGACY: &str = "cf138992-6bec-59f7-bbe8-8218a9ed3312";
    const RUN: &str = "f4ffa6a3-8351-467a-a170-7c7576cf12fc";
    let f = Fixture::new().await;
    f.repo("system");
    let body = payload("system", LEGACY);
    let db = database(&f.root).unwrap();
    projects::dispatch(&db, "project_create", &body["config"]["projects"][0]).unwrap();
    projects::dispatch(
        &db,
        "project_repo_add",
        &json!({"project":LEGACY,"path":f.repos.join("system")}),
    )
    .unwrap();
    projects::dispatch(
        &db,
        "project_runtime_grant",
        &json!({"project":LEGACY,"runtime":"local"}),
    )
    .unwrap();
    crate::accounts::dispatch(
        &db,
        "account_create",
        &json!({"project":LEGACY,"name":"fixture","provider":"codex","auth_mode":"login"}),
    )
    .unwrap();
    crate::management::set(&db, "concurrency", "2").unwrap();
    db.conn
        .execute(
            "INSERT INTO tasks VALUES(?,'protected',?,'succeeded','{}','{}',0)",
            params![RUN, f.repos.join("system").to_str().unwrap()],
        )
        .unwrap();
    projects::bind_task(&db, RUN, LEGACY, &f.repos.join("system")).unwrap();
    let before = snapshot(&db);
    let response = f.put("legacy", &body).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<Value>().await.unwrap()["result"]["projects"][0]["disposition"],
        "reconciled"
    );
    assert_eq!(snapshot(&db), before);
    db.conn
        .execute(
            "DELETE FROM project_runtime_grants WHERE project=?",
            [LEGACY],
        )
        .unwrap();
    let before = snapshot(&db);
    let response = f.put("missing-grant", &body).await;
    assert_eq!(response.status(), 409);
    assert_eq!(
        response.json::<Value>().await.unwrap()["result"]["error"],
        "project_has_history"
    );
    assert_eq!(snapshot(&db), before);
    assert_eq!(f.put("legacy", &body).await.status(), 200);
    assert_eq!(snapshot(&db), before);
}

#[tokio::test]
async fn registration_http_quiet_guard_is_not_a_drain_or_replay_requirement() {
    let f = Fixture::new().await;
    f.repo("deliver");
    f.repo("signals");
    let body = payload("deliver", FIRST);
    let first: Value = f.put("done", &body).await.json().await.unwrap();
    let second = payload("signals", SECOND);
    seed_running(&f, "unfinished", &second);
    let db = database(&f.root).unwrap();
    db.conn.execute_batch("INSERT INTO tasks VALUES('task','test','/tmp','running','{}','{}',0); INSERT INTO steps VALUES('step','task','test','{}','running',NULL); INSERT INTO attempts(id,step,state,started) VALUES('attempt','step','running',0);").unwrap();
    for state in ["running", "uncertain"] {
        db.conn
            .execute("UPDATE attempts SET state=?", [state])
            .unwrap();
        for id in ["new", "unfinished"] {
            let response = f.put(id, &second).await;
            assert_eq!(response.status(), 409);
            assert_eq!(
                response.json::<Value>().await.unwrap()["error"],
                "runtime_not_quiescent"
            );
        }
        assert_eq!(
            f.put("done", &body).await.json::<Value>().await.unwrap(),
            first
        );
        assert!(projects::resolve(&db, SECOND).is_err());
    }
    assert_eq!(f.get("new").await["error"], "operation not found");
    assert_eq!(f.get("unfinished").await["state"], "running");
    assert!(crate::management::value(&db, "drain").unwrap().is_none());
}

#[tokio::test]
async fn registration_http_filesystem_and_ownership_denials() {
    for mode in [
        "symlink",
        "metadata",
        "gitfile",
        "plain",
        "cross-project",
        "stored-path",
        "dedicated",
        "common-indirection",
        "ancestor",
    ] {
        let f = Fixture::new().await;
        f.repo("deliver");
        let db = database(&f.root).unwrap();
        let body = payload("deliver", FIRST);
        let path = f.repos.join("deliver");
        match mode {
            "symlink" => {
                std::fs::rename(&path, f.repos.join("actual")).unwrap();
                std::os::unix::fs::symlink(f.repos.join("actual"), &path).unwrap();
            }
            "ancestor" => {
                std::fs::rename(&f.repos, f._dir.path().join("actual")).unwrap();
                std::os::unix::fs::symlink(f._dir.path().join("actual"), &f.repos).unwrap();
            }
            "metadata" => {
                std::fs::rename(path.join(".git"), path.join("actual-git")).unwrap();
                std::os::unix::fs::symlink(path.join("actual-git"), path.join(".git")).unwrap();
            }
            "gitfile" => {
                std::fs::remove_dir_all(path.join(".git")).unwrap();
                std::fs::write(path.join(".git"), "gitdir: /private/not-authorized").unwrap();
            }
            "plain" => {
                std::fs::remove_dir_all(path.join(".git")).unwrap();
            }
            "common-indirection" => {
                std::fs::write(path.join(".git/commondir"), "/private/not-authorized").unwrap();
            }
            "cross-project" => {
                projects::dispatch(
                    &db,
                    "project_repo_add",
                    &json!({"project":"default","path":path}),
                )
                .unwrap();
            }
            "stored-path" => {
                projects::dispatch(&db, "project_create", &body["config"]["projects"][0]).unwrap();
                projects::dispatch(
                    &db,
                    "project_repo_add",
                    &json!({"project":FIRST,"path":path}),
                )
                .unwrap();
                db.conn.execute("UPDATE project_repositories SET path='/different/private/path' WHERE project=?",[FIRST]).unwrap();
            }
            "dedicated" => {
                projects::bind_runtime(&db, "default", "local").unwrap();
            }
            _ => unreachable!(),
        }
        let before = snapshot(&db);
        let response = f.put("denied", &body).await;
        assert_eq!(response.status(), 409, "mode {mode}");
        let receipt: Value = response.json().await.unwrap();
        assert_eq!(receipt["state"], "failed");
        assert!(!receipt.to_string().contains("private"));
        assert!(
            !receipt
                .to_string()
                .contains(f._dir.path().to_str().unwrap())
        );
        assert_eq!(snapshot(&db), before, "mode {mode}");
    }
}

#[tokio::test]
async fn registration_http_authentication_limits_and_envelope() {
    let f = Fixture::new().await;
    let body = payload("deliver", FIRST);
    for token in ["", "project-grant"] {
        assert_eq!(
            f.client
                .put(format!("{}/operations/unauthorized", f.base))
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
    }
    assert_eq!(f.get("unauthorized").await["error"], "operation not found");
    assert_eq!(
        f.client
            .put(format!("{}/operations/type", f.base))
            .bearer_auth("fixture-administrator-token")
            .body(body.to_string())
            .send()
            .await
            .unwrap()
            .status(),
        415
    );
    assert_eq!(
        f.put_raw("big", &" ".repeat(1024 * 1024 + 1))
            .await
            .status(),
        413
    );
    assert_eq!(f.put_raw("malformed", "{").await.status(), 400);
    assert_eq!(
        f.put_raw(
            "envelope",
            &body
                .to_string()
                .replace("\"kind\":", "\"extra\":true,\"kind\":")
        )
        .await
        .status(),
        400
    );
    assert_eq!(
        f.put_raw(
            "duplicate-kind",
            &body
                .to_string()
                .replace("\"kind\":", "\"kind\":\"storage\",\"kind\":")
        )
        .await
        .status(),
        400
    );
    assert_eq!(f.put(&"a".repeat(129), &body).await.status(), 400);
    let mut rate_limited = false;
    for _ in 0..125 {
        if f.client
            .get(format!("{}/capabilities", f.base))
            .send()
            .await
            .unwrap()
            .status()
            == 429
        {
            rate_limited = true;
            break;
        }
    }
    assert!(rate_limited);
}

#[tokio::test]
async fn registration_http_batch_validation_order_and_atomicity() {
    let f = Fixture::new().await;
    f.repo("deliver");
    f.repo("signals");
    let db = database(&f.root).unwrap();
    let first = payload("deliver", FIRST);
    let second = payload("signals", SECOND);
    let mut batch = second.clone();
    batch["config"]["projects"]
        .as_array_mut()
        .unwrap()
        .push(first["config"]["projects"][0].clone());
    // Final entry conflicts; no earlier project, tenant, repository or grant can survive.
    projects::dispatch(&db, "project_create", &json!({"id":FIRST,"slug":"deliver","name":"Different","tenant_id":"foundry","concurrency":1})).unwrap();
    let before = snapshot(&db);
    let response = f.put("later-conflict", &batch).await;
    assert_eq!(response.status(), 409);
    assert_eq!(snapshot(&db), before);
    db.conn
        .execute("UPDATE projects SET name='Deliver' WHERE id=?", [FIRST])
        .unwrap();
    let response: Value = f.put("ordered", &batch).await.json().await.unwrap();
    let results = response["result"]["projects"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["id"], SECOND);
    assert_eq!(results[0]["disposition"], "created");
    assert_eq!(results[1]["id"], FIRST);
    assert_eq!(results[1]["disposition"], "reconciled");
    assert_ne!(
        results[0]["repository"]["id"],
        results[1]["repository"]["id"]
    );
    for key in ["id", "slug", "repository_slug"] {
        let mut duplicate = batch.clone();
        duplicate["config"]["projects"][1][key] = duplicate["config"]["projects"][0][key].clone();
        assert_eq!(f.put("duplicate", &duplicate).await.status(), 409);
        assert_eq!(f.get("duplicate").await["error"], "operation not found");
    }
    // Reordering object members retains digest, while array order is significant.
    let raw = format!(
        "{{\"config\":{},\"kind\":\"project-registration\"}}",
        batch["config"]
    );
    assert_eq!(
        f.put_raw("ordered", &raw)
            .await
            .json::<Value>()
            .await
            .unwrap(),
        response
    );
    batch["config"]["projects"]
        .as_array_mut()
        .unwrap()
        .reverse();
    assert_eq!(f.put("ordered", &batch).await.status(), 409);
}

#[tokio::test]
async fn registration_http_replaced_checkout_and_unsettled_reservation_hold() {
    let f = Fixture::new().await;
    f.repo("deliver");
    let body = payload("deliver", FIRST);
    seed_running(&f, "intent", &body);
    std::fs::rename(f.repos.join("deliver"), f.repos.join("previous")).unwrap();
    f.repo("deliver");
    assert_eq!(f.put("intent", &body).await.status(), 409);
    assert_eq!(f.get("intent").await["state"], "running");
    std::fs::remove_dir_all(f.repos.join("deliver")).unwrap();
    std::fs::rename(f.repos.join("previous"), f.repos.join("deliver")).unwrap();
    let db = database(&f.root).unwrap();
    let account = crate::accounts::dispatch(
        &db,
        "account_create",
        &json!({"project":"default","name":"fixture","provider":"codex","auth_mode":"login"}),
    )
    .unwrap()
    .unwrap();
    db.conn.execute_batch("INSERT INTO tasks VALUES('task','test','/tmp','succeeded','{}','{}',0); INSERT INTO steps VALUES('step','task','test','{}','succeeded',NULL);").unwrap();
    db.conn.execute("INSERT INTO account_reservations SELECT 'step','default',account,id,0,'uncertain',0 FROM auth_profiles WHERE account=?",[account["id"].as_str().unwrap()]).unwrap();
    for state in ["active", "uncertain", "revoked"] {
        db.conn
            .execute("UPDATE account_reservations SET state=?", [state])
            .unwrap();
        let response = f.put("intent", &body).await;
        assert_eq!(response.status(), 409);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"],
            "runtime_not_quiescent"
        );
        assert!(projects::resolve(&db, FIRST).is_err());
    }
    db.conn
        .execute("UPDATE account_reservations SET state='released'", [])
        .unwrap();
    assert_eq!(f.put("intent", &body).await.status(), 200);
}

#[tokio::test]
async fn registration_http_disconnect_then_new_listener_replays_exact_receipt() {
    use tokio::io::AsyncWriteExt;
    let mut f = Fixture::new().await;
    f.repo("deliver");
    let body = payload("deliver", FIRST).to_string();
    let address = f
        .base
        .strip_prefix("http://")
        .unwrap()
        .strip_suffix("/v1/setup")
        .unwrap();
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let request = format!(
        "PUT /v1/setup/operations/lost HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer fixture-administrator-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    // The caller never consumes the PUT response. Observe durable completion.
    let receipt = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let receipt = f.get("lost").await;
            if receipt["state"] == "succeeded" {
                break receipt;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(stream);
    f.server.abort();
    let mut admin = Admin::new(f.root.clone(), "fixture-administrator-token");
    admin.execution_root = f.repos.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    f.base = format!("http://{}/v1/setup", listener.local_addr().unwrap());
    f.server = tokio::spawn(async move {
        axum::serve(listener, router(Arc::new(admin)))
            .await
            .unwrap()
    });
    assert_eq!(f.get("lost").await, receipt);
    assert_eq!(
        f.put_raw("lost", &body)
            .await
            .json::<Value>()
            .await
            .unwrap(),
        receipt
    );
    let db = database(&f.root).unwrap();
    assert_eq!(
        db.rows(
            "SELECT * FROM project_repositories WHERE project=?",
            &[&FIRST]
        )
        .unwrap()
        .len(),
        1
    );
}

#[tokio::test]
async fn registration_http_competing_retries_and_native_writer_serialize() {
    let f = Fixture::new().await;
    f.repo("deliver");
    let body = payload("deliver", FIRST);
    let (left, right) = tokio::join!(f.put("same", &body), f.put("same", &body));
    assert!([left.status(), right.status()].contains(&StatusCode::OK));
    for response in [left, right] {
        assert!([StatusCode::OK, StatusCode::TOO_MANY_REQUESTS].contains(&response.status()));
    }
    let first = f.get("same").await;
    assert_eq!(
        f.put("same", &body).await.json::<Value>().await.unwrap(),
        first
    );
    let db = database(&f.root).unwrap();
    assert_eq!(
        db.rows(
            "SELECT * FROM project_repositories WHERE project=?",
            &[&FIRST]
        )
        .unwrap()
        .len(),
        1
    );

    // A native writer owns the database transaction before HTTP reconciliation.
    // Registration must observe its committed revocation before granting anything.
    db.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    projects::dispatch(
        &db,
        "project_runtime_revoke",
        &json!({"project":FIRST,"runtime":"local"}),
    )
    .unwrap();
    let request = f
        .client
        .put(format!("{}/operations/after-native", f.base))
        .bearer_auth("fixture-administrator-token")
        .json(&body);
    let pending = tokio::spawn(async move { request.send().await.unwrap() });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    db.conn.execute_batch("COMMIT").unwrap();
    let response = pending.await.unwrap();
    assert_eq!(response.status(), 409);
    assert_eq!(
        response.json::<Value>().await.unwrap()["result"]["error"],
        "runtime_conflict"
    );
    assert!(!projects::runtime_allowed(&db, FIRST, "local").unwrap());
}
