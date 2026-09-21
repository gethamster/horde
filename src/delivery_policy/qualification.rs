use super::*;

pub(super) fn current_policy(i: &Invocation<'_>) -> Result<Option<AutomaticDelivery>> {
    let policy = &i.settings.automatic_delivery;
    if !policy.enabled {
        return Ok(None);
    }
    let operator = Settings::load_user().context("current operator delivery policy unavailable")?;
    ensure!(
        operator.automatic_delivery == *policy && operator.automatic_delivery.enabled,
        "operator automatic-delivery policy changed"
    );
    ensure!(
        i.settings.decision.mode == DecisionMode::Shadow
            && operator.decision == i.settings.decision,
        "decision service is not currently authorized for automatic delivery"
    );
    ensure!(
        i.settings.decision.backend == "typesafe" && i.settings.decision.model == "jev-1.13.0",
        "automatic delivery requires a separately qualified decision provider and model"
    );
    verify_qualification(i.db, policy, &i.settings.decision)?;
    Ok(Some(policy.clone()))
}

pub(super) fn verify_qualification(
    db: &Store,
    policy: &AutomaticDelivery,
    decision: &crate::config::Decision,
) -> Result<()> {
    let qualification = db.rows("SELECT backend,model,policy,purpose,catalog,delivery_policy_hash,threshold,evidence_hash,held_out_passed FROM delivery_qualifications WHERE id=?",
        &[&policy.qualification_id])?.into_iter().next().context("delivery has no held-out qualification record")?;
    let policy_hash = json_hash(&serde_json::to_value(policy)?)?;
    ensure!(
        qualification["held_out_passed"] == 1
            && qualification["backend"] == decision.backend
            && qualification["model"] == decision.model
            && qualification["policy"] == decision.policy
            && qualification["purpose"] == "delivery:veto"
            && qualification["catalog"] == "delivery-v1"
            && qualification["delivery_policy_hash"] == policy_hash
            && qualification["threshold"].as_f64() == Some(policy.minimum_routine_probability)
            && qualification["evidence_hash"]
                .as_str()
                .is_some_and(|v| v.len() == 64),
        "delivery qualification does not match the pinned decision service"
    );
    let evidence = qualification["evidence_hash"]
        .as_str()
        .context("qualification evidence hash")?;
    let bytes = std::fs::read(db.root.join("artifacts").join(evidence))
        .context("delivery qualification artifact unavailable")?;
    ensure!(
        bytes.len() <= 64 * 1024 && hash(&bytes) == evidence,
        "delivery qualification artifact failed integrity check"
    );
    let report: Value = serde_json::from_slice(&bytes)?;
    let held = &report["held_out"];
    let metric = |name: &str| -> Result<f64> {
        let value = held[name]
            .as_f64()
            .with_context(|| format!("missing held-out metric {name}"))?;
        ensure!(
            value.is_finite() && (0.0..=1.0).contains(&value),
            "invalid held-out metric {name}"
        );
        Ok(value)
    };
    ensure!(
        report["backend"] == decision.backend
            && report["model"] == decision.model
            && report["backend_fingerprint"] == decision.fingerprint()?
            && report["policy"] == decision.policy
            && report["purpose"] == "delivery:veto"
            && report["catalog"] == "delivery-v1"
            && report["delivery_policy_hash"] == policy_hash
            && report["minimum_routine_probability"].as_f64()
                == Some(policy.minimum_routine_probability)
            && held["cases"].as_u64().is_some_and(|v| v >= 50),
        "qualification report identity or sample size is invalid"
    );
    ensure!(
        metric("candidate_quality")? >= metric("baseline_quality")?
            && metric("candidate_critical_defect_recall")?
                >= metric("baseline_critical_defect_recall")?,
        "held-out delivery quality regressed"
    );
    let baseline = held["baseline_verified_completion_ms"]
        .as_u64()
        .context("baseline latency")?;
    let candidate = held["candidate_verified_completion_ms"]
        .as_u64()
        .context("candidate latency")?;
    ensure!(
        candidate > 0 && candidate < baseline,
        "held-out verified completion did not improve"
    );
    Ok(())
}

pub(super) fn scoped(i: &Invocation<'_>, policy: &AutomaticDelivery) -> Result<String> {
    let tree =
        i.db.rows(
            "SELECT root,parent,depth FROM task_tree WHERE task=?",
            &[&i.task],
        )?
        .into_iter()
        .next()
        .context("missing task tree")?;
    ensure!(
        tree["root"] == i.task && tree["parent"].is_null() && tree["depth"] == 0,
        "automatic delivery is root-only"
    );
    let remote: i64 = i.db.conn.query_row(
        "SELECT COUNT(*) FROM remote_links WHERE task=?",
        [i.task],
        |r| r.get(0),
    )?;
    let remote_origin: i64 = i.db.conn.query_row(
        "SELECT COUNT(*) FROM remote_origins WHERE task=?",
        [i.task],
        |r| r.get(0),
    )?;
    ensure!(
        remote == 0 && remote_origin == 0,
        "remote tasks cannot deliver automatically"
    );
    let d = &i.settings.delivery;
    ensure!(
        policy
            .repositories
            .iter()
            .any(|v| v.eq_ignore_ascii_case(&d.repository))
            && policy.bases.contains(&d.base),
        "repository or base is outside automatic-delivery scope"
    );
    let environment = d
        .environment
        .as_deref()
        .context("automatic delivery needs an environment")?;
    ensure!(
        policy.environments.iter().any(|v| v == environment),
        "environment is outside automatic-delivery scope"
    );
    ensure!(
        d.deploy_workflow
            .as_ref()
            .is_some_and(|v| policy.deploy_workflows.contains(v))
            && policy.version_url.is_some()
            && policy.smoke_url.is_some(),
        "automatic delivery needs a push deployment workflow, version endpoint, and smoke endpoint"
    );
    Ok(environment.to_owned())
}
