use super::{Fixture, METHOD, counts, old_atomic_submit};
use horde::protocol;
use serde_json::json;
use std::time::Duration;

// Reuses the already-qualified Fixture/old_atomic_submit/real skill pack.
fn coverage_gap_task(f: &Fixture, project: &str) -> String {
    let request = f.request(project);
    old_atomic_submit(&f.db, &f.stable_submit(&request))
        .expect("native task fixture admission")["id"]
        .as_str()
        .expect("native task identity")
        .to_owned()
}

#[test]
fn coverage_gap_contended_migration_returns_busy_without_partial_schema() {
    let f = Fixture::new();
    let contender = rusqlite::Connection::open(f.db.root.join("state.sqlite3")).unwrap();
    contender.busy_timeout(Duration::ZERO).unwrap();
    let schema =
        f.db.rows(
            "SELECT type,name,sql FROM sqlite_master ORDER BY type,name",
            &[],
        )
        .unwrap();
    let before = counts(&f.db);
    f.db.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let error = horde::submission::migrate(&contender).unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<rusqlite::Error>()
            .and_then(rusqlite::Error::sqlite_error_code),
        Some(rusqlite::ErrorCode::DatabaseBusy)
    );
    assert!(
        contender.is_autocommit(),
        "failed migration left no partial transaction"
    );
    assert_eq!(
        contender
            .prepare("SELECT type,name,sql FROM sqlite_master ORDER BY type,name")
            .unwrap()
            .column_count(),
        3
    );
    assert_eq!(
        f.db.rows(
            "SELECT type,name,sql FROM sqlite_master ORDER BY type,name",
            &[]
        )
        .unwrap(),
        schema
    );
    assert_eq!(counts(&f.db), before);
    f.db.conn.execute_batch("ROLLBACK").unwrap();
    horde::submission::migrate(&contender)
        .expect("migration remains idempotent after writer releases");
}

fn coverage_gap_open_error(path: &std::path::Path) -> anyhow::Error {
    match horde::store::Store::open(path) {
        Ok(_) => panic!("invalid owned store fixture must fail closed"),
        Err(error) => error,
    }
}

#[test]
fn coverage_gap_store_open_preserves_non_directory_and_invalid_database_evidence() {
    let root = tempfile::tempdir().unwrap();
    let non_directory = root.path().join("not-a-directory");
    std::fs::write(&non_directory, b"owned sentinel").unwrap();
    let error = coverage_gap_open_error(&non_directory);
    let kind = error
        .downcast_ref::<std::io::Error>()
        .expect("filesystem error")
        .kind();
    assert!(matches!(
        kind,
        std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::NotADirectory
    ));
    assert_eq!(std::fs::read(&non_directory).unwrap(), b"owned sentinel");

    let directory_database = root.path().join("directory-database");
    std::fs::create_dir_all(directory_database.join("state.sqlite3")).unwrap();
    let error = coverage_gap_open_error(&directory_database);
    assert_eq!(
        error
            .downcast_ref::<rusqlite::Error>()
            .and_then(rusqlite::Error::sqlite_error_code),
        Some(rusqlite::ErrorCode::CannotOpen)
    );
    assert!(directory_database.join("state.sqlite3").is_dir());

    let corrupt = root.path().join("corrupt");
    std::fs::create_dir(&corrupt).unwrap();
    let bytes = b"this is preserved corrupt SQLite evidence, never an empty replacement";
    std::fs::write(corrupt.join("state.sqlite3"), bytes).unwrap();
    let error = coverage_gap_open_error(&corrupt);
    assert_eq!(
        error
            .downcast_ref::<rusqlite::Error>()
            .and_then(rusqlite::Error::sqlite_error_code),
        Some(rusqlite::ErrorCode::NotADatabase)
    );
    assert_eq!(std::fs::read(corrupt.join("state.sqlite3")).unwrap(), bytes);
}

