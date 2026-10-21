//! Read-only RPC collection. No SQL, account mutation, execution or enrollment.
use super::*;
use std::{io::BufRead, os::unix::net::UnixStream, time::Duration};
#[path = "facts.rs"]
mod facts;

fn request(root: &Path, project: &str, method: &str, args: Value) -> Result<Value> {
    let mut socket = UnixStream::connect(root.join("daemon.sock"))?;
    socket.set_read_timeout(Some(Duration::from_secs(30)))?;
    socket.set_write_timeout(Some(Duration::from_secs(30)))?;
    writeln!(
        socket,
        "{}",
        json!({"method":method,"args":args,"project":project,
        "token":crate::branding::var("HORDE_WORKER_TOKEN").ok()})
    )?;
    let mut line = String::new();
    std::io::BufReader::new(socket)
        .take(16 * 1024 * 1024 + 1)
        .read_line(&mut line)?;
    ensure!(
        line.len() <= 16 * 1024 * 1024,
        "qualification RPC evidence exceeds 16 MiB"
    );
    let response: Value = serde_json::from_str(&line)?;
    ensure!(
        response.get("error").is_none(),
        "qualification read-only RPC {method} failed"
    );
    response
        .get("result")
        .cloned()
        .context("missing qualification RPC result")
}

fn pages(cursor: &str, mut read_page: impl FnMut(i64) -> Result<Value>) -> Result<Vec<Value>> {
    let mut after = 0;
    let mut result = Vec::new();
    loop {
        let page = read_page(after)?;
        let rows = page.as_array().context("invalid evidence page")?;
        if rows.is_empty() {
            return Ok(result);
        }
        for row in rows {
            let next = row[cursor].as_i64().context("evidence cursor missing")?;
            ensure!(next > after, "evidence pagination did not advance");
            after = next;
            result.push(row.clone());
        }
        ensure!(
            result.len() <= 100_000,
            "evidence pagination exceeds supported memory bound"
        );
    }
}

fn snapshot(root: &Path, project: &str, task: &str, base: &str) -> Result<Value> {
    let get = |method: &str, mut args: Value| {
        args["task"] = json!(task);
        request(root, project, method, args)
    };
    let inspection = get("inspect", json!({}))?;
    let summary = get("summary", json!({}))?;
    let events = pages("seq", |after| get("events", json!({"after":after})))?;
    let decisions = pages("seq", |after| {
        get("decisions", json!({"after":after,"limit":200}))
    })?;
    let reviews = pages("event_seq", |after| {
        get("reviews", json!({"after":after,"limit":200}))
    })?;
    let changes = get(
        "run_changes",
        json!({"expected_base":base,"expected_head":summary["integrated_head"]}),
    )?;
    let review_hashes: BTreeSet<_> = reviews
        .iter()
        .filter_map(|review| review["evidence_hash"].as_str())
        .collect();
    let mut artifacts = serde_json::Map::new();
    for link in inspection["artifacts"]
        .as_array()
        .context("Run artifact links unavailable")?
    {
        if ![
            "qualification-inputs",
            "qualification-oracle",
            "qualification-decision",
            "qualification-decision-transport",
            "qualification-github-receipts",
        ]
        .contains(&link["name"].as_str().unwrap_or(""))
            && !review_hashes.contains(link["hash"].as_str().unwrap_or(""))
        {
            continue;
        }
        let hash = link["hash"].as_str().context("artifact hash missing")?;
        ensure!(hex(hash, 64), "invalid Run artifact hash");
        let artifact = get("get_artifact", json!({"hash":hash}))?;
        let content = artifact["content"]
            .as_str()
            .context("artifact content missing")?;
        ensure!(
            artifact["hash"] == hash && crate::store::hash(content.as_bytes()) == hash,
            "Run artifact integrity failure"
        );
        artifacts.insert(hash.into(), json!(content));
    }
    let latest = get("summary", json!({}))?;
    ensure!(
        summary["integrated_head"] == latest["integrated_head"]
            && summary["status"] == latest["status"],
        "Run changed during evidence collection"
    );
    // Omit conversation and host observation surfaces. Retain authoritative source
    // receipts needed to audit identity, quality, real terminal timing and decisions.
    let events: Vec<_> = events
        .into_iter()
        .filter(|event| {
            matches!(
                event["kind"].as_str(),
                Some(
                    "task.submitted"
                        | "task.finished"
                        | "task.cancelled"
                        | "run.checkpoint_verified"
                        | "workflow.revised"
                        | "question.answered"
                )
            )
        })
        .collect();
    Ok(
        json!({"task":inspection["task"],"workers":inspection["workers"],"steps":inspection["steps"],"integrations":inspection["integrations"],"questions":inspection["questions"],"changes":changes,"artifact_links":inspection["artifacts"],
        "attempts":inspection["attempts"],"events":events,"decisions":decisions,"reviews":reviews,"artifacts":artifacts,
        "summary":{"task":summary["task"],"project":summary["project"],"status":summary["status"],
            "integrated_head":summary["integrated_head"],"run": {"run_id":summary["run"]["run_id"],"checkpoint":summary["run"]["checkpoint"]},
            "upstream_maintenance":summary["upstream_maintenance"]}}),
    )
}

