//! Reconstruct the production veto state from retained authoritative source data.
use super::*;

fn decoded(value: &Value) -> Result<Value> {
    match value {
        Value::String(raw) => Ok(serde_json::from_str(raw)?),
        value => Ok(value.clone()),
    }
}

pub(super) fn baseline(
    case: &Case,
    receipt: &Value,
    oracle: &Value,
    head: &str,
    created: i64,
    finished: i64,
    recalled: bool,
) -> Result<()> {
    let settings: crate::config::Settings = serde_json::from_str(
        receipt["task"]["settings"]
            .as_str()
            .context("baseline settings missing")?,
    )?;
    ensure!(
        !settings.automatic_delivery.enabled,
        "baseline used automatic delivery instead of conventional manual acceptance"
    );
    let question_id = oracle["manual_acceptance_question"]
        .as_str()
        .context("baseline has no actual manual-acceptance question receipt")?;
    let question = receipt["questions"]
        .as_array()
        .context("baseline questions unavailable")?
        .iter()
        .find(|question| question["id"] == question_id)
        .context("manual-acceptance question is not in the actual Run")?;
    ensure!(
        question["task"] == receipt["task"]["id"]
            && question["route_origin"] == receipt["task"]["id"]
            && question["human_only"] == 1
            && question["answerer"] == "external caller"
            && crate::store::hash(
                question["question"]
                    .as_str()
                    .context("manual-acceptance question text missing")?
                    .as_bytes()
            ) == case.baseline_acceptance_question_sha256,
        "manual-acceptance question differs from the frozen baseline contract"
    );
    let expected = if case.critical && recalled {
        "escalate"
    } else {
        "accept"
    };
    ensure!(
        question["answer"] == expected
            && oracle["manual_acceptance_answer"] == expected
            && oracle["head_sha"] == head,
        "baseline oracle does not match the actual manual decision"
    );
    let events = receipt["events"]
        .as_array()
        .context("baseline manual decision events unavailable")?;
    let mut answered = None;
    for event in events
        .iter()
        .filter(|event| event["kind"] == "question.answered")
    {
        let data = decoded(&event["data"])?;
        let identity = match (data.get("id"), data.get("question")) {
            (Some(id), Some(legacy)) => {
                ensure!(
                    id == legacy,
                    "conflicting manual decision question identities"
                );
                Some(id)
            }
            (id, legacy) => id.or(legacy),
        };
        if identity == Some(&json!(question_id)) && data["answer"] == expected {
            ensure!(
                data["origin"] == receipt["task"]["id"] && data["author"] == "external caller",
                "manual decision lacks protected human provenance"
            );
            ensure!(answered.is_none(), "duplicate manual decision event");
            answered = Some(event);
        }
    }
    let answered = answered.context("baseline lacks the protected operator answer event")?;
    ensure!(
        answered["created"]
            .as_i64()
            .is_some_and(|at| at >= created && at <= finished),
        "manual decision was not included in actual baseline completion time"
    );
    Ok(())
}