#[test]
fn coverage_gap_mcp_control_envelopes_never_dispatch_private_settlement() {
    let calls = std::cell::Cell::new(0);
    for (method, expected_error) in [
        ("initialize", false),
        ("ping", false),
        ("unknown-method", true),
    ] {
        let reply =
            protocol::mcp_response(&json!({"jsonrpc":"2.0","id":7,"method":method}), |_, _| {
                calls.set(calls.get() + 1);
                anyhow::bail!("control requests must not call the tool dispatcher")
            })
            .expect("JSON-RPC response");
        assert_eq!(reply["id"], 7);
        assert_eq!(reply.get("error").is_some(), expected_error);
        if method == "initialize" {
            assert_eq!(reply["result"]["protocolVersion"], "2024-11-05");
            assert_eq!(
                reply["result"]["serverInfo"]["version"],
                env!("CARGO_PKG_VERSION")
            );
        }
        if expected_error {
            assert_eq!(reply["error"]["code"], -32601);
        }
    }
    let denied = protocol::mcp_response(&json!({"jsonrpc":"2.0","id":"private","method":"tools/call","params":{"name":METHOD,"arguments":{}}}), |_, _| {
        calls.set(calls.get() + 1);
        anyhow::bail!("private settlement reached model dispatcher")
    }).unwrap();
    assert_eq!(denied["id"], "private");
    assert_eq!(denied["error"]["code"], -32601);
    assert!(
        denied["error"]["message"]
            .as_str()
            .unwrap()
            .contains("private operator/service")
    );
    assert_eq!(
        calls.get(),
        0,
        "no control/private request dispatches any operation"
    );
    assert!(
        protocol::mcp_response(&json!({"method":"ping"}), |_, _| unreachable!(
            "notification dispatch"
        ))
        .is_none()
    );
}

#[test]
fn coverage_gap_binding_receipt_matches_original_canonical_sorted_wire_bytes() {
    let f = Fixture::new();
    let request = f.request("default");
    let fields: std::collections::BTreeMap<String, serde_json::Value> =
        serde_json::from_value(request["system_binding"].clone()).unwrap();
    assert_eq!(fields.len(), 10);
    let canonical_bytes = serde_json::to_vec(&fields).unwrap();
    assert_eq!(
        std::str::from_utf8(&canonical_bytes).unwrap(),
        r#"{"action":"build","invocation_id":"00000000-0000-4000-8000-000000000006","objective_sha256":"4eb568f9b9b6f75c1915c0c95decb85c3f973973131e372235d5d12944f4dc45","operation_id":"op-fixture-build","organization_id":"00000000-0000-4000-8000-000000000003","original_actor_id":"00000000-0000-4000-8000-000000000001","project_id":"00000000-0000-4000-8000-000000000004","request_hash":"97549faeb4ecede3ee98e949f4b5405caa503f5d53adf3ec4a7cbbb630514897","tenant_id":"default","thread_id":"00000000-0000-4000-8000-000000000005"}"#
    );
    assert_eq!(
        horde::store::hash(&canonical_bytes),
        "0f8c26a4693d6bfe0338f210d8fba0a9194465c08bda556e8bb8c39f1ad65022"
    );
    let before = counts(&f.db);
    let receipt = f.close(request.clone(), "default");
    assert_eq!(
        receipt["system_binding_hash"],
        horde::store::hash(&canonical_bytes)
    );
    assert_eq!(
        counts(&f.db),
        before,
        "canonical hash introduces no task admission"
    );
    assert_eq!(
        f.close(request, "default"),
        receipt,
        "wire identity remains immutable across retry"
    );
}

#[test]
fn coverage_gap_public_runtime_directory_routes_keep_removal_local() {
    let f = Fixture::new();
    f.db.conn.execute("INSERT INTO managed_runtimes(id,profile,spec,state,created) VALUES('ts-gap','test','{}','provisioned',?)", [horde::store::now()]).unwrap();
    let renamed = protocol::dispatch(
        &f.db,
        "runtime_rename",
        json!({"id":"ts-gap","name":"gap-worker"}),
        None,
    )
    .unwrap();
    assert_eq!(renamed["name"], "gap-worker");
    assert_eq!(
        horde::runtime_directory::resolve(&f.db, "gap-worker").unwrap(),
        "ts-gap"
    );
    let malformed = protocol::dispatch(
        &f.db,
        "runtime_rename",
        json!({"id":"ts-gap","name":[]}),
        None,
    )
    .unwrap_err();
    assert_eq!(malformed.to_string(), "missing string name");
    let removed =
        protocol::dispatch(&f.db, "runtime_forget", json!({"id":"ts-gap"}), None).unwrap();
    assert_eq!(removed["removed"], true);
    assert_eq!(removed["provider_resources_changed"], false);
    assert_eq!(
        f.db.rows("SELECT state FROM managed_runtimes WHERE id='ts-gap'", &[])
            .unwrap()[0]["state"],
        "removed"
    );
}

