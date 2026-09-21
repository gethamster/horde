use horde::{
    config::{CapabilityGuidance, Decision, DecisionMode, Settings},
    decision::validate_response,
    store::Store,
};
use serde_json::json;
use std::collections::BTreeMap;
#[path = "support/decisions.rs"]
mod support;
use support::*;

#[test]
fn fresh_decision_model_defaults_to_unconfigured_tuara() {
    let fresh = Decision::default();
    assert_eq!(fresh.backend, "tuara");
    assert_eq!(fresh.model, "jev-1.13.0");
    assert_eq!(fresh.protocol, "systemone-v1");
    assert!(fresh.base_url.is_empty());
    assert!(fresh.api_key_env.is_empty());
    fresh.validate().unwrap();
    let enabled = Decision {
        mode: DecisionMode::Shadow,
        ..fresh
    };
    assert!(
        enabled
            .validate()
            .unwrap_err()
            .to_string()
            .contains("tuara needs a configured")
    );
}

#[test]
fn legacy_typesafe_settings_load_but_custom_endpoints_require_explicit_provider() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(&path, "[decision]\nmode='shadow'\n").unwrap();
    let legacy = Settings::load_dir(directory.path()).unwrap();
    assert_eq!(legacy.decision.backend, "typesafe");
    assert_eq!(legacy.decision.base_url, "https://api.typesafe.ai");
    assert_eq!(legacy.decision.api_key_env, "TYPESAFE_API_KEY");
    std::fs::write(
        &path,
        "[decision]\nmode='shadow'\nbase_url='https://other.example'\n",
    )
    .unwrap();
    let error = Settings::load_dir(directory.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("backend must be explicit"), "{error}");
    std::fs::write(&path, "[decision]\nmode='shadow'\nbackend='tuara'\nbase_url='https://other.example'\napi_key_env='OTHER_KEY'\n").unwrap();
    let generic = Settings::load_dir(directory.path()).unwrap();
    assert_eq!(generic.decision.backend, "tuara");
    assert_eq!(generic.decision.api_key_env, "OTHER_KEY");
}

#[test]
fn service_identity_binds_provider_endpoint_protocol_and_model() {
    let base = Decision {
        mode: DecisionMode::Shadow,
        backend: "typesafe".into(),
        base_url: "https://api.typesafe.ai/".into(),
        api_key_env: "TYPESAFE_API_KEY".into(),
        ..Default::default()
    };
    base.validate().unwrap();
    assert_eq!(
        base.fingerprint().unwrap(),
        Decision {
            base_url: "https://api.typesafe.ai".into(),
            ..base.clone()
        }
        .fingerprint()
        .unwrap()
    );
    for changed in [
        Decision {
            backend: "tuara".into(),
            ..base.clone()
        },
        Decision {
            base_url: "https://other.example".into(),
            ..base.clone()
        },
        Decision {
            api_key_env: "OTHER_KEY".into(),
            ..base.clone()
        },
        Decision {
            model: "next-model.1".into(),
            ..base.clone()
        },
        Decision {
            protocol: "systemone-v2".into(),
            ..base.clone()
        },
    ] {
        assert_ne!(base.fingerprint().unwrap(), changed.fingerprint().unwrap());
    }
    let incompatible = Decision {
        protocol: "unknown".into(),
        ..base
    };
    assert!(incompatible.validate().is_err());
}

#[test]
fn generic_request_accepts_a_pinned_nonlaunch_model_and_rejects_a_stale_reply() {
    let mut request = request();
    request.model = "later-model.2".into();
    request.validate().unwrap();
    let mut response: serde_json::Value = serde_json::from_str(response_json()).unwrap();
    assert!(validate_response(&request, &response).is_err());
    response["model"] = json!("later-model.2");
    assert!(validate_response(&request, &response).is_ok());
}

#[tokio::test]
async fn explicit_tuara_compatible_mock_supports_operator_opted_in_actions() {
    let _lock = CONFIG_LOCK.lock().await;
    let (base_url, mut bodies) = server(vec![("200 OK", response_json(), 0)]).await;
    let key = format!("HORDE_DECISION_GENERIC_KEY_{}", std::process::id());
    unsafe { std::env::set_var(&key, "mock-generic-key") };
    let decision = Decision {
        mode: DecisionMode::Shadow,
        backend: "tuara".into(),
        base_url,
        api_key_env: key.clone(),
        ..Default::default()
    };
    let _operator = OperatorConfig::install(&decision);
    let client = horde::decision::DecisionHttpClient::new(decision.clone()).unwrap();
    client.decide_counted(&request()).await.unwrap();
    assert!(bodies.recv().await.is_some());
    let active = Decision {
        native_context_mode: horde::config::NativeContextMode::Active,
        ..decision.clone()
    };
    active.validate().unwrap();
    let browser_active = Decision {
        browser_test_mode: horde::config::BrowserTestMode::Active,
        ..decision
    };
    browser_active.validate().unwrap();
    let incompatible = Decision {
        protocol: "unsupported-v2".into(),
        ..browser_active
    };
    assert!(incompatible.validate().is_err());
    unsafe { std::env::remove_var(key) };
}

