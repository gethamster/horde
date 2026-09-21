use super::*;
#[test]
fn routine_paths_are_bounded() {
    validate_paths(&["src/ui/button.rs".into()], &["src/ui".into()]).unwrap();
    assert!(validate_paths(&["src/auth/login.rs".into()], &["src".into()]).is_err());
    assert!(validate_paths(&["migrations/001.sql".into()], &["migrations".into()]).is_err());
    assert!(validate_paths(&["src/ui-extra/a.rs".into()], &["src/ui".into()]).is_err());
    assert!(
        validate_paths(
            &[".github/workflows/deploy.yml".into()],
            &[".github".into()]
        )
        .is_err()
    );
    assert!(validate_paths(&["src/ui/config/settings.rs".into()], &["src/ui".into()]).is_err());
    assert!(validate_paths(&["package.json".into()], &["package.json".into()]).is_err());
}
#[test]
fn exact_head_checks_and_approvals() {
    let checks = json!({"total_count":1,"check_runs":[{"id":1,"name":"test","head_sha":"abc","status":"completed","conclusion":"success","completed_at":"2026-01-01T00:00:00Z"}]});
    let first = check_runs(&checks, "abc", &["test".into()]).unwrap();
    assert!(check_runs(&checks, "def", &["test".into()]).is_err());
    let mut rerun = checks.clone();
    rerun["check_runs"][0]["id"] = json!(2);
    assert_ne!(first, check_runs(&rerun, "abc", &["test".into()]).unwrap());
    let reviews = json!([{"id":1,"submitted_at":"2026-01-01T00:00:00Z","user":{"login":"alice"},"state":"APPROVED","commit_id":"abc"},
            {"id":2,"submitted_at":"2026-01-01T00:00:01Z","user":{"login":"bob"},"state":"APPROVED","commit_id":"old"}]);
    let first_approval = approvals(&reviews, "abc", "author", 1).unwrap();
    assert!(approvals(&reviews, "abc", "alice", 1).is_err());
    let replaced = json!([reviews[0],{"id":4,"submitted_at":"2026-01-01T00:00:02Z","user":{"login":"alice"},"state":"APPROVED","commit_id":"abc"}]);
    assert_ne!(
        first_approval,
        approvals(&replaced, "abc", "author", 1).unwrap()
    );
    let revoked = json!([reviews[0],{"id":3,"submitted_at":"2026-01-01T00:00:02Z","user":{"login":"alice"},"state":"CHANGES_REQUESTED","commit_id":"abc"}]);
    assert!(approvals(&revoked, "abc", "author", 1).is_err());
}
#[test]
fn reviewer_evidence_must_cover_final_head_and_be_routine() {
    let result = json!({"accepted":true,"verdict":"pass","risk":"routine","coverage_complete":true,
            "findings":[],"reviewed_head":"new","reviewed_revision":2});
    assert!(reviewer_result_passes(&result, "new", 2));
    assert!(!reviewer_result_passes(&result, "old", 2));
    assert!(!reviewer_result_passes(&result, "new", 3));
    let mut adverse = result;
    adverse["risk"] = json!("security");
    assert!(!reviewer_result_passes(&adverse, "new", 2));
}
#[test]
fn low_confidence_or_flagged_review_cannot_authorize_delivery() {
    let mut answers = [
        "requirements",
        "correctness",
        "security",
        "tests",
        "integration",
        "insufficient_evidence",
    ]
    .into_iter()
    .map(|id| json!({"type":"noul","id":id,"noul":0.1,"confidence":0.9}))
    .collect::<Vec<_>>();
    answers.push(json!({"type":"choice","id":"specialist","answer":"none","confidence":0.9}));
    let mut result = json!({"answers":answers});
    assert!(review_signals_clear(&result, 0.8));
    result["answers"][2]["noul"] = json!(0.8);
    assert!(!review_signals_clear(&result, 0.8));
    result["answers"][2]["noul"] = json!(0.1);
    result["answers"][2]["confidence"] = json!(0.2);
    assert!(!review_signals_clear(&result, 0.8));
}
#[test]
fn boundary_removals_escalate() {
    assert!(diff_has_sensitive_change(
        "@@ -1 +0,0 @@\n- check_permission(user);\n"
    ));
    assert!(diff_has_sensitive_change(
        "@@ -1 +1 @@\n+ pub fn account() {}\n"
    ));
    assert!(!diff_has_sensitive_change(
        "@@ -1 +1 @@\n- text = old;\n+ text = new;\n"
    ));
}
#[test]
fn manifest_preserves_leading_space_path() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    crate::git::run(repo, &["init", "-q"]).unwrap();
    crate::git::run(repo, &["config", "user.name", "Test"]).unwrap();
    crate::git::run(repo, &["config", "user.email", "test@example.test"]).unwrap();
    crate::git::run(repo, &["commit", "--allow-empty", "-qm", "base"]).unwrap();
    let base = crate::git::run(repo, &["rev-parse", "HEAD"]).unwrap();
    crate::git::run(repo, &["update-ref", "refs/remotes/origin/main", &base]).unwrap();
    std::fs::create_dir_all(repo.join(" src/ui")).unwrap();
    std::fs::write(repo.join(" src/ui/button.rs"), "text = new;\n").unwrap();
    crate::git::run(repo, &["add", "."]).unwrap();
    crate::git::run(repo, &["commit", "-qm", "change"]).unwrap();
    let head = crate::git::run(repo, &["rev-parse", "HEAD"]).unwrap();
    let (paths, _) = routine_manifest(repo, "main", &head).unwrap();
    assert_eq!(paths, [" src/ui/button.rs"]);
    assert!(validate_paths(&paths, &["src/ui".into()]).is_err());
}
#[test]
fn push_run_identity_is_unique() {
    let runs = json!([{"databaseId":1,"headSha":"abc","headBranch":"main","event":"workflow_dispatch"},
            {"databaseId":2,"headSha":"abc","headBranch":"main","event":"push"}]);
    assert_eq!(
        select_deployment_run(&runs, "abc", "main").unwrap()["databaseId"],
        2
    );
    assert!(
        select_deployment_run(&runs, "def", "main")
            .unwrap()
            .is_null()
    );
}
#[test]
fn base_protection_and_merge_parent_are_bound() {
    let policy = AutomaticDelivery {
        required_checks: vec!["test".into()],
        minimum_approvals: 1,
        ..Default::default()
    };
    let protected = json!({"required_status_checks":{"strict":true,"contexts":["test"]},"enforce_admins":{"enabled":true},
            "required_pull_request_reviews":{"required_approving_review_count":1,
                "bypass_pull_request_allowances":{"users":[],"teams":[],"apps":[]}}});
    assert!(strict_protection(&protected, &policy));
    let mut unprotected = protected.clone();
    unprotected["required_status_checks"]["strict"] = json!(false);
    assert!(!strict_protection(&unprotected, &policy));
    let mut missing_reviews = protected.clone();
    missing_reviews["required_pull_request_reviews"] = Value::Null;
    assert!(!strict_protection(&missing_reviews, &policy));
    let mut bypass = protected;
    bypass["required_pull_request_reviews"]["bypass_pull_request_allowances"]["apps"] =
        json!([{"slug":"bot"}]);
    assert!(!strict_protection(&bypass, &policy));
    let commit = json!({"parents":[{"sha":"base"}]});
    assert!(merge_parent_matches(&commit, "base"));
    assert!(!merge_parent_matches(&commit, "advanced-base"));
}
#[test]
fn held_out_qualification_is_required() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let decision = crate::config::Decision::default();
    let policy = AutomaticDelivery {
        qualification_id: "evaluation-1".into(),
        minimum_routine_probability: 0.8,
        ..Default::default()
    };
    assert!(verify_qualification(&db, &policy, &decision).is_err());
    let policy_hash = json_hash(&serde_json::to_value(&policy).unwrap()).unwrap();
    let report = json!({"backend":decision.backend,"model":decision.model,"policy":decision.policy,
            "purpose":"delivery:veto","catalog":"delivery-v1","delivery_policy_hash":policy_hash,
            "minimum_routine_probability":0.8,
            "held_out":{"cases":50,"baseline_quality":0.9,"candidate_quality":0.91,
                "baseline_critical_defect_recall":1.0,"candidate_critical_defect_recall":1.0,
                "baseline_verified_completion_ms":1000,"candidate_verified_completion_ms":900}});
    let bytes = serde_json::to_vec(&report).unwrap();
    let evidence = hash(&bytes);
    std::fs::create_dir_all(dir.path().join("artifacts")).unwrap();
    std::fs::write(dir.path().join("artifacts").join(&evidence), &bytes).unwrap();
    db.conn
        .execute(
            "INSERT INTO artifacts(hash,size,created) VALUES(?,?,?)",
            params![evidence, bytes.len() as i64, now()],
        )
        .unwrap();
    db.conn.execute("INSERT INTO delivery_qualifications(id,backend,model,policy,purpose,catalog,delivery_policy_hash,threshold,evidence_hash,held_out_passed,created) VALUES(?,?,?,?,?,?,?,?,?,1,?)",
            params!["evaluation-1",decision.backend,decision.model,decision.policy,"delivery:veto","delivery-v1",policy_hash,0.8,evidence,now()]).unwrap();
    verify_qualification(&db, &policy, &decision).unwrap();
    let mut changed = decision.clone();
    changed.policy = "another-policy".into();
    assert!(verify_qualification(&db, &policy, &changed).is_err());
    let changed_threshold = AutomaticDelivery {
        minimum_routine_probability: 0.9,
        ..policy
    };
    assert!(verify_qualification(&db, &changed_threshold, &decision).is_err());
}