#[test]
fn coverage_gap_public_mail_acknowledges_only_the_owning_mailbox() {
    let f = Fixture::new();
    let task = coverage_gap_task(&f, "default");
    let sender = f.db.register(&task, None).unwrap();
    let receiver = f.db.register(&task, None).unwrap();
    protocol::dispatch(&f.db, "send_message", json!({"task":task,"worker":sender["id"],"id":"gap-message","destination":receiver["id"],"body":"bounded coordination","actionable":true}), sender["token"].as_str()).unwrap();
    let unread = protocol::dispatch(
        &f.db,
        "read_messages",
        json!({}),
        receiver["token"].as_str(),
    )
    .unwrap();
    assert_eq!(unread.as_array().unwrap().len(), 1);
    let malformed = protocol::dispatch(
        &f.db,
        "acknowledge_messages",
        json!({"ids":1}),
        receiver["token"].as_str(),
    )
    .unwrap_err();
    assert_eq!(malformed.to_string(), "ids must be a string array");
    let acknowledged = protocol::dispatch(
        &f.db,
        "acknowledge_messages",
        json!({"ids":["gap-message"]}),
        receiver["token"].as_str(),
    )
    .unwrap();
    assert_eq!(acknowledged["acknowledged"], true);
    assert_eq!(
        protocol::dispatch(
            &f.db,
            "read_messages",
            json!({}),
            receiver["token"].as_str()
        )
        .unwrap(),
        json!([])
    );
    let before =
        f.db.rows(
            "SELECT message,worker,ack FROM receipts ORDER BY worker,message",
            &[],
        )
        .unwrap();
    let error = protocol::dispatch(
        &f.db,
        "acknowledge_messages",
        json!({"ids":["gap-message"]}),
        sender["token"].as_str(),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "message not in worker mailbox");
    assert_eq!(
        f.db.rows(
            "SELECT message,worker,ack FROM receipts ORDER BY worker,message",
            &[]
        )
        .unwrap(),
        before
    );
}

