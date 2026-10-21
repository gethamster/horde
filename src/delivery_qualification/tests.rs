//! Synthetic offline fixtures only; these never enroll or establish live qualification.
use super::super::manifest_tests::input;
use super::*;

fn add_artifact(receipt: &mut Value, name: &str, value: Value) -> String {
    let hash = digest(&value).unwrap();
    receipt["artifacts"][&hash] = json!(serde_json::to_string(&value).unwrap());
    let links = receipt["artifact_links"].as_array_mut().unwrap();
    if let Some(link) = links.iter_mut().find(|link| link["name"] == name) {
        *link = json!({"name":name,"hash":hash,"verified":1});
    } else {
        links.push(json!({"name":name,"hash":hash,"verified":1}));
    }
    hash
}
fn receipt(frozen: &Frozen, case: &Case, arm: &str) -> Value {
    let task = if arm == "baseline" {
        &case.baseline_task
    } else {
        &case.candidate_task
    };
    let created = frozen.frozen_at + 10;
    let head = "c".repeat(40);
    let decision = &frozen.manifest.decision;
    let mut result = json!({"task":{"id":task,"created":created,"objective":case.objective,
        "settings":serde_json::to_string(&json!({"decision":decision,"delivery":{"repository":case.repository,"base":"main"}})).unwrap()},
        "summary":{"task":task,"project":"operator-project","status":"succeeded","integrated_head":head,
            "run":{"run_id":task,"checkpoint":{"current":true,"verified":true,"commit_sha":head,"expected_main_head":case.base_commit,"validation":["independent-fixture-verifier"]}},
            "upstream_maintenance":{"binding":{"budget":{"reference":frozen.manifest.spend_reference}}}},
        "workers":[{"id":"generator-worker"},{"id":"reviewer-worker"}],"artifact_links":[],"artifacts":{},"attempts":[{"started":created}],
        "events":[{"seq":1,"kind":"task.finished","created":created+if arm=="baseline" {20}else{10},"data":{"status":"succeeded","integrated_head":head}}]});
    add_artifact(
        &mut result,
        "qualification-inputs",
        json!({"manifest_hash":frozen.manifest_hash,"case_id":case.id,"repository":case.repository,"base_commit":case.base_commit,
        "inputs_sha256":case.inputs_sha256,"objective_sha256":crate::store::hash(case.objective.as_bytes())}),
    );
    add_artifact(
        &mut result,
        "qualification-oracle",
        json!({"manifest_hash":frozen.manifest_hash,"case_id":case.id,"task":task,"head_sha":head,
        "manual_acceptance_question":"acceptance-1","manual_acceptance_answer":if case.critical {"escalate"}else{"accept"},"oracle_id":frozen.manifest.oracle_id,"oracle_spec_sha256":frozen.manifest.oracle_spec_sha256,"critical_defect_present":case.critical,
        "verified_completion":true,"reviewer_id":"independent-human","quality_passed":true,"critical_defect_detected":case.critical}),
    );
    if arm == "baseline" {
        let answer = if case.critical { "escalate" } else { "accept" };
        result["questions"] = json!([{"id":"acceptance-1","task":task,"question":"Accept this exact evaluated change or escalate.","answer":answer}]);
        result["events"].as_array_mut().unwrap().insert(0,json!({"seq":1,"kind":"question.answered","created":created+19,"data":{"question":"acceptance-1","answer":answer}}));
    }
    if arm == "candidate" {
        candidate_facts(&mut result, case, &head, created);
        let state =
            facts::production_state(frozen, case, &result, &head, created, created + 6).unwrap();
        let request = serde_json::to_value(
            crate::delivery_policy::delivery_veto_request(&decision.model, &state).unwrap(),
        )
        .unwrap();
        let response = json!({"model":decision.model,"answers":{"delivery":{"type":"choice","choice":if case.critical {"escalate"}else{"routine"},
            "confidence":0.99,"probabilities":{"routine":if case.critical {0.01}else{0.99},"escalate":if case.critical {0.99}else{0.01}}}}});
        let typed: crate::decision::DecisionRequest =
            serde_json::from_value(request.clone()).unwrap();
        let transport = add_artifact(
            &mut result,
            "qualification-decision-transport",
            json!({"endpoint":crate::decision::typesafe::endpoint(&decision.base_url).unwrap().as_str(),
            "http_status":200,"request_sha256":digest(&crate::decision::wire_request(&typed)).unwrap(),"response_sha256":digest(&response).unwrap()}),
        );
        add_artifact(
            &mut result,
            "qualification-decision",
            json!({"backend_fingerprint":decision.fingerprint().unwrap(),"delivery_policy_hash":digest(&frozen.manifest.delivery_policy).unwrap(),
            "purpose":"delivery:veto","catalog":"delivery-v1","transport_sha256":transport,"observed_at":created+6,"request":request,"response":response}),
        );
    }
    result
}
fn candidate_facts(result: &mut Value, case: &Case, head: &str, created: i64) {
    let manifest = json!({"base":case.base_commit,"head":head,"complete":true,"path_count":1,"excerpted_paths":1,
        "paths":[{"path":"src/fixture.rs","status":"M","binary":false,"covered":true,"uncovered_reason":null}],
        "excerpts":[{"path":"src/fixture.rs","excerpt":"-let x = 1;\n+let x = 2;","truncated":false}]});
    let evidence = add_artifact(
        result,
        "review-evidence-1",
        json!({"catalog":"review-v1","objective":case.objective,"revision":1,"context_version":1,"coverage":{"complete":true},"manifest":manifest}),
    );
    let mut answers = vec![json!({"id":"specialist","answer":"none","confidence":0.99})];
    for id in [
        "requirements",
        "correctness",
        "security",
        "tests",
        "integration",
        "insufficient_evidence",
    ] {
        answers.push(json!({"id":id,"type":"noul","noul":0.0,"confidence":0.99}));
    }
    result["reviews"] = json!([{"id":"review-1","event_seq":10,"created":created+1,"finished":created+4,"state":"succeeded","fresh":true,"head":head,"revision":1,"context_version":1,"coverage":{"complete":true},"result":{"answers":answers},"evidence_hash":evidence,"manifest_hash":digest(&manifest).unwrap(),"external_hash":"e".repeat(64)}]);
    result["changes"] = json!({"run_id":case.candidate_task,"base_sha":case.base_commit,"head_sha":head,"truncated":false,"files":[{"path":"src/fixture.rs","status":"M","diff":"-let x = 1;\n+let x = 2;","truncated":false}]});
    result["integrations"] =
        json!([{"state":"succeeded","worker":"generator-worker","created":created}]);
    result["steps"] = json!([{"id":"review-step","spec":serde_json::to_string(&json!({"id":"review","kind":"agent","role":"reviewer","output_types":{"reviewed_head":"string","reviewed_revision":"integer","verdict":"string","risk":"string","coverage_complete":"boolean","findings":"array"}})).unwrap()}]);
    result["attempts"] = json!([{"id":"attempt-1","step":"review-step","worker":"reviewer-worker","state":"succeeded","started":created+1,"finished":created+2,"result":serde_json::to_string(&json!({"accepted":true,"verdict":"pass","risk":"routine","coverage_complete":true,"findings":[],"reviewed_head":head,"reviewed_revision":1})).unwrap()}]);
    add_artifact(
        result,
        "qualification-github-receipts",
        json!({"repository":case.repository,"base_oid":case.base_commit,"observed_at":created+3,"pull_request":{"number":1,"state":"OPEN","headRefOid":head,"baseRefName":"main","author":{"login":"author"}},"check_runs":{"total_count":1,"check_runs":[{"id":1,"name":"test","head_sha":head,"status":"completed","conclusion":"success","completed_at":"2026-10-20T00:00:00Z"}]},"reviews":[{"id":2,"user":{"login":"independent"},"state":"APPROVED","commit_id":head,"submitted_at":"2026-10-20T00:00:00Z"}]}),
    );
}
fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("frozen.json");
    let manifest = parse_manifest(&input()).unwrap();
    let frozen = Frozen {
        manifest_hash: frozen_id(&manifest, 1_000_000).unwrap(),
        manifest,
        frozen_at: 1_000_000,
    };
    persist(&path, &frozen).unwrap();
    let evidence = directory.path().join("evidence");
    std::fs::create_dir(&evidence).unwrap();
    let mut rows = Vec::new();
    for case in &frozen.manifest.cases {
        for arm in ["baseline", "candidate"] {
            let value = receipt(&frozen, case, arm);
            let hash = digest(&value).unwrap();
            persist(&evidence.join(format!("{hash}.json")), &value).unwrap();
            rows.push(json!({"case_id":case.id,"arm":arm,"sha256":hash}));
        }
    }
    persist(
        &evidence.join("index.json"),
        &json!({"schema_version":1,"manifest_hash":frozen.manifest_hash,"runs":rows}),
    )
    .unwrap();
    (directory, path, evidence)
}
fn edit_first_candidate(evidence: &Path, edit: impl FnOnce(&mut Value)) {
    let index_path = evidence.join("index.json");
    let mut index = read(&index_path).unwrap();
    let row = &mut index["runs"][1];
    let old = row["sha256"].as_str().unwrap();
    let mut value = read(&evidence.join(format!("{old}.json"))).unwrap();
    edit(&mut value);
    let hash = digest(&value).unwrap();
    if !evidence.join(format!("{hash}.json")).exists() {
        persist(&evidence.join(format!("{hash}.json")), &value).unwrap();
    }
    row["sha256"] = json!(hash);
    std::fs::write(index_path, serde_json::to_vec(&index).unwrap()).unwrap();
}

