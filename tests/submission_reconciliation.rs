//! Native stable-request settlement contracts. These tests never start a scheduler or provider.
#[path = "submission_reconciliation/fixture.rs"]
mod fixture;

use fixture::{
    Fixture, METHOD, ORIGINAL_ACTOR, REPAIR_ACTOR, assert_negative, counts, fence_count,
    old_atomic_submit,
};
use horde::protocol;
use serde_json::json;
use std::sync::{Arc, Barrier, mpsc};
use std::time::Duration;

#[test]
fn absent_stable_request_is_terminally_fenced_without_task_admission() {
    let f = Fixture::new();
    let request = f.request("default");
    let before = counts(&f.db);
    let receipt = f.close(request.clone(), "default");
    assert_negative(&receipt, &request);
    assert_eq!(
        counts(&f.db),
        before,
        "closing uncertainty admits no task/step/attempt/worker"
    );
    assert!(
        horde::project_runtime::ordered_tasks(&f.second_store())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        fence_count(&f.db, "default", request["request_id"].as_str().unwrap()),
        1
    );
    let reopened = f.second_store();
    let repeated =
        protocol::dispatch_scoped(&reopened, METHOD, request, None, Some("default")).unwrap();
    assert_eq!(
        repeated, receipt,
        "native fence is durable across connections"
    );
}

#[test]
fn exact_retry_keeps_first_receipt_and_original_actor_key() {
    let f = Fixture::new();
    let request = f.request("default");
    assert_ne!(ORIGINAL_ACTOR, REPAIR_ACTOR);
    let receipt = f.close(request.clone(), "default");
    let mut retry = request.clone();
    retry["repair_actor_id"] = json!("00000000-0000-4000-8000-000000000007");
    retry["continuity_evidence_sha256"] = json!(horde::store::hash(
        b"fresh audit of the same configured route"
    ));
    assert_eq!(
        f.close(retry, "default"),
        receipt,
        "repair actor/audit changes never rebind original admission key"
    );
    let mut changed = request.clone();
    changed["system_binding"]["request_hash"] =
        json!(horde::store::hash(b"different System reservation"));
    assert!(protocol::dispatch_scoped(&f.db, METHOD, changed, None, Some("default")).is_err());
    assert_eq!(
        f.close(request, "default"),
        receipt,
        "conflicting binding cannot overwrite a fence"
    );
}

#[test]
fn fenced_request_rejects_native_retry_before_repository_discovery() {
    let f = Fixture::new();
    let request = f.request("default");
    f.close(request.clone(), "default");
    let before = counts(&f.db);
    let mut submit = f.stable_submit(&request);
    submit["repo"] = json!(f.root.path().join("must-not-be-inspected"));
    let error = protocol::dispatch(&f.db, "submit_task", submit, None)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("not admitted") || error.contains("fenced"),
        "terminal fence must precede repository lookup: {error}"
    );
    assert_eq!(counts(&f.db), before);
}

#[test]
fn fence_first_rolls_back_the_delayed_old_stable_transaction() {
    let f = Fixture::new();
    let request = f.request("default");
    let delayed_copy = f.stable_submit(&request);
    let barrier = Arc::new(Barrier::new(2));
    let ready = Arc::clone(&barrier);
    let db = f.second_store();
    let worker = std::thread::spawn(move || {
        ready.wait();
        old_atomic_submit(&db, &delayed_copy)
    });
    let result = protocol::dispatch_scoped(&f.db, METHOD, request.clone(), None, Some("default"));
    let before_delayed_submit = counts(&f.db);
    barrier.wait();
    let delayed = worker.join().expect("delayed old transaction thread");
    let receipt = result.expect("terminal native fence must exist before releasing delayed copy");
    assert_negative(&receipt, &request);
    let delayed_error =
        delayed.expect_err("receipt insertion guard must reject the old atomic path");
    assert_eq!(
        delayed_error.to_string(),
        "submission request is terminally fenced as not admitted",
        "the old atomic path must reach the native receipt-insertion fence, not fail during setup"
    );
    let after = counts(&f.db);
    eprintln!(
        "delayed old transaction counts tasks={} attempts={} workers={}",
        after["tasks"], after["attempts"], after["workers"]
    );
    assert_eq!(
        after, before_delayed_submit,
        "all native task, context, skill ownership and event rows must roll back to the post-fence snapshot"
    );
    assert!(
        horde::project_runtime::ordered_tasks(&f.second_store())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn positive_first_is_invisible_before_commit_and_can_never_be_fenced_negative() {
    let f = Fixture::new();
    let request = f.request("default");
    let submit = f.stable_submit(&request);
    let db = f.second_store();
    let barrier = Arc::new(Barrier::new(2));
    let release = Arc::clone(&barrier);
    let (send, receive) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        db.atomic(|| {
            let positive = old_atomic_submit(&db, &submit)?;
            send.send(()).expect("staged positive notification");
            release.wait();
            Ok(positive)
        })
    });
    receive
        .recv_timeout(Duration::from_secs(5))
        .expect("positive admission must reach its real uncommitted transaction");
    assert!(
        horde::project_runtime::ordered_tasks(&f.db)
            .unwrap()
            .is_empty(),
        "scheduler connection cannot see uncommitted task"
    );
    assert_eq!(counts(&f.db)["attempts"], 0);
    f.db.conn.busy_timeout(Duration::from_millis(30)).unwrap();
    let held = protocol::dispatch_scoped(&f.db, METHOD, request.clone(), None, Some("default"));
    barrier.wait();
    let positive = worker
        .join()
        .expect("positive thread")
        .expect("positive native commit");
    let error = held
        .expect_err("held write transaction cannot become a negative absence receipt")
        .to_string();
    assert!(
        error.contains("locked") || error.contains("busy"),
        "actual write-lock hold required: {error}"
    );
    let receipt = f.close(request.clone(), "default");
    assert_eq!(receipt["state"], "admitted");
    assert_eq!(receipt["admission_fenced"], false);
    assert_eq!(receipt["task_id"], positive["id"]);
    assert_eq!(receipt["receipt"], positive);
    assert_eq!(
        horde::project_runtime::ordered_tasks(&f.db).unwrap().len(),
        1,
        "committed native task is actually scheduler-visible"
    );
    assert_eq!(
        fence_count(&f.db, "default", request["request_id"].as_str().unwrap()),
        0
    );
}