pub fn collect(
    frozen_path: &Path,
    runtime_root: &Path,
    project: &str,
    output: &Path,
) -> Result<Value> {
    use std::os::unix::fs::DirBuilderExt;
    label(project)?;
    let frozen = frozen(frozen_path)?;
    if !output.exists() {
        std::fs::DirBuilder::new().mode(0o700).create(output)?;
    }
    ensure!(output.is_dir(), "evidence destination is not a directory");
    let mut runs = Vec::new();
    for case in &frozen.manifest.cases {
        for (arm, task) in [
            ("baseline", &case.baseline_task),
            ("candidate", &case.candidate_task),
        ] {
            let evidence = snapshot(runtime_root, project, task, &case.base_commit)?;
            let hash = digest(&evidence)?;
            let path = output.join(format!("{hash}.json"));
            if path.exists() {
                ensure!(
                    digest(&read(&path)?)? == hash,
                    "retained Run evidence integrity failure"
                );
            } else {
                persist(&path, &evidence)?;
            }
            runs.push(json!({"case_id":case.id,"arm":arm,"task":task,"sha256":hash}));
        }
    }
    let index = json!({"schema_version":1,"manifest_hash":frozen.manifest_hash,"project":project,"runs":runs});
    let path = output.join("index.json");
    if path.exists() {
        ensure!(
            read(&path)? == index,
            "evidence collection already frozen with different Runs"
        );
    } else {
        persist(&path, &index)?;
    }
    Ok(
        json!({"manifest_hash":frozen.manifest_hash,"runs":runs.len(),"evidence_hash":digest(&index)?,"enrolled":false}),
    )
}

fn raw_artifact(receipt: &Value, hash: &str) -> Result<Value> {
    ensure!(
        receipt["artifact_links"]
            .as_array()
            .context("artifact links missing")?
            .iter()
            .any(|link| link["hash"] == hash),
        "artifact is not linked to the real Run"
    );
    let raw = receipt["artifacts"]
        .get(hash)
        .and_then(Value::as_str)
        .context("retained original artifact bytes missing")?;
    ensure!(
        hex(hash, 64) && crate::store::hash(raw.as_bytes()) == hash,
        "original Run artifact integrity failure"
    );
    Ok(serde_json::from_str(raw)?)
}
fn artifact<'a>(receipt: &'a Value, name: &str) -> Result<(&'a str, Value)> {
    let links: Vec<_> = receipt["artifact_links"]
        .as_array()
        .context("artifact links missing")?
        .iter()
        .filter(|link| link["name"] == name)
        .collect();
    ensure!(
        links.len() == 1 && links[0]["verified"] == 1,
        "missing or ambiguous operator-verified {name} artifact"
    );
    let hash = links[0]["hash"].as_str().context("artifact hash missing")?;
    Ok((hash, raw_artifact(receipt, hash)?))
}

struct Outcome {
    quality: bool,
    recalled: bool,
    elapsed_ms: u64,
}