fn reviewed_manifest<'a>(
    frozen: &Frozen,
    case: &Case,
    receipt: &'a Value,
    head: &str,
) -> Result<(&'a Value, Value)> {
    let rows = receipt["reviews"]
        .as_array()
        .context("actual runtime review checkpoint rows missing")?;
    let row = rows
        .iter()
        .filter(|row| row["head"] == head)
        .max_by_key(|row| row["event_seq"].as_i64().unwrap_or(0))
        .context("candidate has no actual runtime review checkpoint")?;
    let revision = row["revision"]
        .as_i64()
        .context("review revision unavailable")?;
    let context = row["context_version"]
        .as_i64()
        .context("review context unavailable")?;
    ensure!(
        crate::delivery_policy::review_eligible(row, head, revision, context)
            && crate::delivery_policy::review_signals_clear(
                &row["result"],
                frozen.manifest.delivery_policy.minimum_review_confidence
            ),
        "actual runtime review is stale, incomplete or flagged"
    );
    let evidence_hash = row["evidence_hash"]
        .as_str()
        .context("runtime review evidence hash unavailable")?;
    let evidence = raw_artifact(receipt, evidence_hash)?;
    ensure!(
        evidence["catalog"] == "review-v1"
            && evidence["objective"] == case.objective
            && evidence["revision"] == revision
            && evidence["context_version"] == context
            && evidence["coverage"]["complete"] == true
            && digest(&evidence["manifest"])? == row["manifest_hash"],
        "review checkpoint is not bound to actual runtime-produced evidence"
    );
    let manifest = &evidence["manifest"];
    ensure!(
        manifest["base"] == case.base_commit
            && manifest["head"] == head
            && manifest["complete"] == true,
        "runtime manifest differs from the frozen verified change"
    );
    let changes = &receipt["changes"];
    ensure!(
        changes["run_id"] == receipt["task"]["id"]
            && changes["base_sha"] == case.base_commit
            && changes["head_sha"] == head
            && changes["truncated"] == false,
        "actual Run changes are missing or incomplete"
    );
    let paths = manifest["paths"]
        .as_array()
        .context("runtime manifest paths unavailable")?;
    let actual = changes["files"]
        .as_array()
        .context("actual committed file changes unavailable")?;
    ensure!(
        !paths.is_empty() && paths.len() <= 8 && paths.len() == actual.len(),
        "delivery corpus must have the same complete routine change coverage"
    );
    let excerpts = manifest["excerpts"]
        .as_array()
        .context("actual reviewed excerpts unavailable")?;
    ensure!(
        manifest["path_count"].as_u64() == Some(paths.len() as u64)
            && manifest["excerpted_paths"].as_u64() == Some(paths.len() as u64)
            && excerpts.len() == paths.len(),
        "actual reviewed excerpt coverage differs from complete changes"
    );
    for ((path, file), excerpt) in paths.iter().zip(actual).zip(excerpts) {
        ensure!(
            path["covered"] == true
                && path["binary"] == false
                && ["A", "M"].contains(&path["status"].as_str().unwrap_or(""))
                && path["path"] == file["path"]
                && path["status"] == file["status"]
                && file["truncated"] == false
                && excerpt["path"] == path["path"]
                && excerpt["truncated"] == false
                && !crate::delivery_policy::diff_has_sensitive_change(
                    excerpt["excerpt"]
                        .as_str()
                        .context("actual reviewed excerpt missing")?
                )
                && !crate::delivery_policy::diff_has_sensitive_change(
                    file["diff"]
                        .as_str()
                        .context("actual committed diff missing")?
                ),
            "actual committed change differs from runtime review coverage"
        );
    }
    Ok((row, evidence["manifest"].clone()))
}

fn independent_review(receipt: &Value, head: &str, revision: i64, observed: i64) -> Result<Value> {
    let integrations = receipt["integrations"]
        .as_array()
        .context("actual integration receipts unavailable")?;
    let integrated_workers: BTreeSet<_> = integrations
        .iter()
        .filter(|row| row["state"] == "succeeded")
        .filter_map(|row| row["worker"].as_str())
        .collect();
    let last = integrations
        .iter()
        .filter(|row| row["state"] == "succeeded")
        .filter_map(|row| row["created"].as_i64())
        .max()
        .unwrap_or(0);
    let steps = receipt["steps"]
        .as_array()
        .context("actual reviewer step definitions unavailable")?;
    let attempts = receipt["attempts"]
        .as_array()
        .context("actual reviewer attempts unavailable")?;
    let mut eligible = Vec::new();
    for attempt in attempts {
        let Some(worker) = attempt["worker"].as_str() else {
            continue;
        };
        if integrated_workers.contains(worker) || attempt["started"].as_i64().unwrap_or(0) < last {
            continue;
        }
        let Some(step) = steps.iter().find(|step| step["id"] == attempt["step"]) else {
            continue;
        };
        let spec: crate::template::Step = serde_json::from_value(decoded(&step["spec"])?)?;
        if spec.kind != "agent" || spec.role != "reviewer" {
            continue;
        }
        for (name, kind) in [
            ("reviewed_head", "string"),
            ("reviewed_revision", "integer"),
            ("verdict", "string"),
            ("risk", "string"),
            ("coverage_complete", "boolean"),
            ("findings", "array"),
        ] {
            ensure!(
                spec.output_types
                    .get(name)
                    .is_some_and(|value| value == kind),
                "actual reviewer step lacks the production evidence contract"
            );
        }
        eligible.push(attempt);
    }
    let attempt = eligible
        .into_iter()
        .max_by_key(|attempt| {
            (
                attempt["finished"].as_i64().unwrap_or(0),
                attempt["started"].as_i64().unwrap_or(0),
            )
        })
        .context("actual independent reviewer attempt unavailable")?;
    let result = decoded(&attempt["result"])?;
    ensure!(
        attempt["state"] == "succeeded"
            && attempt["finished"]
                .as_i64()
                .is_some_and(|at| at <= observed)
            && crate::delivery_policy::reviewer_result_passes(&result, head, revision),
        "actual latest independent reviewer did not pass the exact head/revision"
    );
    Ok(
        json!({"attempt":attempt["id"],"worker":attempt["worker"],"result_hash":digest(&result)?,"reviewed_head":head,"reviewed_revision":revision}),
    )
}