#[tokio::test]
async fn typesafe_uses_the_official_wire_shape_and_retries_only_retryable_statuses() {
    let _lock = CONFIG_LOCK.lock().await;
    let (base_url, mut bodies) = server(vec![
        ("429 Too Many Requests", "{}", 0),
        ("200 OK", response_json(), 0),
        ("200 OK", response_json(), 0),
    ])
    .await;
    let key = format!("HORDE_DECISION_TEST_KEY_{}", std::process::id());
    unsafe { std::env::set_var(&key, "test-secret") };
    let decision = Decision {
        mode: DecisionMode::Shadow,
        base_url,
        api_key_env: key.clone(),
        deadline_ms: 2_000,
        max_attempts: 2,
        ..Decision::default()
    };
    let _operator = OperatorConfig::install(&decision);
    let backend = horde::decision::typesafe::TypeSafe::new(decision).unwrap();
    let (_, attempts) = backend.decide_counted(&request()).await.unwrap();
    assert_eq!(attempts, 2);
    let wire: serde_json::Value = serde_json::from_slice(&bodies.recv().await.unwrap()).unwrap();
    assert!(wire["questions"].is_object());
    assert_eq!(wire["questions"]["route"]["type"], "choice");
    assert_eq!(wire["questions"]["difficulty"]["type"], "score");
    assert_eq!(wire["questions"]["security"]["type"], "noul");
    assert!(wire["questions"]["security"].get("criteria").is_none());
    assert_eq!(
        wire["questions"]["route"]["instructions"],
        "Which eligible capability should run this work?"
    );
    assert_eq!(
        wire["questions"]["route"]["criteria"]["local/codex"],
        serde_json::Value::Null
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bodies.recv().await.unwrap()).unwrap(),
        wire
    );
    horde::decision::DecisionBackend::decide(&backend, &request())
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bodies.recv().await.unwrap()).unwrap(),
        wire
    );
    unsafe { std::env::remove_var(key) };
}

#[tokio::test]
async fn typesafe_enforces_the_absolute_deadline() {
    let _lock = CONFIG_LOCK.lock().await;
    let (base_url, _bodies) = server(vec![("200 OK", response_json(), 400)]).await;
    let key = format!("HORDE_DECISION_DEADLINE_KEY_{}", std::process::id());
    unsafe { std::env::set_var(&key, "test-secret") };
    let decision = Decision {
        mode: DecisionMode::Shadow,
        base_url,
        api_key_env: key.clone(),
        deadline_ms: 100,
        max_attempts: 1,
        ..Decision::default()
    };
    let _operator = OperatorConfig::install(&decision);
    let backend = horde::decision::typesafe::TypeSafe::new(decision).unwrap();
    let started = std::time::Instant::now();
    let error = backend
        .decide_counted(&request())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("deadline"), "{error}");
    assert!(started.elapsed() < std::time::Duration::from_millis(300));
    unsafe { std::env::remove_var(key) };
}

#[test]
fn validates_a_mixed_response_and_rejects_unoffered_or_inconsistent_answers() {
    let input = request();
    let response = json!({
        "model":"jev-1.13.0",
        "answers": {
            "route":{"type":"choice","choice":"local/codex","confidence":0.7,
             "probabilities":{"local/codex":0.7,"local/claude":0.2,"abstain":0.1}},
            "difficulty":{"type":"score","score":1.3,"confidence":0.6,"legend":["routine","moderate","complex"],
             "probabilities":{"0":0.1,"1":0.5,"2":0.4}},
            "security":{"type":"noul","noul":0.25,"confidence":0.8}
        },
        "usage":{"input_tokens":21,"output_tokens":3},
        "provider_metadata":{"ignored":true}
    });
    let result = validate_response(&input, &response).unwrap();
    assert_eq!(result.answers.len(), 3);
    assert_eq!(result.usage.input_tokens, Some(21));

    let mut invalid = response.clone();
    invalid["answers"]["route"]["choice"] = json!("not-offered");
    assert!(validate_response(&input, &invalid).is_err());

    let mut invalid = response;
    invalid["answers"]["difficulty"]["score"] = json!(0.1);
    assert!(validate_response(&input, &invalid).is_err());
}