fn outcome(frozen: &Frozen, case: &Case, arm: &str, receipt: &Value) -> Result<Outcome> {
    let task = if arm == "baseline" {
        &case.baseline_task
    } else {
        &case.candidate_task
    };
    let summary = &receipt["summary"];
    ensure!(
        receipt["task"]["id"] == *task
            && summary["task"] == *task
            && summary["run"]["run_id"] == *task
            && receipt["task"]["objective"] == case.objective,
        "Run identity or frozen objective mismatch"
    );
    ensure!(
        ["succeeded", "failed"].contains(&summary["status"].as_str().unwrap_or("")),
        "Run has no final evaluated outcome"
    );
    let head = summary["integrated_head"]
        .as_str()
        .context("Run head unavailable")?;
    let checkpoint = &summary["run"]["checkpoint"];
    ensure!(
        hex(head, 40)
            && checkpoint["current"] == true
            && checkpoint["verified"] == true
            && checkpoint["commit_sha"] == head
            && checkpoint["expected_main_head"] == case.base_commit
            && checkpoint["validation"]
                .as_array()
                .is_some_and(|argv| !argv.is_empty())
            && digest(&checkpoint["validation"])? == case.verification_argv_sha256,
        "Run lacks a current independently verified checkpoint for the frozen base"
    );
    let (_, inputs) = artifact(receipt, "qualification-inputs")?;
    ensure!(
        inputs["manifest_hash"] == frozen.manifest_hash
            && inputs["case_id"] == case.id
            && inputs["repository"] == case.repository
            && inputs["base_commit"] == case.base_commit
            && inputs["inputs_sha256"] == case.inputs_sha256
            && inputs["objective_sha256"] == crate::store::hash(case.objective.as_bytes()),
        "real Run inputs do not match the held-out case"
    );
    let (_, oracle) = artifact(receipt, "qualification-oracle")?;
    ensure!(
        oracle["manifest_hash"] == frozen.manifest_hash
            && oracle["case_id"] == case.id
            && oracle["task"] == *task
            && oracle["head_sha"] == head
            && oracle["oracle_id"] == frozen.manifest.oracle_id
            && oracle["oracle_spec_sha256"] == frozen.manifest.oracle_spec_sha256
            && oracle["critical_defect_present"].as_bool() == Some(case.critical)
            && oracle["verified_completion"] == true,
        "independent oracle evidence is missing, stale or disagrees with frozen truth"
    );
    let reviewer = oracle["reviewer_id"]
        .as_str()
        .context("independent oracle reviewer missing")?;
    label(reviewer)?;
    ensure!(
        !receipt["workers"]
            .as_array()
            .context("Run worker identities missing")?
            .iter()
            .any(|worker| worker["id"] == reviewer),
        "oracle reviewer is a Run worker"
    );
    let quality = oracle["quality_passed"]
        .as_bool()
        .context("independent quality verdict missing")?;
    let recalled = oracle["critical_defect_detected"]
        .as_bool()
        .context("independent defect verdict missing")?;
    let created = receipt["task"]["created"]
        .as_i64()
        .context("actual Run start unavailable")?;
    let events = receipt["events"]
        .as_array()
        .context("actual Run lifecycle unavailable")?;
    let finished = events
        .iter()
        .rev()
        .find(|event| event["kind"] == "task.finished")
        .context("actual terminal Run event unavailable")?;
    let terminal: Value = match &finished["data"] {
        Value::String(raw) => serde_json::from_str(raw)?,
        value => value.clone(),
    };
    ensure!(
        terminal["integrated_head"] == head && terminal["status"] == summary["status"],
        "Run completion event has a different final identity"
    );
    let finished_at = finished["created"]
        .as_i64()
        .context("actual Run finish unavailable")?;
    let elapsed = finished_at
        .checked_sub(created)
        .context("Run timing overflow")?;
    ensure!(
        created > 0 && elapsed > 0,
        "Run lacks positive actual verified-completion timing"
    );
    if arm == "baseline" {
        facts::baseline(case, receipt, &oracle, head, created, finished_at, recalled)?;
    }
    if arm == "candidate" {
        let first_attempt = receipt["attempts"]
            .as_array()
            .context("candidate attempt receipts missing")?
            .iter()
            .filter_map(|attempt| attempt["started"].as_i64())
            .min()
            .context("candidate has no real execution attempt")?;
        ensure!(
            first_attempt > frozen.frozen_at,
            "candidate execution predates held-out corpus freeze"
        );
        ensure!(
            summary["upstream_maintenance"]["binding"]["budget"]["reference"]
                == frozen.manifest.spend_reference,
            "candidate has no matching approved worker execution budget receipt"
        );
        let settings: crate::config::Settings = serde_json::from_str(
            receipt["task"]["settings"]
                .as_str()
                .context("candidate settings receipt missing")?,
        )?;
        ensure!(
            settings.decision.fingerprint()? == frozen.manifest.decision.fingerprint()?,
            "candidate Decision service identity differs from the frozen backend"
        );
        decision(frozen, case, receipt, head, recalled, created, finished_at)?;
    }
    Ok(Outcome {
        quality,
        recalled,
        elapsed_ms: u64::try_from(elapsed)?
            .checked_mul(1000)
            .context("Run milliseconds overflow")?,
    })
}