pub(super) fn production_state(
    frozen: &Frozen,
    case: &Case,
    receipt: &Value,
    head: &str,
    created: i64,
    finished: i64,
) -> Result<Value> {
    let settings: crate::config::Settings = serde_json::from_str(
        receipt["task"]["settings"]
            .as_str()
            .context("actual candidate settings unavailable")?,
    )?;
    let policy = &frozen.manifest.delivery_policy;
    ensure!(
        settings.delivery.repository == case.repository
            && policy.bases.contains(&settings.delivery.base),
        "actual candidate repository/base configuration differs from frozen delivery scope"
    );
    let (review, mut manifest) = reviewed_manifest(frozen, case, receipt, head)?;
    ensure!(
        review["created"]
            .as_i64()
            .is_some_and(|at| at >= created && at <= finished)
            && review["finished"]
                .as_i64()
                .is_some_and(|at| at >= created && at <= finished),
        "actual runtime review did not finish before the advisory decision"
    );
    let revision = review["revision"]
        .as_i64()
        .context("actual reviewed revision unavailable")?;
    let context = review["context_version"]
        .as_i64()
        .context("actual reviewed context unavailable")?;
    let paths = manifest["paths"]
        .as_array()
        .context("runtime manifest paths missing")?
        .iter()
        .map(|row| {
            row["path"]
                .as_str()
                .map(str::to_owned)
                .context("changed path missing")
        })
        .collect::<Result<Vec<_>>>()?;
    crate::delivery_policy::validate_paths(&paths, &policy.allowed_paths)?;
    let (_, github) = artifact(receipt, "qualification-github-receipts")?;
    ensure!(
        github["repository"] == case.repository
            && github["base_oid"] == case.base_commit
            && github["observed_at"]
                .as_i64()
                .is_some_and(|at| at >= created && at <= finished),
        "actual GitHub snapshot has a different source identity or timing"
    );
    let pr = &github["pull_request"];
    ensure!(
        pr["state"] == "OPEN"
            && pr["number"].as_u64().is_some_and(|id| id > 0)
            && pr["headRefOid"] == head
            && pr["baseRefName"] == settings.delivery.base,
        "actual PR does not identify the verified Run head/base"
    );
    let checks =
        crate::delivery_policy::check_runs(&github["check_runs"], head, &policy.required_checks)?;
    let approvals = crate::delivery_policy::approvals(
        &github["reviews"],
        head,
        pr["author"]["login"]
            .as_str()
            .context("actual PR author unavailable")?,
        policy.minimum_approvals,
    )?;
    let environment = if let Some(package) = &policy.package_release {
        format!("package:{}", package.repository)
    } else {
        let value = settings
            .delivery
            .environment
            .as_deref()
            .context("actual delivery environment missing")?;
        ensure!(
            policy.environments.iter().any(|allowed| allowed == value),
            "actual environment is outside candidate policy"
        );
        value.to_owned()
    };
    // Review capture identifies its base by SHA; the production delivery manifest
    // uses this branch label after independently pinning it to the same base SHA.
    manifest["base"] = json!(format!("refs/remotes/origin/{}", settings.delivery.base));
    let review_fact = json!({"id":review["id"],"head":head,"revision":revision,"context_version":context,"evidence_hash":review["evidence_hash"],"manifest_hash":review["manifest_hash"],"external_hash":review["external_hash"],"signals_hash":digest(&review["result"])?});
    Ok(
        json!({"head":head,"repo":case.repository,"base":settings.delivery.base,"base_oid":case.base_commit,"environment":environment,
        "paths":paths,"manifest":manifest,"checks":checks,"approvals":approvals,"review":review_fact,"independent_review":independent_review(receipt,head,revision,finished)?,"context_version":context}),
    )
}