#[test]
fn rejects_bad_probabilities_ids_types_and_request_limits() {
    let input = request();
    let base = json!({"model":"jev-1.13.0","answers":{
        "route":{"type":"choice","choice":"local/codex","confidence":0.7,"probabilities":{"local/codex":0.7,"local/claude":0.2,"abstain":0.1}},
        "difficulty":{"type":"score","score":1.3,"confidence":0.6,"legend":["routine","moderate","complex"],"probabilities":{"0":0.1,"1":0.5,"2":0.4}},
        "security":{"type":"noul","noul":0.25}
    }});
    let mut invalid_probability = base.clone();
    invalid_probability["answers"]["route"]["probabilities"]["local/codex"] = json!(1.7);
    assert!(validate_response(&input, &invalid_probability).is_err());
    let mut wrong_type = base.clone();
    wrong_type["answers"]["security"]["type"] = json!("choice");
    assert!(validate_response(&input, &wrong_type).is_err());
    let mut wrong_id = base;
    wrong_id["answers"]["other"] = wrong_id["answers"]["security"].take();
    assert!(validate_response(&input, &wrong_id).is_err());

    let mut oversized = request();
    oversized.state = json!("x".repeat(60 * 1024));
    assert!(oversized.validate().is_err());
}

#[test]
fn decision_configuration_is_disabled_by_default_and_repository_cannot_enable_it() {
    assert_eq!(Settings::default().decision.mode, DecisionMode::Disabled);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".horde.toml"),
        "[decision]\nmode='shadow'\napi_key_env='SHOULD_NOT_BE_READ'\n",
    )
    .unwrap();
    let error = Settings::load(dir.path()).unwrap_err().to_string();
    assert!(error.contains("operator-only"), "{error}");

    let operator = tempfile::tempdir().unwrap();
    std::fs::write(
        operator.path().join("config.toml"),
        "[decision]\nmode='shadow'\napi_key_env='JEV_KEY'\n[[decision.capability_guidance]]\nruntime='local'\ncapability='codex'\ndescription='Use for Rust changes.'\n",
    )
    .unwrap();
    let loaded = Settings::load_dir(operator.path()).unwrap();
    assert_eq!(loaded.decision.mode, DecisionMode::Shadow);
    assert_eq!(loaded.decision.model, "jev-1.13.0");
    let serialized = serde_json::to_string(&loaded).unwrap();
    assert!(serialized.contains("JEV_KEY"));
    assert!(!serialized.contains("SHOULD_NOT_BE_READ"));
}

#[test]
fn validates_decision_configuration_ranges_and_endpoint_policy() {
    let valid = Decision {
        mode: DecisionMode::Shadow,
        backend: "typesafe".into(),
        base_url: "https://api.typesafe.ai".into(),
        api_key_env: "JEV_KEY".into(),
        ..Decision::default()
    };
    valid.validate().unwrap();
    for invalid in [
        Decision {
            deadline_ms: 0,
            ..valid.clone()
        },
        Decision {
            max_attempts: 0,
            ..valid.clone()
        },
        Decision {
            max_decisions_per_task: 0,
            ..valid.clone()
        },
        Decision {
            base_url: "http://example.com".into(),
            ..valid.clone()
        },
        Decision {
            base_url: "https://user@example.com?q=secret".into(),
            ..valid
        },
    ] {
        assert!(invalid.validate().is_err());
    }
}

#[test]
fn endpoint_request_and_safe_error_boundaries_are_enforced() {
    let endpoint = horde::decision::typesafe::endpoint("https://api.typesafe.ai/base/").unwrap();
    assert_eq!(
        endpoint.as_str(),
        "https://api.typesafe.ai/base/v1/systemone"
    );
    for invalid in [
        "relative",
        "ftp://127.0.0.1",
        "http://localhost:8080",
        "https://user:pass@example.com",
        "https://example.com/#fragment",
    ] {
        assert!(horde::decision::typesafe::endpoint(invalid).is_err());
    }
    let long = anyhow::anyhow!("{}suffix", "é".repeat(600));
    let safe = horde::decision::safe_error(&long);
    assert_eq!(safe.chars().count(), 512);
    assert!(safe.ends_with('…'));

    let mut invalid = request();
    invalid.state = json!(42);
    assert!(invalid.validate().is_err());
    let mut duplicate = request();
    duplicate.questions.push(duplicate.questions[0].clone());
    assert!(duplicate.validate().is_err());
}