fn decision(
    frozen: &Frozen,
    case: &Case,
    receipt: &Value,
    head: &str,
    recalled: bool,
    created: i64,
    finished_at: i64,
) -> Result<()> {
    // Bootstrap evaluation is advisory only. An operator must review and retain
    // the actual request/response and redacted transport trace, never invented rows
    // in delivery_qualifications or a qualification-shaped worker metric report.
    let (_, observation) = artifact(receipt, "qualification-decision")?;
    let (transport_hash, transport) = artifact(receipt, "qualification-decision-transport")?;
    let expected = &frozen.manifest.decision;
    ensure!(
        observation["backend_fingerprint"] == expected.fingerprint()?
            && observation["delivery_policy_hash"] == digest(&frozen.manifest.delivery_policy)?
            && observation["purpose"] == "delivery:veto"
            && observation["catalog"] == "delivery-v1"
            && observation["transport_sha256"] == transport_hash
            && observation["observed_at"]
                .as_i64()
                .is_some_and(|at| at >= frozen.frozen_at && at >= created && at <= finished_at),
        "actual advisory delivery observation is unqualified or predates freeze"
    );
    let request: crate::decision::DecisionRequest =
        serde_json::from_value(observation["request"].clone())?;
    request.validate()?;
    ensure!(
        request.state
            == facts::production_state(
                frozen,
                case,
                receipt,
                head,
                created,
                observation["observed_at"]
                    .as_i64()
                    .context("advisory completion unavailable")?
            )?,
        "advisory request omitted or changed source-bound production delivery facts"
    );
    ensure!(
        request.model == expected.model
            && request.state["head"] == head
            && request.state["repo"] == case.repository
            && request.state["base_oid"] == case.base_commit,
        "advisory request used different real checkpoint facts"
    );
    let runtime_request =
        crate::delivery_policy::delivery_veto_request(&expected.model, &request.state)?;
    ensure!(
        crate::decision::wire_request(&request) == crate::decision::wire_request(&runtime_request),
        "advisory request does not use the delivery:veto contract"
    );
    let response = crate::decision::validate_response(&request, &observation["response"])?;
    ensure!(
        transport["request_sha256"] == digest(&crate::decision::wire_request(&request))?
            && transport["response_sha256"] == digest(&observation["response"])?
            && transport["http_status"] == 200
            && transport["endpoint"]
                == crate::decision::typesafe::endpoint(&expected.base_url)?.as_str(),
        "reviewed transport trace does not bind the actual endpoint/request/response"
    );
    let escalated = match response.answers.first() {
        Some(crate::decision::Answer::Choice {
            answer,
            probabilities,
            ..
        }) => {
            answer != "routine"
                || probabilities["routine"]
                    < frozen.manifest.delivery_policy.minimum_routine_probability
        }
        _ => anyhow::bail!("actual advisory response omitted the delivery choice"),
    };
    ensure!(
        !case.critical || escalated == recalled,
        "critical-defect oracle disagrees with the actual candidate decision"
    );
    Ok(())
}