#[test]
fn pages_exhaust_all_results_and_refuse_nonadvancing_cursors() {
    let mut count = 0;
    let rows = pages("seq", |after| {
        count += 1;
        Ok(if after < 1200 {
            json!([{"seq":after+1}])
        } else {
            json!([])
        })
    })
    .unwrap();
    assert_eq!(rows.len(), 1200);
    assert_eq!(count, 1201);
    assert!(pages("seq", |_| Ok(json!([{"seq":1}]))).is_err());
}

#[test]
fn real_review_pages_use_the_event_sequence_cursor() {
    let mut requests = 0;
    let reviews = pages("event_seq", |after| {
        requests += 1;
        Ok(match after {
            0 => json!([{"event_seq":501,"id":"review-1"}]),
            501 => json!([{"event_seq":1001,"id":"review-2"}]),
            1001 => json!([]),
            _ => panic!("wrong real review cursor"),
        })
    })
    .unwrap();
    assert_eq!(reviews.len(), 2);
    assert_eq!(requests, 3);
}

#[test]
fn three_identifiers_do_not_qualify_the_production_delivery_facts_contract() {
    let (_directory, path, evidence) = fixture();
    edit_first_candidate(&evidence, |value| {
        let (_, mut observation) = artifact(value, "qualification-decision").unwrap();
        let previous = observation["request"]["state"].clone();
        observation["request"]["state"] = json!({"head":previous["head"],"repo":previous["repo"],"base_oid":previous["base_oid"]});
        let request: crate::decision::DecisionRequest =
            serde_json::from_value(observation["request"].clone()).unwrap();
        let (_, mut transport) = artifact(value, "qualification-decision-transport").unwrap();
        transport["request_sha256"] =
            json!(digest(&crate::decision::wire_request(&request)).unwrap());
        observation["transport_sha256"] = json!(add_artifact(
            value,
            "qualification-decision-transport",
            transport
        ));
        add_artifact(value, "qualification-decision", observation);
    });
    let report = assess(&path, &evidence).unwrap();
    assert_eq!(
        report["qualified"], false,
        "qualification must exercise real production preflight evidence"
    );
    assert!(report.get("held_out").is_none());
}
#[test]
fn metrics_come_from_all_real_receipt_fields_and_runtime_completion_events() {
    let (_directory, path, evidence) = fixture();
    let report = assess(&path, &evidence).unwrap();
    assert_eq!(report["qualified"], true);
    assert_eq!(report["enrolled"], false);
    assert_eq!(report["held_out"]["cases"], 50);
    assert_eq!(
        report["held_out"]["baseline_verified_completion_ms"],
        20_000
    );
    assert_eq!(
        report["held_out"]["candidate_verified_completion_ms"],
        10_000
    );
    assert_eq!(report["held_out"]["candidate_critical_defect_recall"], 1.0);
    assert_eq!(report["evidence"].as_array().unwrap().len(), 100);
}
#[test]
fn missing_protected_manual_acceptance_or_runtime_preflight_sources_block_qualification() {
    for arm in [0, 1] {
        let (_directory, path, evidence) = fixture();
        let index_path = evidence.join("index.json");
        let mut index = read(&index_path).unwrap();
        let row = &mut index["runs"][arm];
        let mut value =
            read(&evidence.join(format!("{}.json", row["sha256"].as_str().unwrap()))).unwrap();
        if arm == 0 {
            value["questions"] = json!([]);
        } else {
            value["reviews"] = json!([]);
        }
        let hash = digest(&value).unwrap();
        persist(&evidence.join(format!("{hash}.json")), &value).unwrap();
        row["sha256"] = json!(hash);
        std::fs::write(index_path, serde_json::to_vec(&index).unwrap()).unwrap();
        let report = assess(&path, &evidence).unwrap();
        assert_eq!(report["qualified"], false);
        assert!(report.get("held_out").is_none());
    }
}
#[test]
fn missing_real_baseline_never_becomes_a_qualifying_aggregate_report() {
    let (_directory, path, evidence) = fixture();
    let mut index = read(&evidence.join("index.json")).unwrap();
    index["runs"].as_array_mut().unwrap().remove(0);
    std::fs::write(
        evidence.join("index.json"),
        serde_json::to_vec(&index).unwrap(),
    )
    .unwrap();
    assert!(
        assess(&path, &evidence)
            .unwrap_err()
            .to_string()
            .contains("Run pairs")
    );
}
#[test]
fn changed_head_unreviewed_oracle_missing_budget_and_unfrozen_execution_are_blockers() {
    for edit in [
        (|v: &mut Value| v["summary"]["run"]["checkpoint"]["current"] = json!(false))
            as fn(&mut Value),
        |v: &mut Value| v["artifact_links"][1]["verified"] = json!(0),
        |v: &mut Value| v["summary"]["upstream_maintenance"] = Value::Null,
        |v: &mut Value| v["attempts"][0]["started"] = json!(1),
        |v: &mut Value| v["attempts"][0]["started"] = json!(1_000_000),
    ] {
        let (_directory, path, evidence) = fixture();
        edit_first_candidate(&evidence, edit);
        let report = assess(&path, &evidence).unwrap();
        assert_eq!(report["qualified"], false);
        assert!(report.get("held_out").is_none());
        assert!(!report["blockers"].as_array().unwrap().is_empty());
    }
}
#[test]
fn equal_or_worse_actual_completion_latency_does_not_pass() {
    let (_directory, path, evidence) = fixture();
    edit_first_candidate(&evidence, |value| {
        value["events"][0]["created"] = json!(2_000_000)
    });
    let report = assess(&path, &evidence).unwrap();
    assert_eq!(report["qualified"], false);
    assert!(
        report["held_out"]["candidate_verified_completion_ms"]
            .as_u64()
            .unwrap()
            > 20_000
    );
}
#[test]
fn source_hash_corruption_is_reported_without_fabricated_metrics() {
    let (_directory, path, evidence) = fixture();
    let index = read(&evidence.join("index.json")).unwrap();
    std::fs::write(
        evidence.join(format!(
            "{}.json",
            index["runs"][0]["sha256"].as_str().unwrap()
        )),
        b"{}",
    )
    .unwrap();
    let report = assess(&path, &evidence).unwrap();
    assert_eq!(report["qualified"], false);
    assert!(report.get("held_out").is_none());
}