#[tokio::test]
async fn typesafe_does_not_retry_auth_failures_and_bounds_response_bytes() {
    let _lock = CONFIG_LOCK.lock().await;
    let key = format!("HORDE_DECISION_BOUNDARY_KEY_{}", std::process::id());
    unsafe { std::env::set_var(&key, "boundary-secret") };
    let (unauthorized_url, mut unauthorized_bodies) =
        server(vec![("401 Unauthorized", "{}", 0)]).await;
    let unauthorized = Decision {
        mode: DecisionMode::Shadow,
        base_url: unauthorized_url,
        api_key_env: key.clone(),
        deadline_ms: 1_000,
        max_attempts: 3,
        ..Decision::default()
    };
    let operator = OperatorConfig::install(&unauthorized);
    let backend = horde::decision::typesafe::TypeSafe::new(unauthorized).unwrap();
    let mut credential_in_state = request();
    credential_in_state.state = json!({"objective":"boundary-secret"});
    let failure = backend
        .decide_counted(&credential_in_state)
        .await
        .unwrap_err();
    assert_eq!(failure.attempts, 0);
    assert!(failure.to_string().contains("active credential"));
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            unauthorized_bodies.recv()
        )
        .await
        .is_err()
    );
    let failure = backend.decide_counted(&request()).await.unwrap_err();
    assert_eq!(failure.attempts, 1);
    let _ = std::error::Error::source(&failure);
    unauthorized_bodies.recv().await.unwrap();

    let oversized: &'static str = Box::leak("x".repeat(256 * 1024 + 1).into_boxed_str());
    let (oversized_url, _bodies) = server(vec![("200 OK", oversized, 0)]).await;
    let bounded = Decision {
        mode: DecisionMode::Shadow,
        base_url: oversized_url,
        api_key_env: key.clone(),
        deadline_ms: 2_000,
        max_attempts: 1,
        ..Decision::default()
    };
    operator.rewrite(&bounded);
    let failure = horde::decision::typesafe::TypeSafe::new(bounded)
        .unwrap()
        .decide_counted(&request())
        .await
        .unwrap_err();
    assert_eq!(failure.attempts, 1);
    assert!(failure.to_string().contains("size limit"));
    unsafe { std::env::remove_var(key) };
}

#[test]
fn additive_storage_is_paginated_cache_scoped_and_interrupts_running_rows() {
    let root = tempfile::tempdir().unwrap();
    let db = Store::open(root.path()).unwrap();
    db.conn.execute("DROP TABLE decisions", []).unwrap();
    db.conn.pragma_update(None, "user_version", 5).unwrap();
    drop(db);
    let db = Store::open(root.path()).unwrap();
    assert_eq!(
        db.conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='decisions'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    db.conn
        .execute(
            "INSERT INTO tasks VALUES('task','objective','.','running',?,'{}',0)",
            [serde_json::to_string(&Settings::default()).unwrap()],
        )
        .unwrap();
    horde::decision::store::enqueue(
        &db,
        &horde::decision::store::QueuedDecision {
            id: "decision-1".into(),
            task: "task".into(),
            step: None,
            attempt: None,
            purpose: "routing".into(),
            policy: "routing-v1".into(),
            backend: "typesafe".into(),
            model: "jev-1.13.0".into(),
            baseline: Some("local/codex".into()),
        },
    )
    .unwrap();
    drop(db);
    let db = Store::open(root.path()).unwrap();
    horde::decision::store::interrupt_running(&db.conn).unwrap();
    let rows = horde::decision::store::list(&db, "task", 0, 50).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["state"], "interrupted");
    assert!(
        horde::decision::store::cached(&db, "task", "cache")
            .unwrap()
            .is_none()
    );
    assert!(horde::decision::store::list(&db, "task", 0, 201).is_err());
}

#[test]
fn candidate_filtering_respects_policy_freshness_availability_and_operator_guidance() {
    let settings = Settings {
        decision: Decision {
            mode: DecisionMode::Shadow,
            capability_guidance: vec![
                CapabilityGuidance {
                    runtime: "local".into(),
                    capability: "codex".into(),
                    description: "Rust coding harness".into(),
                },
                CapabilityGuidance {
                    runtime: "remote".into(),
                    capability: "claude".into(),
                    description: "Remote review harness".into(),
                },
            ],
            ..Decision::default()
        },
        ..Settings::default()
    };
    let inventory = json!({"runtimes":[
        {"runtime":"controller","local":true,"fresh":true,"ready":true,"capacity":{"available":2,"draining":false},"capabilities":[
            {"id":"codex","available":null,"configuration_hash":"a".repeat(64)},
            {"id":"unguided","available":true,"configuration_hash":"b".repeat(64)}]},
        {"runtime":"remote","local":false,"fresh":false,"ready":true,"capacity":{"available":2,"draining":false},"capabilities":[
            {"id":"claude","available":true,"configuration_hash":"c".repeat(64)}]}
    ]});
    let policy = json!({"bindings":[
        {"runtime":"controller","capability":"codex","configuration_hash":"a".repeat(64)},
        {"runtime":"controller","capability":"unguided","configuration_hash":"b".repeat(64)},
        {"runtime":"remote","capability":"claude","configuration_hash":"c".repeat(64)}
    ],"selected":{"runtime":"controller","capability":"codex"}});
    let candidates =
        horde::decision::shadow::candidates(&inventory, Some(&policy), &settings, "worker")
            .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id, "controller/codex");
    assert_eq!(policy["selected"]["capability"], "codex");
}