#[cfg(test)]
mod routed_baseline_tests {
    use super::*;

    #[test]
    fn supported_protected_human_answer_is_a_baseline_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let repo = directory.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        for argv in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@localhost"],
            vec!["commit", "--allow-empty", "-m", "initial"],
        ] {
            crate::git::run(&repo, &argv).unwrap();
        }
        let db = crate::store::Store::open(&directory.path().join("data")).unwrap();
        let settings = crate::config::Settings::default();
        let plan = crate::template::compile(
            "simulated",
            &crate::template::load_templates(&repo).unwrap(),
            std::collections::BTreeMap::from([("task".into(), "baseline".into())]),
        )
        .unwrap();
        let task = db.submit("baseline", &repo, &settings, &plan).unwrap();
        let question = crate::protocol::dispatch(&db, "request_question", json!({"task":task,"id":"acceptance","question":"Accept this exact evaluated change or escalate.","human_only":true}), None).unwrap();
        assert!(
            crate::protocol::dispatch(
                &db,
                "answer_question",
                json!({"task":task,"question":question["id"],"answer":"accept"}),
                None
            )
            .is_err()
        );
        crate::protocol::dispatch(
            &db,
            "answer_question",
            json!({"task":task,"question":question["id"],"answer":"accept","human":true}),
            None,
        )
        .unwrap();
        let inspection =
            crate::protocol::dispatch(&db, "inspect", json!({"task":task}), None).unwrap();
        let receipt = json!({"task":inspection["task"],"questions":inspection["questions"],"events":db.rows("SELECT * FROM events WHERE task=? ORDER BY seq", &[&task]).unwrap()});
        let mut case = crate::delivery_qualification::parse_manifest(
            &crate::delivery_qualification::manifest_tests::input(),
        )
        .unwrap()
        .cases[1]
            .clone();
        case.baseline_task = task;
        let oracle = json!({"manual_acceptance_question":question["id"],"manual_acceptance_answer":"accept","head_sha":"head"});
        baseline(
            &case,
            &receipt,
            &oracle,
            "head",
            crate::store::now() - 10,
            crate::store::now() + 10,
            false,
        )
        .unwrap();
        for field in ["human_only", "answerer", "route_origin"] {
            let mut unprotected = receipt.clone();
            unprotected["questions"][0][field] = Value::Null;
            assert!(baseline(&case, &unprotected, &oracle, "head", 0, i64::MAX, false).is_err());
        }
        let mut conflict = receipt.clone();
        let event = conflict["events"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|event| event["kind"] == "question.answered")
            .unwrap();
        let mut data = decoded(&event["data"]).unwrap();
        data["question"] = json!("different-question");
        event["data"] = data;
        assert!(baseline(&case, &conflict, &oracle, "head", 0, i64::MAX, false).is_err());
        assert!(baseline(&case, &receipt, &oracle, "changed-head", 0, i64::MAX, false).is_err());
        assert!(baseline(&case, &receipt, &oracle, "head", 0, 1, false).is_err());
    }
}