#[test]
fn original_artifact_hashes_are_rechecked_after_collection() {
    let (_directory, path, evidence) = fixture();
    edit_first_candidate(&evidence, |value| {
        let oracle = value["artifact_links"][1]["hash"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut content: Value =
            serde_json::from_str(value["artifacts"][&oracle].as_str().unwrap()).unwrap();
        content["quality_passed"] = json!(false);
        value["artifacts"][&oracle] = json!(serde_json::to_string(&content).unwrap());
    });
    let report = assess(&path, &evidence).unwrap();
    assert_eq!(report["qualified"], false);
    assert!(
        report.get("held_out").is_none(),
        "tampered source artifacts must fail integrity, not become computed metrics"
    );
}

#[test]
fn candidate_latency_includes_the_actual_advisory_decision() {
    let (_directory, path, evidence) = fixture();
    edit_first_candidate(&evidence, |value| {
        let (_, mut content) = artifact(value, "qualification-decision").unwrap();
        content["observed_at"] = json!(9_000_000);
        add_artifact(value, "qualification-decision", content);
    });
    let report = assess(&path, &evidence).unwrap();
    assert_eq!(
        report["qualified"], false,
        "a post-completion provider call cannot be omitted from completion latency"
    );
    assert!(report.get("held_out").is_none());
}

#[test]
fn shared_request_constructor_preserves_the_runtime_veto_contract() {
    let facts = json!({"head":"exact-head","repo":"gethamster/adam"});
    let request = crate::delivery_policy::delivery_veto_request("jev-1.13.0", &facts).unwrap();
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        json!({"model":"jev-1.13.0","state":facts,
        "questions":[{"type":"choice","id":"delivery","question":"Should this already policy-eligible routine change be escalated for manual delivery? Escalate on uncertainty, consequential API or migration effects, or security concerns.","options":["routine","escalate"]}]})
    );
}