#[test]
fn coverage_gap_public_claim_transfer_and_release_preserve_worker_ownership() {
    let f = Fixture::new();
    let task = coverage_gap_task(&f, "default");
    let first = f.db.register(&task, None).unwrap();
    let second = f.db.register(&task, None).unwrap();
    horde::git::allocate(&f.db, &task, first["id"].as_str().unwrap()).unwrap();
    horde::git::allocate(&f.db, &task, second["id"].as_str().unwrap()).unwrap();
    let malformed = protocol::dispatch(
        &f.db,
        "claim_paths",
        json!({"paths":1}),
        first["token"].as_str(),
    )
    .unwrap_err();
    assert_eq!(malformed.to_string(), "paths must be a string array");
    assert!(
        f.db.rows("SELECT * FROM claims WHERE task=?", &[&task])
            .unwrap()
            .is_empty()
    );
    protocol::dispatch(
        &f.db,
        "claim_paths",
        json!({"paths":["gap.rs"]}),
        first["token"].as_str(),
    )
    .unwrap();
    assert_eq!(
        f.db.rows(
            "SELECT worker FROM claims WHERE task=? AND path='gap.rs'",
            &[&task]
        )
        .unwrap()[0]["worker"],
        first["id"]
    );
    protocol::dispatch(
        &f.db,
        "transfer_claim",
        json!({"to":second["id"],"path":"gap.rs"}),
        first["token"].as_str(),
    )
    .unwrap();
    assert_eq!(
        f.db.rows(
            "SELECT worker FROM claims WHERE task=? AND path='gap.rs'",
            &[&task]
        )
        .unwrap()[0]["worker"],
        second["id"]
    );
    protocol::dispatch(
        &f.db,
        "set_worker_status",
        json!({"status":"working"}),
        second["token"].as_str(),
    )
    .unwrap();
    let forbidden =
        protocol::dispatch(&f.db, "release_claims", json!({}), second["token"].as_str())
            .unwrap_err();
    assert_eq!(
        forbidden.to_string(),
        "operation unavailable to worker credentials"
    );
    let error = protocol::dispatch(
        &f.db,
        "release_claims",
        json!({"task":task,"worker":second["id"]}),
        None,
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "stop or reconcile worker before releasing claims"
    );
    protocol::dispatch(
        &f.db,
        "set_worker_status",
        json!({"status":"idle"}),
        second["token"].as_str(),
    )
    .unwrap();
    let released = protocol::dispatch(
        &f.db,
        "release_claims",
        json!({"task":task,"worker":second["id"]}),
        None,
    )
    .unwrap();
    assert_eq!(released["released"], true);
    assert!(
        f.db.rows("SELECT * FROM claims WHERE task=?", &[&task])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn coverage_gap_public_refresh_bundles_preserves_atomic_pins_on_invalid_content() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let project = f.other_project();
    let task = coverage_gap_task(&f, &project);
    let root = horde::projects::storage_root(&f.db, &project).unwrap();
    let bundle = root.join("gap.env");
    std::fs::write(&bundle, b"TEST_VALUE=before\n").unwrap();
    std::fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(root.join("secrets.toml"), "[bundles]\ngap=\"gap.env\"\n").unwrap();
    horde::secrets::select(&f.db, &task, &["gap".to_owned()]).unwrap();
    std::fs::write(&bundle, b"TEST_VALUE=after\n").unwrap();
    let reply = protocol::dispatch_scoped(
        &f.db,
        "refresh_bundles",
        json!({"task":task}),
        None,
        Some(&project),
    )
    .unwrap();
    assert_eq!(reply["refreshed"], true);
    let pinned =
        f.db.rows(
            "SELECT name,version FROM task_bundles WHERE task=?",
            &[&task],
        )
        .unwrap();
    assert_eq!(
        pinned[0]["version"],
        horde::store::hash(b"TEST_VALUE=after\n")
    );
    std::fs::write(&bundle, b"not a KEY=VALUE line\n").unwrap();
    let error = protocol::dispatch_scoped(
        &f.db,
        "refresh_bundles",
        json!({"task":task}),
        None,
        Some(&project),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "invalid environment variable name");
    assert_eq!(
        f.db.rows(
            "SELECT name,version FROM task_bundles WHERE task=?",
            &[&task]
        )
        .unwrap(),
        pinned
    );
}

#[test]
fn coverage_gap_public_read_only_capabilities_and_preview_rejection_do_not_schedule() {
    let f = Fixture::new();
    let task = coverage_gap_task(&f, "default");
    let before = counts(&f.db);
    let inventory =
        protocol::dispatch(&f.db, "runtime_capabilities", json!({"task":task}), None).unwrap();
    assert_eq!(inventory["project"], "default");
    assert!(inventory["runtimes"].is_array());
    let error = protocol::dispatch(
        &f.db,
        "run_preview_retry",
        json!({"task":task,"expected_head":"not-used-before-key-validation","idempotency_key":"!"}),
        None,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "invalid preview retry key");
    for (arguments, expected) in [
        (
            json!({"task":task,"account":[],"idempotency_key":"bounded"}),
            "missing string account",
        ),
        (
            json!({"task":task,"account":"valid-account","idempotency_key":[]}),
            "missing string idempotency_key",
        ),
    ] {
        let error = protocol::dispatch(&f.db, "run_unpin_account", arguments, None).unwrap_err();
        assert_eq!(error.to_string(), expected);
    }
    assert_eq!(
        counts(&f.db),
        before,
        "read-only request and invalid retry admit no work"
    );
}

#[test]
fn coverage_gap_public_confirmation_denial_is_durable_and_never_starts_work() {
    let f = Fixture::new();
    let plan = horde::template::compile(
        "simulated",
        &horde::template::load_templates(&f.repo.join("absent")).unwrap(),
        std::collections::BTreeMap::from([("task".to_owned(), "confirmation".to_owned())]),
    )
    .unwrap();
    let settings = horde::config::Settings {
        autonomy: false,
        ..Default::default()
    };
    let task =
        f.db.submit_project("default", "confirmation", &f.repo, &settings, &plan)
            .unwrap();
    let question =
        f.db.rows(
            "SELECT id FROM questions WHERE task=? AND answer IS NULL",
            &[&task],
        )
        .unwrap()[0]["id"]
            .clone();
    assert_eq!(f.db.task(&task).unwrap()["status"], "waiting");
    protocol::dispatch(
        &f.db,
        "answer_question",
        json!({"task":task,"question":question,"answer":"no"}),
        None,
    )
    .unwrap();
    assert_eq!(f.db.task(&task).unwrap()["status"], "paused");
    let error = protocol::dispatch(
        &f.db,
        "answer_question",
        json!({"task":task,"question":question,"answer":"yes"}),
        None,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "unknown or answered question");
    assert_eq!(f.db.task(&task).unwrap()["status"], "paused");
    let after = counts(&f.db);
    assert_eq!(after["attempts"], 0);
    assert_eq!(after["workers"], 0);
    assert_eq!(after["external_ops"], 0);
}