#[test]
fn decision_inspection_and_metrics_are_bounded_and_keep_unknown_cost_null() {
    let root = tempfile::tempdir().unwrap();
    let db = Store::open(root.path()).unwrap();
    db.conn
        .execute(
            "INSERT INTO tasks VALUES('task','objective','.','running',?,'{}',0)",
            [serde_json::to_string(&Settings::default()).unwrap()],
        )
        .unwrap();
    let row = horde::decision::store::QueuedDecision {
        id: "done".into(),
        task: "task".into(),
        step: None,
        attempt: None,
        purpose: "routing".into(),
        policy: "routing-v1".into(),
        backend: "typesafe".into(),
        model: "jev-1.13.0".into(),
        baseline: Some("local/codex".into()),
    };
    horde::decision::store::enqueue(&db, &row).unwrap();
    horde::decision::store::start(
        &db,
        "done",
        &horde::decision::store::PreparedDecision {
            state_hash: "state".into(),
            context_version: 1,
            policy_hash: "policy".into(),
            catalog_hash: "catalog".into(),
            candidate_hashes: json!({"local/codex":"hash"}),
            backend_fingerprint: "backend".into(),
            evidence_hash: "evidence".into(),
            request_hash: "request".into(),
            cache_hash: "cache".into(),
            artifact_hash: None,
        },
    )
    .unwrap();
    horde::decision::store::complete(
        &db,
        "done",
        &horde::decision::store::CompletedDecision {
            result: &json!({"answer":true}),
            proposed: Some("local/claude"),
            abstention: false,
            provider_ms: 12,
            attempts: 2,
            usage: &json!({"input_tokens":10,"output_tokens":2}),
        },
    )
    .unwrap();
    let local = horde::decision::store::QueuedDecision {
        id: "local-abstention".into(),
        ..row.clone()
    };
    horde::decision::store::enqueue(&db, &local).unwrap();
    horde::decision::store::start(
        &db,
        "local-abstention",
        &horde::decision::store::PreparedDecision {
            state_hash: "local-state".into(),
            context_version: 1,
            policy_hash: "policy".into(),
            catalog_hash: "catalog".into(),
            candidate_hashes: json!({}),
            backend_fingerprint: "backend".into(),
            evidence_hash: "local-evidence".into(),
            request_hash: "local-request".into(),
            cache_hash: "local-cache".into(),
            artifact_hash: None,
        },
    )
    .unwrap();
    horde::decision::store::complete(
        &db,
        "local-abstention",
        &horde::decision::store::CompletedDecision {
            result: &json!({"abstained":true}),
            proposed: Some("abstain"),
            abstention: true,
            provider_ms: 0,
            attempts: 0,
            usage: &json!({}),
        },
    )
    .unwrap();
    let listed = horde::protocol::dispatch(
        &db,
        "decisions",
        json!({"task":"task","after":0,"limit":1}),
        None,
    )
    .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    let metrics = horde::metrics::report(&db, "task").unwrap();
    assert_eq!(metrics["decisions"]["provider_requests"], 2);
    assert_eq!(metrics["decisions"]["retries"], 1);
    assert_eq!(metrics["decisions"]["completed"], 2);
    assert_eq!(metrics["decisions"]["states"]["succeeded"], 2);
    assert_eq!(metrics["decisions"]["states"]["cached"], 0);
    assert_eq!(metrics["decisions"]["failed"], 0);
    assert_eq!(metrics["decisions"]["baseline_disagreements"], 1);
    assert_eq!(
        metrics["decisions"]["provider_latency_ms"]["samples"],
        json!([12])
    );
    assert!(metrics["decisions"]["reported_api_cost_usd"].is_null());
    assert_eq!(
        horde::protocol::admin_schema("decisions")["properties"]["limit"]["maximum"],
        200
    );
    assert!(!horde::protocol::worker_allowed("decisions"));
}

#[test]
fn versioned_offline_calibration_fixture_is_well_formed() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/decision-routing-v1.json")).unwrap();
    assert_eq!(fixture["version"], "routing-v1");
    assert!(
        fixture["calibration"]
            .as_array()
            .is_some_and(|cases| !cases.is_empty())
    );
}

#[tokio::test]
async fn operator_drift_between_retry_attempts_stops_before_a_second_post() {
    let _lock = CONFIG_LOCK.lock().await;
    let (base_url, mut bodies) = server(vec![
        ("429 Too Many Requests", "{}", 0),
        ("200 OK", response_json(), 0),
    ])
    .await;
    let key = format!("HORDE_DECISION_DRIFT_KEY_{}", std::process::id());
    unsafe { std::env::set_var(&key, "drift-secret") };
    let decision = Decision {
        mode: DecisionMode::Shadow,
        base_url,
        api_key_env: key.clone(),
        deadline_ms: 2_000,
        max_attempts: 2,
        ..Decision::default()
    };
    let operator = OperatorConfig::install(&decision);
    let backend = horde::decision::typesafe::TypeSafe::new(decision.clone()).unwrap();
    let call = tokio::spawn(async move { backend.decide_counted(&request()).await });
    bodies.recv().await.unwrap();
    operator.rewrite(&Decision {
        deadline_ms: 1_999,
        ..decision
    });
    let failure = call.await.unwrap().unwrap_err();
    assert_eq!(failure.attempts, 1);
    assert!(failure.to_string().contains("no longer authorizes"));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), bodies.recv())
            .await
            .is_err()
    );
    unsafe { std::env::remove_var(key) };
}