#[test]
fn cancelled_or_completed_admission_is_not_reclassified_as_no_effect() {
    for status in ["cancelled", "succeeded"] {
        let f = Fixture::new();
        let request = f.request("default");
        let positive = old_atomic_submit(&f.db, &f.stable_submit(&request)).unwrap();
        f.db.conn
            .execute(
                "UPDATE tasks SET status=? WHERE id=?",
                [status, positive["id"].as_str().unwrap()],
            )
            .unwrap();
        let before = counts(&f.db);
        let receipt = f.close(request.clone(), "default");
        assert_eq!(
            receipt["state"], "admitted",
            "historical status {status} is not non-admission"
        );
        assert_eq!(receipt["task_id"], positive["id"]);
        assert_eq!(receipt["receipt"], positive);
        assert_eq!(counts(&f.db), before);
        assert_eq!(
            fence_count(&f.db, "default", request["request_id"].as_str().unwrap()),
            0
        );
    }
}

#[test]
fn same_request_in_other_project_does_not_disclose_or_block_its_namespace() {
    let f = Fixture::new();
    let other = f.other_project();
    let request = f.request("default");
    let mut foreign_submit = f.stable_submit(&request);
    foreign_submit["project"] = json!(other);
    let positive = old_atomic_submit(&f.db, &foreign_submit).unwrap();
    let receipt = f.close(request.clone(), "default");
    assert_negative(&receipt, &request);
    assert_ne!(receipt.get("task_id"), positive.get("id"));
    let mut foreign = request;
    foreign["project"] = json!(other);
    assert!(protocol::dispatch_scoped(&f.db, METHOD, foreign, None, Some("default")).is_err());
}

#[test]
fn legacy_default_receipt_is_admitted_but_not_leaked_to_other_project() {
    let f = Fixture::new();
    let request = f.request("default");
    let positive = old_atomic_submit(&f.db, &f.stable_submit(&request)).unwrap();
    f.db.conn.execute("INSERT INTO submission_receipts SELECT request_id,request_hash,task,response FROM project_submission_receipts WHERE project='default'", []).unwrap();
    f.db.conn
        .execute(
            "DELETE FROM project_submission_receipts WHERE project='default'",
            [],
        )
        .unwrap();
    let receipt = f.close(request.clone(), "default");
    assert_eq!(receipt["state"], "admitted");
    assert_eq!(receipt["task_id"], positive["id"]);
    let other = f.other_project();
    let foreign = f.request(&other);
    assert_negative(&f.close(foreign.clone(), &other), &foreign);
}

#[test]
fn invalid_original_key_and_runtime_overrides_never_create_a_fence() {
    let f = Fixture::new();
    let request = f.request("default");
    let mut wrong_key = request.clone();
    wrong_key["system_binding"]["original_actor_id"] = json!(REPAIR_ACTOR);
    let mut wrong_action = request.clone();
    wrong_action["system_binding"]["action"] = json!("retry");
    let mut extra_binding = request.clone();
    extra_binding["system_binding"]["budget"] = json!({"max_model_calls":999});
    let mut cases = vec![wrong_key, wrong_action, extra_binding];
    for key in ["repo", "on", "execution", "expected_store_id", "approved"] {
        let mut extra = request.clone();
        extra[key] = json!("caller override");
        cases.push(extra);
    }
    for invalid in cases {
        assert!(protocol::dispatch_scoped(&f.db, METHOD, invalid, None, Some("default")).is_err());
    }
    assert_negative(&f.close(request.clone(), "default"), &request);
    assert_eq!(
        fence_count(&f.db, "default", request["request_id"].as_str().unwrap()),
        1
    );
}

#[test]
fn worker_credentials_cannot_close_submission_uncertainty() {
    let f = Fixture::new();
    let request = f.request("default");
    assert!(!protocol::worker_allowed(METHOD));
    let listed = protocol::mcp_response(
        &json!({"jsonrpc":"2.0", "id":1, "method":"tools/list"}),
        |_, _| panic!("tool discovery must not dispatch a native operation"),
    )
    .expect("public MCP discovery response");
    assert!(
        listed["result"]["tools"]
            .as_array()
            .expect("public MCP tool catalog")
            .iter()
            .all(|tool| tool["name"] != METHOD),
        "settlement-only operator API must not be advertised to models"
    );
    assert!(
        protocol::dispatch_scoped(
            &f.db,
            METHOD,
            request.clone(),
            Some("fixture-worker-token"),
            Some("default")
        )
        .is_err()
    );
    assert_negative(&f.close(request.clone(), "default"), &request);
}

#[path = "submission_reconciliation/coverage_gaps.rs"]
mod coverage_gaps;