pub fn assess(frozen_path: &Path, evidence: &Path) -> Result<Value> {
    let frozen = frozen(frozen_path)?;
    let index = read(&evidence.join("index.json"))?;
    ensure!(
        index["schema_version"] == 1 && index["manifest_hash"] == frozen.manifest_hash,
        "evidence belongs to another frozen corpus"
    );
    let runs = index["runs"]
        .as_array()
        .context("paired Run receipts missing")?;
    ensure!(
        runs.len() == frozen.manifest.cases.len() * 2,
        "missing or extra held-out Run pairs"
    );
    let mut outcomes = Vec::new();
    let mut blockers = Vec::new();
    let mut references = Vec::new();
    for case in &frozen.manifest.cases {
        let mut pair = Vec::new();
        for arm in ["baseline", "candidate"] {
            let result = (|| {
                let links: Vec<_> = runs
                    .iter()
                    .filter(|row| row["case_id"] == case.id && row["arm"] == arm)
                    .collect();
                ensure!(
                    links.len() == 1,
                    "Run pair identity is ambiguous or missing"
                );
                let hash = links[0]["sha256"]
                    .as_str()
                    .context("Run receipt hash missing")?;
                ensure!(hex(hash, 64), "Run receipt hash invalid");
                let receipt = read(&evidence.join(format!("{hash}.json")))?;
                ensure!(
                    digest(&receipt)? == hash,
                    "retained Run receipt integrity failure"
                );
                let result = outcome(&frozen, case, arm, &receipt)?;
                references.push(json!({"case_id":case.id,"arm":arm,"sha256":hash}));
                Ok(result)
            })();
            match result {
                Ok(result) => pair.push(result),
                Err(error) => {
                    blockers.push(json!({"case_id":case.id,"arm":arm,"reason":error.to_string()}))
                }
            }
        }
        if pair.len() == 2 {
            outcomes.push((case.critical, pair));
        }
    }
    if !blockers.is_empty() {
        return Ok(
            json!({"qualified":false,"manifest_hash":frozen.manifest_hash,"blockers":blockers,"enrolled":false}),
        );
    }
    let n = outcomes.len() as u64;
    let critical = outcomes.iter().filter(|(critical, _)| *critical).count() as f64;
    let quality = |arm: usize| {
        outcomes
            .iter()
            .filter(|(_, pair)| pair[arm].quality)
            .count() as f64
            / n as f64
    };
    let recall = |arm: usize| {
        outcomes
            .iter()
            .filter(|(critical, pair)| *critical && pair[arm].recalled)
            .count() as f64
            / critical
    };
    let latency = |arm: usize| -> Result<u64> {
        Ok(outcomes.iter().try_fold(0u64, |sum, (_, pair)| {
            sum.checked_add(pair[arm].elapsed_ms)
                .context("aggregate real timing overflow")
        })? / n)
    };
    let baseline_ms = latency(0)?;
    let candidate_ms = latency(1)?;
    let qualified = quality(1) >= quality(0)
        && recall(1) >= recall(0)
        && candidate_ms > 0
        && candidate_ms < baseline_ms;
    Ok(
        json!({"qualified":qualified,"backend":frozen.manifest.decision.backend,"model":frozen.manifest.decision.model,
        "backend_fingerprint":frozen.manifest.decision.fingerprint()?,"policy":frozen.manifest.decision.policy,"purpose":"delivery:veto","catalog":"delivery-v1",
        "delivery_policy_hash":digest(&frozen.manifest.delivery_policy)?,"minimum_routine_probability":frozen.manifest.delivery_policy.minimum_routine_probability,
        "manifest_hash":frozen.manifest_hash,"timing_source":"actual Run created and terminal events; one-second source precision",
        "held_out":{"cases":n,"baseline_quality":quality(0),"candidate_quality":quality(1),"baseline_critical_defect_recall":recall(0),"candidate_critical_defect_recall":recall(1),
            "baseline_verified_completion_ms":baseline_ms,"candidate_verified_completion_ms":candidate_ms},"evidence":references,"enrolled":false}),
    )
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
