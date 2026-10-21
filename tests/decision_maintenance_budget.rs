//! Shared maintenance admission for real HTTP attempts, using loopback mocks only.
use horde::{
    config::{Decision, DecisionMode, Settings},
    decision::DecisionHttpClient,
    store::Store,
};
use std::collections::BTreeMap;
#[path = "support/maintenance.rs"]
mod maintenance;
#[allow(dead_code)]
#[path = "support/decisions.rs"]
mod support;

async fn fixture(
    url: String,
    calls: u64,
    output: u64,
) -> (
    tempfile::TempDir,
    Store,
    String,
    Decision,
    support::OperatorConfig,
) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "fixture@localhost"],
        vec!["commit", "--allow-empty", "-m", "base"],
    ] {
        horde::git::run(&repo, &args).unwrap();
    }
    let db = Store::open(&dir.path().join("state")).unwrap();
    let plan = horde::template::compile(
        "simulated",
        &horde::template::load_templates(std::path::Path::new("absent")).unwrap(),
        BTreeMap::from([("task".into(), "budget fixture".into())]),
    )
    .unwrap();
    let task = db
        .submit("budget fixture", &repo, &Settings::default(), &plan)
        .unwrap();
    let decision = Decision {
        mode: DecisionMode::Shadow,
        backend: "tuara".into(),
        base_url: url,
        api_key_env: "HORDE_SHARED_DECISION_BUDGET_FIXTURE_KEY".into(),
        ..Default::default()
    };
    unsafe { std::env::set_var(&decision.api_key_env, "fixture-decision-key") };
    let config = support::OperatorConfig::install(&decision);
    maintenance::bind(&db, &task, &repo, calls, 100_000, output)
        .await
        .unwrap();
    (dir, db, task, decision, config)
}

#[tokio::test]
async fn decision_success_charges_canonical_input_and_actual_output_then_fences_extra_call() {
    let _lock = support::CONFIG_LOCK.lock().await;
    let body = support::response_json();
    let (url, mut requests) = support::server(vec![("200 OK", body, 0)]).await;
    let (_dir, db, task, decision, _config) = fixture(url, 1, 100_000).await;
    let client = DecisionHttpClient::new_project(decision, &db.root, &task).unwrap();
    let request = support::request();
    let (_, attempts) = client.decide_counted(&request).await.unwrap();
    assert_eq!(attempts, 1);
    assert!(requests.recv().await.is_some());
    let state = horde::upstream_maintenance::inspection(&db, &task).unwrap();
    assert_eq!(state["usage"]["model_calls"], 1);
    assert_eq!(
        state["usage"]["input_bytes"],
        serde_json::to_vec(&horde::decision::wire_request(&request))
            .unwrap()
            .len()
    );
    assert_eq!(state["usage"]["output_bytes"], body.len());
    let failure = client.decide_counted(&request).await.unwrap_err();
    assert_eq!(failure.attempts, 0);
    assert!(
        failure
            .to_string()
            .contains("maintenance worker execution budget exhausted")
    );
    assert!(requests.try_recv().is_err());
    assert_eq!(
        horde::upstream_maintenance::inspection(&db, &task).unwrap()["usage"]["fenced"],
        true
    );
}

#[tokio::test]
async fn every_decision_retry_reserves_shared_budget_before_another_http_attempt() {
    let _lock = support::CONFIG_LOCK.lock().await;
    let (url, mut requests) = support::server(vec![
        ("429 Too Many Requests", "{}", 0),
        ("200 OK", support::response_json(), 0),
    ])
    .await;
    let (_dir, db, task, decision, _config) = fixture(url, 1, 100_000).await;
    let failure = DecisionHttpClient::new_project(decision, &db.root, &task)
        .unwrap()
        .decide_counted(&support::request())
        .await
        .unwrap_err();
    assert!(requests.recv().await.is_some());
    assert!(
        requests.try_recv().is_err(),
        "retry escaped shared reservation"
    );
    assert_eq!(failure.attempts, 1);
    assert!(
        failure
            .to_string()
            .contains("maintenance worker execution budget exhausted")
    );
    let state = horde::upstream_maintenance::inspection(&db, &task).unwrap();
    assert_eq!(state["usage"]["model_calls"], 1);
    assert_eq!(state["usage"]["fenced"], true);
}

#[tokio::test]
async fn oversized_decision_output_cannot_be_returned_and_persists_the_shared_fence() {
    let _lock = support::CONFIG_LOCK.lock().await;
    let (url, mut requests) = support::server(vec![("200 OK", support::response_json(), 0)]).await;
    let (_dir, db, task, decision, _config) = fixture(url, 2, 1).await;
    let failure = DecisionHttpClient::new_project(decision, &db.root, &task)
        .unwrap()
        .decide_counted(&support::request())
        .await
        .unwrap_err();
    assert!(requests.recv().await.is_some());
    assert_eq!(failure.attempts, 1);
    assert!(
        failure
            .to_string()
            .contains("maintenance worker execution budget exhausted")
    );
    let reopened = Store::open(&db.root).unwrap();
    assert!(horde::upstream_maintenance::check_local(&reopened, &task).is_err());
    assert_eq!(
        horde::upstream_maintenance::inspection(&reopened, &task).unwrap()["usage"]["fenced"],
        true
    );
}
