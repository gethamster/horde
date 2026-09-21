//! Operator-owned, default-off authority for routine automatic delivery.
//! Facts are checked independently of Jev. Its answer can only veto an eligible merge.
use crate::{
    config::{AutomaticDelivery, DecisionMode, Settings},
    decision::{
        Answer, ChoiceQuestion, DecisionRequest, Question, store as decisions, typesafe::TypeSafe,
    },
    executor::Invocation,
    store::{Store, hash, id, now},
};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::time::Instant;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

const DELIVERY_NAMES: &[&str] = &[
    "pr",
    "checks",
    "merge",
    "deployment",
    "health",
    "version",
    "smoke",
];

pub fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS delivery_preflights(
        task TEXT PRIMARY KEY REFERENCES tasks(id), data TEXT NOT NULL, created INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS delivery_authorizations(
        id TEXT PRIMARY KEY, task TEXT NOT NULL REFERENCES tasks(id),
        head TEXT NOT NULL, data TEXT NOT NULL, created INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS delivery_authorizations_task ON delivery_authorizations(task,created);
    CREATE TABLE IF NOT EXISTS delivery_merge_intents(
        task TEXT PRIMARY KEY REFERENCES tasks(id), authorization TEXT NOT NULL REFERENCES delivery_authorizations(id),
        pr TEXT NOT NULL, head TEXT NOT NULL, created INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS delivery_qualifications(
        id TEXT PRIMARY KEY, backend TEXT NOT NULL, model TEXT NOT NULL,
        policy TEXT NOT NULL, purpose TEXT NOT NULL, catalog TEXT NOT NULL,
        delivery_policy_hash TEXT NOT NULL, threshold REAL NOT NULL,
        evidence_hash TEXT NOT NULL REFERENCES artifacts(hash),
        held_out_passed INTEGER NOT NULL CHECK(held_out_passed IN (0,1)), created INTEGER NOT NULL
    );")?;
    Ok(())
}

fn json_hash(value: &Value) -> Result<String> {
    Ok(hash(&serde_json::to_vec(value)?))
}

mod qualification;
use qualification::*;
mod evidence;
pub use evidence::preflight;
use evidence::*;

fn check_runs(value: &Value, head: &str, required: &[String]) -> Result<Value> {
    let total = value["total_count"].as_u64().context("check run count")?;
    ensure!(total <= 100, "check run list exceeds one complete page");
    let runs = value["check_runs"].as_array().context("check runs")?;
    ensure!(runs.len() == total as usize, "incomplete check run list");
    let mut by_name = BTreeMap::<String, &Value>::new();
    for run in runs {
        let name = run["name"].as_str().context("check run name")?;
        if required.iter().any(|v| v == name) {
            ensure!(
                by_name.insert(name.to_owned(), run).is_none(),
                "duplicate required check run"
            );
        }
    }
    let mut evidence = Vec::new();
    for name in required {
        let run = by_name.get(name).context("required check is missing")?;
        ensure!(
            run["head_sha"] == head
                && run["status"] == "completed"
                && run["conclusion"] == "success",
            "required check did not pass on exact head: {name}"
        );
        evidence.push(json!({"id":run["id"],"name":name,"head_sha":head,
            "status":run["status"],"conclusion":run["conclusion"],
            "completed_at":run["completed_at"]}));
    }
    ensure!(
        evidence.iter().all(|v| v["id"].as_u64().is_some()),
        "required check run lacks identity"
    );
    evidence.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(json!(evidence))
}

fn approvals(value: &Value, head: &str, author: &str, minimum: usize) -> Result<Value> {
    let reviews = value.as_array().context("PR reviews")?;
    ensure!(reviews.len() < 100, "review list may be paginated");
    let mut latest = BTreeMap::<String, (&str, u64, &Value)>::new();
    for row in reviews {
        let user = row["user"]["login"]
            .as_str()
            .context("reviewer login")?
            .to_ascii_lowercase();
        let Some(submitted) = row["submitted_at"].as_str() else {
            continue;
        };
        let review_id = row["id"].as_u64().context("review id")?;
        if latest
            .get(&user)
            .is_none_or(|(time, id, _)| (submitted, review_id) > (*time, *id))
        {
            latest.insert(user, (submitted, review_id, row));
        }
    }
    ensure!(
        !latest
            .values()
            .any(|(_, _, row)| row["state"] == "CHANGES_REQUESTED"),
        "an independent reviewer requested changes"
    );
    let accepted = latest
        .into_iter()
        .filter(|(user, (_, _, row))| {
            !user.eq_ignore_ascii_case(author)
                && row["state"] == "APPROVED"
                && row["commit_id"] == head
        })
        .map(|(user, (submitted, id, row))| {
            json!({"user":user,"id":id,
            "commit_id":row["commit_id"],"state":row["state"],"submitted_at":submitted})
        })
        .collect::<Vec<_>>();
    ensure!(
        accepted.len() >= minimum,
        "missing exact-head independent PR approvals"
    );
    Ok(json!(accepted))
}

fn clean_workspace(i: &Invocation<'_>, head: &str) -> Result<()> {
    ensure!(
        crate::git::run(i.workspace, &["rev-parse", "HEAD"])? == head,
        "integrated head changed"
    );
    ensure!(
        crate::git::run(
            i.workspace,
            &["status", "--porcelain", "--untracked-files=all"]
        )?
        .is_empty(),
        "integrated workspace is dirty"
    );
    Ok(())
}

async fn base_oid(i: &Invocation<'_>, repository: &str, base: &str) -> Result<String> {
    let local = crate::git::run(
        i.workspace,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/remotes/origin/{base}^{{commit}}"),
        ],
    )?;
    let encoded = base.replace('/', "%2F");
    let endpoint = format!("repos/{repository}/branches/{encoded}");
    let remote = crate::delivery::gh(i, &["api", &endpoint]).await?;
    let actual = remote["commit"]["sha"]
        .as_str()
        .context("GitHub base SHA")?;
    ensure!(
        actual == local,
        "local base ref is stale relative to GitHub"
    );
    let protection = crate::delivery::gh(i, &["api", &format!("{endpoint}/protection")]).await?;
    ensure!(
        strict_protection(&protection, &i.settings.automatic_delivery),
        "automatic merge requires strict up-to-date branch protection enforced for admins"
    );
    Ok(local)
}

fn strict_protection(protection: &Value, policy: &AutomaticDelivery) -> bool {
    let status = &protection["required_status_checks"];
    let reviews = &protection["required_pull_request_reviews"];
    let bypass = &reviews["bypass_pull_request_allowances"];
    let protected_names = status["contexts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .chain(
            status["checks"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v["context"].as_str()),
        )
        .collect::<BTreeSet<_>>();
    status["strict"] == true
        && policy
            .required_checks
            .iter()
            .all(|name| protected_names.contains(name.as_str()))
        && protection["enforce_admins"]["enabled"] == true
        && reviews["required_approving_review_count"]
            .as_u64()
            .is_some_and(|n| n as usize >= policy.minimum_approvals)
        && ["users", "teams", "apps"]
            .iter()
            .all(|name| bypass[name].as_array().is_some_and(Vec::is_empty))
}

fn preflight_snapshot(i: &Invocation<'_>, head: &str) -> Result<Value> {
    let raw: String = i.db.conn.query_row(
        "SELECT data FROM delivery_preflights WHERE task=?",
        [i.task],
        |r| r.get(0),
    )?;
    let snapshot: Value = serde_json::from_str(&raw)?;
    let (revision, context) = revision_and_context(i)?;
    ensure!(
        snapshot["head"] == head
            && snapshot["revision"] == revision
            && snapshot["context_version"] == context
            && snapshot["non_delivery_ops_hash"] == non_delivery_ops(i.db, i.task)?,
        "automatic-delivery review snapshot is stale"
    );
    let review_id = snapshot["review"]["id"].as_str().context("review id")?;
    let review: Value = i.db.rows("SELECT r.head,r.revision,r.context_version,r.coverage,r.evidence_hash,r.manifest_hash,d.state,d.result
        FROM review_checkpoints r JOIN decisions d ON d.id=r.id WHERE r.id=? AND r.task=?", &[&review_id,&i.task])?
        .into_iter().next().context("review checkpoint vanished")?;
    ensure!(
        review["head"] == head
            && review["revision"] == revision
            && review["context_version"] == context
            && review["state"] == "succeeded"
            && review["evidence_hash"] == snapshot["review"]["evidence_hash"]
            && review["manifest_hash"] == snapshot["review"]["manifest_hash"],
        "review checkpoint changed"
    );
    let coverage: Value =
        serde_json::from_str(review["coverage"].as_str().context("review coverage")?)?;
    ensure!(
        coverage["complete"] == true,
        "review coverage is incomplete"
    );
    let signals: Value = serde_json::from_str(review["result"].as_str().context("review result")?)?;
    ensure!(
        review_signals_clear(
            &signals,
            i.settings.automatic_delivery.minimum_review_confidence
        ) && snapshot["review"]["signals_hash"] == json_hash(&signals)?,
        "review signals changed or flagged an unresolved concern"
    );
    ensure!(
        independent_review(i, head, revision)? == snapshot["independent_review"],
        "independent review evidence changed"
    );
    Ok(snapshot)
}

fn jev_request(i: &Invocation<'_>, facts: &Value) -> Result<DecisionRequest> {
    let request = DecisionRequest { model:i.settings.decision.model.clone(), state:facts.clone(),
        questions: vec![Question::Choice(ChoiceQuestion {id:"delivery".into(),
            question:"Should this already policy-eligible routine change be escalated for manual delivery? Escalate on uncertainty, consequential API or migration effects, or security concerns.".into(),
            options:vec!["routine".into(),"escalate".into()]})] };
    request.validate()?;
    Ok(request)
}

async fn jev_veto(i: &Invocation<'_>, facts: &Value, minimum: f64) -> Result<String> {
    let request = jev_request(i, facts)?;
    let decision_id = id();
    let admitted = decisions::enqueue_bounded(
        i.db,
        &decisions::QueuedDecision {
            id: decision_id.clone(),
            task: i.task.into(),
            step: Some(i.step.into()),
            attempt: Some(i.attempt.into()),
            purpose: "delivery:veto".into(),
            policy: i.settings.decision.policy.clone(),
            backend: i.settings.decision.backend.clone(),
            model: i.settings.decision.model.clone(),
            baseline: Some("manual".into()),
        },
        i.settings.decision.max_decisions_per_task,
    )?;
    ensure!(admitted, "task decision limit reached");
    let request_hash = json_hash(&crate::decision::wire_request(&request))?;
    decisions::start(
        i.db,
        &decision_id,
        &decisions::PreparedDecision {
            state_hash: json_hash(facts)?,
            context_version: facts["context_version"]
                .as_i64()
                .context("context version")?,
            policy_hash: json_hash(&serde_json::to_value(&i.settings.automatic_delivery)?)?,
            catalog_hash: hash(b"delivery-v1"),
            candidate_hashes: json!({}),
            backend_fingerprint: hash(
                format!(
                    "{}:{}",
                    i.settings.decision.backend, i.settings.decision.model
                )
                .as_bytes(),
            ),
            evidence_hash: json_hash(facts)?,
            request_hash,
            cache_hash: String::new(),
            artifact_hash: None,
        },
    )?;
    let backend = TypeSafe::new(i.settings.decision.clone())?;
    let start = Instant::now();
    let outcome = backend
        .decide_counted_with(&request, |_| {
            let _ = decisions::attempt_started(i.db, &decision_id);
        })
        .await;
    let (response, attempts) = match outcome {
        Ok(ok) => ok,
        Err((error, attempts)) => {
            decisions::finish_state(
                i.db,
                &decision_id,
                "failed",
                "provider_unavailable",
                attempts,
            )?;
            return Err(error).context("delivery Jev veto unavailable");
        }
    };
    let (answer, probability) = match response.answers.first() {
        Some(Answer::Choice {
            answer,
            probabilities,
            ..
        }) => (answer.as_str(), probabilities["routine"]),
        _ => bail!("delivery Jev response omitted choice"),
    };
    let result = serde_json::to_value(&response)?;
    decisions::complete(
        i.db,
        &decision_id,
        &decisions::CompletedDecision {
            result: &result,
            proposed: Some(answer),
            abstention: answer != "routine",
            provider_ms: start.elapsed().as_millis() as u64,
            attempts,
            usage: &serde_json::to_value(&response.usage)?,
        },
    )?;
    ensure!(
        answer == "routine" && probability >= minimum,
        "Jev escalated automatic delivery"
    );
    Ok(decision_id)
}

/// Re-read the PR, head-bound checks, approvals, task state and policy at the
/// immediate merge boundary. The returned immutable row is written before gh merge.
pub async fn authorize_merge(i: &Invocation<'_>, head: &str, number: &str) -> Result<Value> {
    active_delivery_attempt(i)?;
    let policy = current_policy(i)?.context("automatic delivery disabled")?;
    let environment = scoped(i, &policy)?;
    clean_workspace(i, head)?;
    let review = preflight_snapshot(i, head)?;
    let (paths, manifest) = routine_manifest(i.workspace, &i.settings.delivery.base, head)?;
    validate_paths(&paths, &policy.allowed_paths)?;
    let d = &i.settings.delivery;
    let pr = crate::delivery::gh(
        i,
        &[
            "pr",
            "view",
            number,
            "--repo",
            &d.repository,
            "--json",
            "number,url,state,headRefOid,baseRefName,author",
        ],
    )
    .await?;
    ensure!(
        pr["state"] == "OPEN"
            && pr["headRefOid"] == head
            && pr["baseRefName"] == d.base
            && pr["number"].as_u64() == number.parse::<u64>().ok(),
        "PR identity changed before automatic merge"
    );
    let author = pr["author"]["login"].as_str().context("PR author")?;
    let base_sha = base_oid(i, &d.repository, &d.base).await?;
    let endpoint = format!(
        "repos/{}/commits/{head}/check-runs?per_page=100",
        d.repository
    );
    let checks = check_runs(
        &crate::delivery::gh(i, &["api", &endpoint]).await?,
        head,
        &policy.required_checks,
    )?;
    let endpoint = format!("repos/{}/pulls/{number}/reviews?per_page=100", d.repository);
    let approved = approvals(
        &crate::delivery::gh(i, &["api", &endpoint]).await?,
        head,
        author,
        policy.minimum_approvals,
    )?;
    let facts = json!({"head":head,"repo":d.repository,"base":d.base,"base_oid":base_sha,"environment":environment,
        "paths":paths,"manifest":redacted_manifest(i, &manifest)?,"checks":checks,"approvals":approved,
        "review":review["review"],"independent_review":review["independent_review"],
        "context_version":review["context_version"]});
    let decision = jev_veto(i, &facts, policy.minimum_routine_probability).await?;
    // A provider call may have overlapped an edit or policy change.
    current_policy(i)?.context("automatic delivery disabled")?;
    clean_workspace(i, head)?;
    preflight_snapshot(i, head)?;
    ensure!(
        routine_manifest(i.workspace, &d.base, head)?.1 == manifest,
        "patch changed during delivery decision"
    );
    ensure!(
        base_oid(i, &d.repository, &d.base).await? == base_sha,
        "base branch changed during delivery decision"
    );
    let pr_again = crate::delivery::gh(
        i,
        &[
            "pr",
            "view",
            number,
            "--repo",
            &d.repository,
            "--json",
            "number,url,state,headRefOid,baseRefName,author",
        ],
    )
    .await?;
    ensure!(pr_again == pr, "PR changed during delivery decision");
    let endpoint = format!(
        "repos/{}/commits/{head}/check-runs?per_page=100",
        d.repository
    );
    ensure!(
        check_runs(
            &crate::delivery::gh(i, &["api", &endpoint]).await?,
            head,
            &policy.required_checks
        )? == checks,
        "checks changed during delivery decision"
    );
    let endpoint = format!("repos/{}/pulls/{number}/reviews?per_page=100", d.repository);
    ensure!(
        approvals(
            &crate::delivery::gh(i, &["api", &endpoint]).await?,
            head,
            author,
            policy.minimum_approvals
        )? == approved,
        "approvals changed during delivery decision"
    );
    active_delivery_attempt(i)?;
    let details = json!({"facts":facts,"decision":decision,"pr":number,"head":head,
        "policy_hash":json_hash(&serde_json::to_value(&policy)?)?,"merge":"squash",
        "deploy_workflow":d.deploy_workflow,"version_url":policy.version_url});
    let identity = json_hash(&details)?;
    i.db.conn.execute("INSERT OR IGNORE INTO delivery_authorizations(id,task,head,data,created) VALUES(?,?,?,?,?)",
        params![identity,i.task,head,details.to_string(),now()])?;
    i.db.event(
        i.task,
        "delivery.authorized",
        json!({"authorization":identity,"head":head,"pr":number}),
    )?;
    Ok(json!({"id":identity,"head":head,"decision":decision}))
}

fn active_delivery_attempt(i: &Invocation<'_>) -> Result<()> {
    let active: i64 = i.db.conn.query_row(
        "SELECT COUNT(*) FROM attempts a
         JOIN steps s ON s.id=a.step JOIN tasks t ON t.id=s.task
         WHERE t.id=? AND t.status='running' AND s.id=? AND s.state='running'
           AND a.id=? AND a.state='running'",
        params![i.task, i.step, i.attempt],
        |row| row.get(0),
    )?;
    ensure!(
        active == 1,
        "delivery task, step, or attempt is no longer active"
    );
    Ok(())
}

/// This runs after the durable merge intent and immediately before the external write.
/// A changed policy or cancelled attempt leaves the intent for read-only reconciliation.
pub fn validate_merge_boundary(i: &Invocation<'_>, authorization: &Value) -> Result<()> {
    let policy = current_policy(i)?.context("automatic delivery disabled")?;
    let row =
        i.db.rows(
            "SELECT data FROM delivery_authorizations WHERE id=? AND task=? AND head=?",
            &[
                &authorization["id"].as_str().context("authorization id")?,
                &i.task,
                &authorization["head"]
                    .as_str()
                    .context("authorization head")?,
            ],
        )?
        .into_iter()
        .next()
        .context("delivery authorization missing")?;
    let details: Value = serde_json::from_str(row["data"].as_str().context("authorization data")?)?;
    ensure!(
        details["policy_hash"] == json_hash(&serde_json::to_value(&policy)?)?,
        "operator delivery policy changed before merge"
    );
    active_delivery_attempt(i)
}

pub fn select_deployment_run(runs: &Value, sha: &str, base: &str) -> Result<Value> {
    let rows = runs.as_array().context("deployment runs")?;
    ensure!(rows.len() < 100, "deployment run list may be paginated");
    let matches = rows
        .iter()
        .filter(|v| {
            v["headSha"] == sha
                && v["headBranch"] == base
                && v["event"] == "push"
                && v["databaseId"].as_u64().is_some()
        })
        .collect::<Vec<_>>();
    ensure!(
        matches.len() <= 1,
        "multiple deployment runs match the merge identity"
    );
    Ok(matches.first().cloned().cloned().unwrap_or(Value::Null))
}

pub fn require_prior_authorization(i: &Invocation<'_>, head: &str, pr: &str) -> Result<String> {
    preflight_snapshot(i, head)?;
    let row =
        i.db.rows(
            "SELECT m.authorization,m.pr,m.head,a.data FROM delivery_merge_intents m
        JOIN delivery_authorizations a ON a.id=m.authorization AND a.task=m.task AND a.head=m.head
        WHERE m.task=?",
            &[&i.task],
        )?
        .into_iter()
        .next()
        .context("merged PR has no recorded automatic-delivery intent")?;
    ensure!(
        row["head"] == head && row["pr"] == pr,
        "merge intent PR or head changed"
    );
    let authorization: Value =
        serde_json::from_str(row["data"].as_str().context("authorization data")?)?;
    ensure!(
        authorization["head"] == head
            && authorization["pr"] == row["pr"]
            && authorization["policy_hash"]
                == json_hash(&serde_json::to_value(&i.settings.automatic_delivery)?)?,
        "merge intent does not match authorization"
    );
    let decision_id = authorization["decision"]
        .as_str()
        .context("delivery decision")?;
    let decision = i.db.rows("SELECT state,proposed,result FROM decisions WHERE id=? AND task=? AND purpose='delivery:veto'",
        &[&decision_id,&i.task])?.into_iter().next().context("delivery decision missing")?;
    ensure!(
        decision["state"] == "succeeded" && decision["proposed"] == "routine",
        "delivery decision did not authorize routine delivery"
    );
    let base = authorization["facts"]["base_oid"]
        .as_str()
        .context("authorized base SHA")?;
    Ok(base.to_owned())
}

pub fn merge_parent_matches(value: &Value, base_sha: &str) -> bool {
    value["parents"]
        .as_array()
        .is_some_and(|parents| parents.len() == 1 && parents[0]["sha"] == base_sha)
}

pub fn record_merge_intent(
    i: &Invocation<'_>,
    authorization: &Value,
    pr: &str,
    head: &str,
) -> Result<()> {
    let identity = authorization["id"]
        .as_str()
        .context("authorization identity")?;
    let row =
        i.db.rows(
            "SELECT data FROM delivery_authorizations WHERE id=? AND task=? AND head=?",
            &[&identity, &i.task, &head],
        )?
        .into_iter()
        .next()
        .context("authorization row missing")?;
    let details: Value = serde_json::from_str(row["data"].as_str().context("authorization data")?)?;
    ensure!(
        details["pr"] == pr && details["decision"] == authorization["decision"],
        "merge intent does not match authorization"
    );
    i.db.conn.execute("INSERT OR IGNORE INTO delivery_merge_intents(task,authorization,pr,head,created) VALUES(?,?,?,?,?)",
        params![i.task,identity,pr,head,now()])?;
    let original =
        i.db.rows(
            "SELECT authorization,pr,head FROM delivery_merge_intents WHERE task=?",
            &[&i.task],
        )?
        .into_iter()
        .next()
        .context("merge intent missing")?;
    ensure!(
        original["authorization"] == identity && original["pr"] == pr && original["head"] == head,
        "automatic merge intent differs from prior attempt"
    );
    Ok(())
}

async fn fetch_assertion(url: &str) -> Result<Value> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let response = client.get(url).send().await?.error_for_status()?;
    ensure!(
        response.content_length().is_none_or(|n| n <= 4096),
        "version response too large"
    );
    let bytes = response.bytes().await?;
    ensure!(bytes.len() <= 4096, "version response too large");
    Ok(serde_json::from_slice(&bytes)?)
}

pub async fn verify_version(url: &str, sha: &str, environment: &str) -> Result<()> {
    let value = fetch_assertion(url).await?;
    ensure!(
        value["commit"] == sha && value["environment"] == environment,
        "deployment version does not match merge commit and environment"
    );
    Ok(())
}

pub async fn verify_smoke(url: &str, sha: &str, environment: &str) -> Result<()> {
    let value = fetch_assertion(url).await?;
    ensure!(
        value["ok"] == true && value["commit"] == sha && value["environment"] == environment,
        "application smoke check failed for deployed commit and environment"
    );
    Ok(())
}

#[cfg(test)]
mod tests;