#[tokio::test(flavor = "current_thread")]
async fn missing_operator_guidance_abstains_without_a_provider_request() {
    let _lock = CONFIG_LOCK.lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut bodies) = server(vec![("200 OK", shadow_response(), 0)]).await;
            let key = format!("HORDE_DECISION_ABSTAIN_KEY_{}", std::process::id());
            unsafe { std::env::set_var(&key, "abstain-secret") };
            let decision = Decision {
                mode: DecisionMode::Shadow,
                base_url,
                api_key_env: key.clone(),
                max_decisions_per_task: 1,
                ..Decision::default()
            };
            let _operator = OperatorConfig::install(&decision);
            let data = tempfile::tempdir().unwrap();
            let repo = tempfile::tempdir().unwrap();
            let db = Store::open(data.path()).unwrap();
            let settings = Settings {
                decision: decision.clone(),
                ..Settings::default()
            };
            let plan = horde::template::compile(
                "simulated",
                &horde::template::load_templates(repo.path()).unwrap(),
                BTreeMap::from([("task".into(), "bounded work".into())]),
            )
            .unwrap();
            let task = db
                .submit("bounded work", repo.path(), &settings, &plan)
                .unwrap();
            let row = db.steps(&task).unwrap().remove(0);
            let mut queue = horde::decision::shadow::Queue::default();
            let first = horde::decision::shadow::Job::new(
                data.path().to_path_buf(),
                task.clone(),
                row.clone(),
                None,
                "simulated".into(),
                decision.clone(),
            );
            let first_id = first.id.clone();
            queue.enqueue(&db, first).unwrap();
            let overflow = horde::decision::shadow::Job::new(
                data.path().to_path_buf(),
                task.clone(),
                row,
                None,
                "simulated".into(),
                decision,
            );
            let overflow_id = overflow.id.clone();
            queue.enqueue(&db, overflow).unwrap();
            let admitted = horde::decision::store::list(&db, &task, 0, 50).unwrap();
            assert_eq!(
                admitted.iter().find(|row| row["id"] == first_id).unwrap()["state"],
                "queued"
            );
            let overflow = admitted
                .iter()
                .find(|row| row["id"] == overflow_id)
                .unwrap();
            assert_eq!(overflow["state"], "skipped");
            assert_eq!(overflow["error"], "task_decision_limit");
            drain(&mut queue, &db).await;
            let decisions = horde::decision::store::list(&db, &task, 0, 50).unwrap();
            let decision = decisions.iter().find(|row| row["id"] == first_id).unwrap();
            assert_eq!(decision["state"], "succeeded");
            assert_eq!(decision["proposed"], "abstain");
            assert_eq!(decision["abstention"], 1);
            assert_eq!(decision["provider_attempts"], 0);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(100), bodies.recv())
                    .await
                    .is_err()
            );
            unsafe { std::env::remove_var(key) };
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn shadow_queue_redacts_secrets_caches_without_usage_and_cancels_tracked_work() {
    let _lock = CONFIG_LOCK.lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let (base_url, mut bodies) = server(vec![
                ("200 OK", shadow_response(), 0),
                ("200 OK", shadow_response(), 0),
            ])
            .await;
            let key = format!("HORDE_DECISION_SHADOW_KEY_{}", std::process::id());
            let provider_secret = "provider-secret-do-not-store";
            let application_secret = "application-secret-do-not-send";
            unsafe { std::env::set_var(&key, provider_secret) };
            let decision = Decision {
                mode: DecisionMode::Shadow,
                base_url,
                api_key_env: key.clone(),
                capability_guidance: vec![CapabilityGuidance {
                    runtime: "local".into(),
                    capability: "simulated".into(),
                    description: format!(
                        "Deterministic {application_secret} {provider_secret} execution"
                    ),
                }],
                ..Decision::default()
            };
            let operator = OperatorConfig::install(&decision);
            let bundle = operator.directory.path().join("horde/bundle.env");
            std::fs::write(&bundle, format!("APP_SECRET={application_secret}\n")).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::fs::write(
                operator.directory.path().join("horde/secrets.toml"),
                "[bundles]\napp='bundle.env'\n",
            )
            .unwrap();

            let data = tempfile::tempdir().unwrap();
            let repo = tempfile::tempdir().unwrap();
            let db = Store::open(data.path()).unwrap();
            let mut settings = Settings {
                decision: decision.clone(),
                secret_bundles: vec!["app".into()],
                ..Settings::default()
            };
            settings.default_template = "simulated".into();
            let objective = format!("route {application_secret} and {provider_secret}");
            let plan = horde::template::compile(
                "simulated",
                &horde::template::load_templates(repo.path()).unwrap(),
                BTreeMap::from([("task".into(), objective.clone())]),
            )
            .unwrap();
            let task = db
                .submit(&objective, repo.path(), &settings, &plan)
                .unwrap();
            let row = db.steps(&task).unwrap().remove(0);
            horde::management::set(&db, "concurrency", "1").unwrap();
            let inventory = horde::capabilities::inventory(&db).unwrap();
            let runtime = inventory["runtimes"]
                .as_array()
                .unwrap()
                .iter()
                .find(|runtime| runtime["local"] == true)
                .unwrap();
            let runtime_id = runtime["runtime"].as_str().unwrap();
            let capability = runtime["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .find(|capability| capability["id"] == "simulated")
                .unwrap();
            let binding = json!({
                "runtime":runtime_id,
                "capability":"simulated",
                "provider":capability["provider"],
                "model":capability["model"],
                "kind":capability["kind"],
                "configuration_hash":capability["configuration_hash"],
                "operator_note":format!("{application_secret}:{provider_secret}"),
            });
            let policy = json!({
                "version":1,
                "allowed":[{"runtime":runtime_id,"capabilities":["simulated"]}],
                "bindings":[binding.clone()],
                "selected":binding,
                "metadata":format!("policy {application_secret} {provider_secret}"),
            });
            horde::execution_selection::pin(&db, &task, &policy).unwrap();
            let own_attempt = "decision-capacity-own-attempt";
            let step_id = row["id"].as_str().unwrap();
            db.conn
                .execute("UPDATE steps SET state='running' WHERE id=?", [step_id])
                .unwrap();
            db.conn
                .execute(
                    "INSERT INTO attempts(id,step,state,started) VALUES(?,?,'running',?)",
                    rusqlite::params![own_attempt, step_id, horde::store::now()],
                )
                .unwrap();
            let mut queue = horde::decision::shadow::Queue::default();
            let first = horde::decision::shadow::Job::new(
                data.path().to_path_buf(),
                task.clone(),
                row.clone(),
                Some(own_attempt.into()),
                "simulated".into(),
                decision.clone(),
            );
            queue.enqueue(&db, first).unwrap();
            let blocker = Store::open(data.path()).unwrap();
            blocker.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            let started = std::time::Instant::now();
            queue.tick(&db).await.unwrap();
            assert!(started.elapsed() < std::time::Duration::from_millis(50));
            blocker.conn.execute_batch("ROLLBACK").unwrap();
            drain(&mut queue, &db).await;
            let body = String::from_utf8(bodies.recv().await.unwrap()).unwrap();
            assert!(!body.contains(application_secret));
            assert!(!body.contains(provider_secret));
            assert!(body.contains("[REDACTED]"));
            assert!(body.contains(&format!("{runtime_id}/simulated")));

            let first_row = horde::decision::store::list(&db, &task, 0, 50)
                .unwrap()
                .remove(0);
            assert_eq!(first_row["state"], "succeeded");
            let redactions = BTreeMap::from([
                ("APP_SECRET".to_owned(), application_secret.to_owned()),
                ("decision credential".to_owned(), provider_secret.to_owned()),
            ]);
            let catalog = horde::decision::shadow::candidates(
                &inventory,
                Some(&policy),
                &settings,
                "simulated",
            )
            .unwrap();
            let raw_catalog = serde_json::to_value(catalog).unwrap();
            let redacted_catalog = horde::secrets::redact_json(&raw_catalog, &redactions);
            let raw_policy_hash = horde::store::hash(&serde_json::to_vec(&policy).unwrap());
            let redacted_policy = horde::secrets::redact_json(&policy, &redactions);
            assert_eq!(
                first_row["catalog_hash"],
                horde::store::hash(&serde_json::to_vec(&redacted_catalog).unwrap())
            );
            assert_ne!(
                first_row["catalog_hash"],
                horde::store::hash(&serde_json::to_vec(&raw_catalog).unwrap())
            );
            assert_eq!(
                first_row["policy_hash"],
                horde::store::hash(&serde_json::to_vec(&redacted_policy).unwrap())
            );
            assert_ne!(first_row["policy_hash"], raw_policy_hash);
            let artifact = std::fs::read_to_string(
                data.path()
                    .join("artifacts")
                    .join(first_row["artifact_hash"].as_str().unwrap()),
            )
            .unwrap();
            assert!(!artifact.contains(application_secret));
            assert!(!artifact.contains(provider_secret));
            db.conn
                .execute(
                    "UPDATE attempts SET state='succeeded',finished=? WHERE id=?",
                    rusqlite::params![horde::store::now(), own_attempt],
                )
                .unwrap();
            db.conn
                .execute("UPDATE steps SET state='pending' WHERE id=?", [step_id])
                .unwrap();

            let second = horde::decision::shadow::Job::new(
                data.path().to_path_buf(),
                task.clone(),
                row.clone(),
                None,
                "simulated".into(),
                decision.clone(),
            );
            queue.enqueue(&db, second).unwrap();
            drain(&mut queue, &db).await;
            let rows = horde::decision::store::list(&db, &task, 0, 50).unwrap();
            assert_eq!(rows[1]["state"], "cached");
            assert_eq!(rows[1]["provider_attempts"], 0);
            assert_eq!(rows[1]["usage"], json!({}));
            assert_eq!(rows[1]["cache_source"], rows[0]["id"]);
            let decision_text = serde_json::to_string(&rows).unwrap();
            assert!(!decision_text.contains(application_secret));
            assert!(!decision_text.contains(provider_secret));
            let events = db
                .rows(
                    "SELECT kind,data FROM events WHERE task=? AND kind LIKE 'decision.%'",
                    &[&task],
                )
                .unwrap();
            let event_text = serde_json::to_string(&events).unwrap();
            assert!(!event_text.contains(application_secret));
            assert!(!event_text.contains(provider_secret));

            horde::delegation::update_context(
                &db,
                &task,
                &json!({
                    "kind":"requirement",
                    "content":format!("new constraint {application_secret}"),
                    "provenance":"test",
                    "mandatory":true
                }),
            )
            .unwrap();
            let invalidated = horde::decision::shadow::Job::new(
                data.path().to_path_buf(),
                task.clone(),
                row.clone(),
                None,
                "simulated".into(),
                decision.clone(),
            );
            let invalidated_id = invalidated.id.clone();
            queue.enqueue(&db, invalidated).unwrap();
            drain(&mut queue, &db).await;
            let invalidated_body = String::from_utf8(bodies.recv().await.unwrap()).unwrap();
            assert!(!invalidated_body.contains(application_secret));
            let invalidated_row = horde::decision::store::list(&db, &task, 0, 50)
                .unwrap()
                .into_iter()
                .find(|row| row["id"] == invalidated_id)
                .unwrap();
            assert_eq!(invalidated_row["state"], "succeeded");
            assert!(invalidated_row["cache_source"].is_null());

            std::fs::remove_file(&bundle).unwrap();
            let unavailable = horde::decision::shadow::Job::new(
                data.path().to_path_buf(),
                task.clone(),
                row.clone(),
                None,
                "simulated".into(),
                decision.clone(),
            );
            let unavailable_id = unavailable.id.clone();
            queue.enqueue(&db, unavailable).unwrap();
            drain(&mut queue, &db).await;
            let unavailable_row = horde::decision::store::list(&db, &task, 0, 50)
                .unwrap()
                .into_iter()
                .find(|row| row["id"] == unavailable_id)
                .unwrap();
            assert_eq!(unavailable_row["state"], "skipped");
            assert_eq!(unavailable_row["error"], "preparation_unavailable");
            assert_eq!(unavailable_row["provider_attempts"], 0);
            std::fs::write(&bundle, format!("APP_SECRET={application_secret}\n")).unwrap();
            std::fs::set_permissions(&bundle, std::fs::Permissions::from_mode(0o600)).unwrap();

            let (slow_url, mut slow_bodies) =
                server(vec![("200 OK", shadow_response(), 2_000)]).await;
            let slow_decision = Decision {
                base_url: slow_url,
                deadline_ms: 3_000,
                ..decision
            };
            operator.rewrite(&slow_decision);
            db.conn
                .execute(
                    "UPDATE tasks SET settings=? WHERE id=?",
                    rusqlite::params![
                        serde_json::to_string(&Settings {
                            decision: slow_decision.clone(),
                            secret_bundles: vec!["app".into()],
                            ..Settings::default()
                        })
                        .unwrap(),
                        task
                    ],
                )
                .unwrap();
            let cancel = horde::decision::shadow::Job::new(
                data.path().to_path_buf(),
                task.clone(),
                row,
                None,
                "simulated".into(),
                slow_decision,
            );
            let cancel_id = cancel.id.clone();
            queue.enqueue(&db, cancel).unwrap();
            queue.tick(&db).await.unwrap();
            slow_bodies.recv().await.unwrap();
            db.conn
                .execute("UPDATE tasks SET status='cancelled' WHERE id=?", [&task])
                .unwrap();
            queue.tick(&db).await.unwrap();
            let cancelled = horde::decision::store::list(&db, &task, 0, 50).unwrap();
            let cancelled = cancelled.iter().find(|row| row["id"] == cancel_id).unwrap();
            assert_eq!(cancelled["state"], "cancelled");
            assert_eq!(cancelled["provider_attempts"], 1);
            let cancellation = db
                .rows(
                    "SELECT kind,data FROM events WHERE task=? AND kind='decision.cancelled'",
                    &[&task],
                )
                .unwrap();
            assert!(cancellation.iter().any(|event| {
                event["data"]
                    .as_str()
                    .is_some_and(|data| data.contains(&cancel_id))
            }));
            unsafe { std::env::remove_var(key) };
        })
        .await;
}
