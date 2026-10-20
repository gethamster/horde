//! Operator-reviewed enrollment of an existing held-out qualification artifact.
use super::*;
pub fn enroll_qualification(db: &Store, args: &Value) -> Result<Value> {
    let project = args["project"]
        .as_str()
        .unwrap_or(crate::projects::DEFAULT_PROJECT);
    let settings = Settings::load_project_user(db, project)?;
    let policy = &settings.automatic_delivery;
    let decision = &settings.decision;
    ensure!(
        policy.enabled && decision.mode == DecisionMode::Shadow,
        "operator delivery and decision policies must be enabled before enrollment"
    );
    let id = args["qualification_id"]
        .as_str()
        .context("qualification_id required")?;
    let evidence = args["evidence_hash"]
        .as_str()
        .context("evidence_hash required")?;
    ensure!(
        id == policy.qualification_id
            && args["approved"] == true
            && args["reviewed_evidence_hash"] == evidence,
        "operator must approve the exact reviewed qualification artifact"
    );
    ensure!(
        evidence.len() == 64 && evidence.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid qualification evidence hash"
    );
    let exists: i64 = db.conn.query_row(
        "SELECT count(*) FROM artifacts WHERE hash=?",
        [evidence],
        |r| r.get(0),
    )?;
    ensure!(
        exists == 1,
        "qualification artifact must already be retained in the content-addressed store"
    );
    let policy_hash = json_hash(&serde_json::to_value(policy)?)?;
    db.atomic(|| {
        let existing=db.rows("SELECT evidence_hash FROM delivery_qualifications WHERE id=?",&[&id])?;
        if let Some(existing)=existing.first() {
            ensure!(existing["evidence_hash"]==evidence,"qualification identity is immutable; choose a new identity for changed evidence");
        } else {
            db.conn.execute("INSERT INTO delivery_qualifications(id,backend,model,policy,purpose,catalog,delivery_policy_hash,threshold,evidence_hash,held_out_passed,created) VALUES(?,?,?,?,?,?,?,?,?,1,?)",rusqlite::params![id,decision.backend,decision.model,decision.policy,"delivery:veto","delivery-v1",policy_hash,policy.minimum_routine_probability,evidence,now()])?;
        }
        // Invalid or regressed reports roll back the provisional record atomically.
        verify_qualification(db,policy,decision)?;
        if existing.is_empty() {
            crate::management::event(db,"delivery.qualification_enrolled",json!({"project":project,"qualification_id":id,"evidence_hash":evidence,"reviewed_evidence_hash":evidence,"delivery_policy_hash":policy_hash}))?;
        }
        Ok(json!({"qualification_id":id,"evidence_hash":evidence,"delivery_policy_hash":policy_hash,"enrolled":true,"project":project}))
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Store, Settings) {
        let dir = tempfile::tempdir().unwrap();
        let mut db = Store::open(&dir.path().join("data")).unwrap();
        let config = dir.path().join("operator");
        std::fs::create_dir(&config).unwrap();
        db.config_dir = Some(config.clone());
        let settings:Settings=serde_json::from_value(json!({"decision":{"mode":"shadow","backend":"tuara","base_url":"https://tuara.com/router/v1","api_key_env":"SYNTHETIC_DECISION_KEY"},"automatic_delivery":{"enabled":true,"gh_program":"/usr/bin/gh","repositories":["test/repo"],"bases":["main"],"environments":["staging"],"deploy_workflows":["deploy.yml"],"allowed_paths":["src"],"required_checks":["test"],"minimum_approvals":1,"require_independent_review":true,"qualification_id":"held-out-1","minimum_routine_probability":0.8,"minimum_review_confidence":0.8}})).unwrap();
        std::fs::write(
            config.join("config.toml"),
            toml::to_string(&settings).unwrap(),
        )
        .unwrap();
        (dir, db, settings)
    }
    fn evidence(db: &Store, settings: &Settings, cases: u64) -> String {
        let decision = &settings.decision;
        let policy = &settings.automatic_delivery;
        let report = json!({"backend":decision.backend,"model":decision.model,"backend_fingerprint":decision.fingerprint().unwrap(),"policy":decision.policy,"purpose":"delivery:veto","catalog":"delivery-v1","delivery_policy_hash":json_hash(&serde_json::to_value(policy).unwrap()).unwrap(),"minimum_routine_probability":0.8,"held_out":{"cases":cases,"baseline_quality":0.9,"candidate_quality":0.91,"baseline_critical_defect_recall":1.0,"candidate_critical_defect_recall":1.0,"baseline_verified_completion_ms":1000,"candidate_verified_completion_ms":900}});
        let bytes = serde_json::to_vec(&report).unwrap();
        let hash = hash(&bytes);
        std::fs::create_dir_all(db.root.join("artifacts")).unwrap();
        std::fs::write(db.root.join("artifacts").join(&hash), &bytes).unwrap();
        db.conn
            .execute(
                "INSERT INTO artifacts VALUES(?,?,?)",
                rusqlite::params![hash, bytes.len(), now()],
            )
            .unwrap();
        hash
    }
    #[test]
    fn enrollment_requires_reviewed_exact_evidence_and_is_immutable() {
        let (_dir, db, settings) = fixture();
        let hash = evidence(&db, &settings, 50);
        let args = json!({"qualification_id":"held-out-1","evidence_hash":hash,"reviewed_evidence_hash":hash,"approved":true});
        assert_eq!(enroll_qualification(&db, &args).unwrap()["enrolled"], true);
        assert_eq!(enroll_qualification(&db, &args).unwrap()["enrolled"], true);
        let mut wrong = args.clone();
        wrong["reviewed_evidence_hash"] = json!("0".repeat(64));
        assert!(enroll_qualification(&db, &wrong).is_err());
        let changed = evidence(&db, &settings, 51);
        let changed = json!({"qualification_id":"held-out-1","evidence_hash":changed,"reviewed_evidence_hash":changed,"approved":true});
        assert!(enroll_qualification(&db, &changed).is_err());
    }
    #[test]
    fn undersized_or_corrupted_report_never_leaves_a_qualification() {
        let (_dir, db, settings) = fixture();
        let hash = evidence(&db, &settings, 49);
        let args = json!({"qualification_id":"held-out-1","evidence_hash":hash,"reviewed_evidence_hash":hash,"approved":true});
        assert!(enroll_qualification(&db, &args).is_err());
        assert!(
            db.rows("SELECT id FROM delivery_qualifications", &[])
                .unwrap()
                .is_empty()
        );
        std::fs::write(db.root.join("artifacts").join(&hash), b"corrupted").unwrap();
        assert!(enroll_qualification(&db, &args).is_err());
        assert!(
            db.rows("SELECT id FROM delivery_qualifications", &[])
                .unwrap()
                .is_empty()
        );
    }
}
