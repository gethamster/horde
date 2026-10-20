use super::*;
use crate::{config::Settings, template};

fn fixture() -> (tempfile::TempDir, Store, Policy) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "test@localhost"],
        vec!["config", "user.name", "Test"],
        vec!["commit", "--allow-empty", "-m", "initial"],
    ] {
        crate::git::run(&repo, &args).unwrap();
    }
    let repo = repo.canonicalize().unwrap();
    let db = Store::open(&dir.path().join("state")).unwrap();
    let skills = crate::skill_catalog::load_from(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("skills"),
    )
    .unwrap();
    crate::skill_catalog::install(&db.root, &skills).unwrap();
    let policy: Policy = serde_json::from_value(json!({
        "schema_version":1,"policy_id":"adam-upstream","policy_revision":1,
        "project_id":"default","tenant_id":"default","repo":repo,
        "repository":"gethamster/adam","base":"main","automation_grant_id":"grant-1",
        "routine_id":"routine-1","expires_at":now()+3600,
        "authority_url":"http://127.0.0.1:1/internal/upstream-maintenance/adam-upstream/authorization",
        "authority_token_env":"FOUNDRY_MAINTENANCE_AUTHORITY_TOKEN",
        "budget":{"reference":"approved-worker-budget-1","max_model_calls":2,"max_input_bytes":100,"max_output_bytes":80}
    })).unwrap();
    (dir, db, policy)
}
fn bind(policy: &Policy) -> Binding {
    serde_json::from_value(json!({"schema_version":1,"policy_id":policy.policy_id,"policy_revision":policy.policy_revision,
      "automation_grant_id":policy.automation_grant_id,"routine_id":policy.routine_id,"occurrence_id":"night-1","invocation_id":"invocation-1",
      "expires_at":policy.expires_at,"upstreams":{"vercel/ai":"a".repeat(40),"vercel/chat":"b".repeat(40),"vercel/eve":"c".repeat(40)},"budget":policy.budget})).unwrap()
}
fn task(db: &Store, policy: &Policy) -> String {
    let plan = template::compile(
        "simulated",
        &template::load_templates(std::path::Path::new("absent")).unwrap(),
        BTreeMap::from([("task".into(), "test".into())]),
    )
    .unwrap();
    db.submit("test", &policy.repo, &Settings::default(), &plan)
        .unwrap()
}
fn install(db: &Store, policy: &Policy) {
    register(db, policy.clone()).unwrap();
}
#[test]
fn an_unregistered_binding_and_changed_grant_cannot_authorize_submission() {
    let (_dir, db, policy) = fixture();
    let binding = bind(&policy);
    assert!(validate_binding(&db, &binding, "default", &policy.repo).is_err());
    install(&db, &policy);
    validate_binding(&db, &binding, "default", &policy.repo).unwrap();
    let changed = Binding {
        automation_grant_id: "other".into(),
        ..binding
    };
    assert!(validate_binding(&db, &changed, "default", &policy.repo).is_err());
}
#[test]
fn changed_revision_expiry_source_scope_and_budget_are_rejected() {
    let (_dir, db, policy) = fixture();
    install(&db, &policy);
    let binding = bind(&policy);
    for changed in [
        Binding {
            policy_revision: 2,
            ..binding.clone()
        },
        Binding {
            expires_at: policy.expires_at + 1,
            ..binding.clone()
        },
        Binding {
            budget: Budget {
                max_model_calls: 3,
                ..binding.budget.clone()
            },
            ..binding.clone()
        },
        Binding {
            budget: Budget {
                reference: "replacement".into(),
                ..binding.budget.clone()
            },
            ..binding.clone()
        },
    ] {
        assert!(validate_binding(&db, &changed, "default", &policy.repo).is_err());
    }
    let mut wrong_source = binding.clone();
    wrong_source
        .upstreams
        .insert("other/repository".into(), "d".repeat(40));
    assert!(validate_binding(&db, &wrong_source, "default", &policy.repo).is_err());
    assert!(validate_binding(&db, &binding, "other", &policy.repo).is_err());
    assert!(validate_binding(&db, &binding, "default", std::path::Path::new("/tmp")).is_err());
}
#[test]
fn policy_retries_do_not_restore_revoked_authority_and_revision_changes_fence_work() {
    let (_dir, db, policy) = fixture();
    install(&db, &policy);
    let task = task(&db, &policy);
    pin(&db, &task, &bind(&policy)).unwrap();
    check_local(&db, &task).unwrap();
    revoke(&db, &policy.policy_id, 1, "default").unwrap();
    install(&db, &policy);
    assert!(check_local(&db, &task).is_err());
    assert_eq!(db.task(&task).unwrap()["status"], "cancelled");
    let newer = Policy {
        policy_revision: 2,
        ..policy.clone()
    };
    install(&db, &newer);
    assert!(check_local(&db, &task).is_err());
}
#[test]
fn budgets_are_reserved_atomically_across_descendants_and_parallel_root_batches() {
    let (_dir, db, policy) = fixture();
    install(&db, &policy);
    let first = task(&db, &policy);
    let second = task(&db, &policy);
    let child = task(&db, &policy);
    pin(&db, &first, &bind(&policy)).unwrap();
    pin(&db, &second, &bind(&policy)).unwrap();
    db.conn
        .execute(
            "UPDATE task_tree SET root=?,parent=? WHERE task=?",
            rusqlite::params![first, first, child],
        )
        .unwrap();
    reserve_model(&db, &first, 40).unwrap();
    reserve_model(&db, &child, 60).unwrap();
    assert!(reserve_model(&db, &second, 1).is_err());
    consume_output(&db, &child, 80).unwrap();
    assert!(consume_output(&db, &second, 1).is_err());
    let state = inspection(&db, &second).unwrap();
    assert_eq!(state["usage"]["model_calls"], 2);
    assert_eq!(state["usage"]["input_bytes"], 100);
    assert_eq!(state["usage"]["output_bytes"], 80);
}
#[test]
fn ordinary_tasks_are_unchanged_and_workers_cannot_register_authority() {
    let (_dir, db, policy) = fixture();
    let task = task(&db, &policy);
    check_local(&db, &task).unwrap();
    reserve_model(&db, &task, usize::MAX).unwrap();
    assert!(inspection(&db, &task).unwrap().is_null());
    let worker = db.register(&task, None).unwrap();
    for op in [
        "maintenance_policy_register",
        "maintenance_policy_revoke",
        "maintenance_policy_get",
    ] {
        assert!(
            crate::protocol::dispatch(
                &db,
                op,
                json!({"policy":policy,"policy_id":"adam-upstream","policy_revision":1}),
                worker["token"].as_str()
            )
            .is_err()
        );
    }
    assert!(!crate::protocol::project_allowed(
        "maintenance_policy_register"
    ));
    assert!(crate::protocol::project_allowed("maintenance_policy_get"));
    assert!(crate::protocol::project_allowed(
        "maintenance_policy_revoke"
    ));
}
#[test]
fn owning_authority_must_confirm_exact_invocation_and_policy_identity() {
    let (_dir, _db, policy) = fixture();
    let binding = bind(&policy);
    let valid = json!({"active":true,"policy_id":policy.policy_id,"policy_revision":1,"automation_grant_id":policy.automation_grant_id,"routine_id":policy.routine_id,"invocation_id":binding.invocation_id,"occurrence_id":binding.occurrence_id,"expires_at":policy.expires_at});
    authority::validate_response(&policy, &binding, &valid).unwrap();
    for (field, replacement) in [
        ("active", json!(false)),
        ("policy_revision", json!(2)),
        ("automation_grant_id", json!("other")),
        ("invocation_id", json!("other")),
        ("occurrence_id", json!("other")),
        ("expires_at", json!(now() - 1)),
    ] {
        let mut invalid = valid.clone();
        invalid[field] = replacement;
        assert!(authority::validate_response(&policy, &binding, &invalid).is_err());
    }
}
#[tokio::test]
async fn callback_uses_authenticated_exact_headers_and_rejects_redirects() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![0; 8192];
        let count = stream.read(&mut request).await.unwrap();
        let request = String::from_utf8_lossy(&request[..count]).to_lowercase();
        assert!(request.contains("authorization: bearer synthetic-authority-token"));
        assert!(request.contains("x-upstream-maintenance-invocation: invocation-1"));
        assert!(request.contains("x-upstream-maintenance-occurrence: night-1"));
        stream
            .write_all(
                b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\n\r\n",
            )
            .await
            .unwrap();
    });
    let (_dir, _db, policy) = fixture();
    let binding = bind(&policy);
    assert!(
        authority::fetch(
            format!("http://{address}/authorization").parse().unwrap(),
            "synthetic-authority-token",
            &binding
        )
        .await
        .is_err()
    );
    server.await.unwrap();
}
#[test]
fn maintenance_rejects_unqualified_automatic_merge_without_changing_ordinary_settings() {
    let (_dir, db, policy) = fixture();
    install(&db, &policy);
    let mut settings = Settings::default();
    validate_settings(&db, &bind(&policy), &settings).unwrap();
    settings.delivery.enabled = true;
    settings.delivery.merge = true;
    settings.delivery.repository = policy.repository.clone();
    settings.delivery.base = policy.base.clone();
    assert!(validate_settings(&db, &bind(&policy), &settings).is_err());
}
#[test]
fn simultaneous_batches_cannot_each_spend_the_shared_ceiling() {
    let (_dir, db, policy) = fixture();
    install(&db, &policy);
    let task = task(&db, &policy);
    pin(&db, &task, &bind(&policy)).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let threads = (0..8)
        .map(|_| {
            let root = db.root.clone();
            let task = task.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let db = Store::open(&root).unwrap();
                barrier.wait();
                reserve_model(&db, &task, 40).is_ok()
            })
        })
        .collect::<Vec<_>>();
    let admitted = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .filter(|admitted| *admitted)
        .count();
    assert_eq!(admitted, 2);
    assert_eq!(inspection(&db, &task).unwrap()["usage"]["model_calls"], 2);
}
#[tokio::test]
async fn oversized_model_response_is_refused_before_its_tool_call_is_parsed() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let body=json!({"choices":[{"message":{"tool_calls":[{"id":"call","function":{"name":"command","arguments":"{}"}}]}}]}).to_string();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 4096];
        let _ = stream.read(&mut buffer).await.unwrap();
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(reply.as_bytes()).await.unwrap();
    });
    let (_dir, db, policy) = fixture();
    install(&db, &policy);
    let task = task(&db, &policy);
    pin(&db, &task, &bind(&policy)).unwrap();
    let response = reqwest::get(format!("http://{address}/response"))
        .await
        .unwrap();
    let error = crate::native_protocol::read_response_bounded(
        response,
        false,
        |_| panic!("no provider events may be processed"),
        |bytes| consume_output(&db, &task, bytes),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("budget exhausted"));
    server.await.unwrap();
}
