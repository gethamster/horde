//! Bounded browser-test controller. The checked-in assertions, not Jev, decide success.
use crate::{
    config::{BrowserTestMode, DecisionMode, Settings},
    decision::{Answer, ChoiceQuestion, DecisionRequest, Question, store, typesafe::TypeSafe},
    executor::{Invocation, clean_command},
    secrets,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
};
const DRIVER: &str = include_str!("browser_driver.mjs");
const MAX_MESSAGE: u64 = 64 * 1024;
pub fn migrate(connection: &rusqlite::Connection) -> Result<()> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS browser_action_receipts(
        decision TEXT PRIMARY KEY REFERENCES decisions(id),
        task TEXT NOT NULL REFERENCES tasks(id),
        environment TEXT NOT NULL,
        attempt TEXT NOT NULL,
        tested_commit TEXT NOT NULL,
        context_version INTEGER NOT NULL,
        observation_version INTEGER NOT NULL,
        action TEXT NOT NULL,
        target_id TEXT,
        state TEXT NOT NULL CHECK(state IN ('pending','confirmed')),
        trace_hash TEXT,
        created INTEGER NOT NULL,
        confirmed INTEGER
    );
    CREATE INDEX IF NOT EXISTS browser_receipts_environment ON browser_action_receipts(task,environment);")?;
    Ok(())
}
struct Receipt<'a> {
    environment: &'a str,
    decision: &'a str,
    commit: &'a str,
    context_version: i64,
    version: u64,
    action: &'a str,
    target: Option<&'a str>,
}
fn prepare_receipt(i: &Invocation<'_>, receipt: &Receipt<'_>) -> Result<()> {
    i.db.conn.execute("INSERT INTO browser_action_receipts(decision,task,environment,attempt,tested_commit,context_version,observation_version,action,target_id,state,created) VALUES(?,?,?,?,?,?,?,?,?,'pending',?)",
        rusqlite::params![receipt.decision,i.task,receipt.environment,i.attempt,receipt.commit,receipt.context_version,receipt.version as i64,receipt.action,receipt.target,crate::store::now()])?;
    Ok(())
}
fn confirm_receipt(i: &Invocation<'_>, decision: &str) -> Result<()> {
    ensure!(i.db.conn.execute("UPDATE browser_action_receipts SET state='confirmed',confirmed=? WHERE decision=? AND task=? AND state='pending'",
        rusqlite::params![crate::store::now(),decision,i.task])? == 1,"browser action receipt changed");
    Ok(())
}
pub fn link_trace(
    db: &crate::store::Store,
    task: &str,
    environment: &str,
    attempt: &str,
    hash: &str,
) -> Result<()> {
    db.conn.execute("UPDATE browser_action_receipts SET trace_hash=? WHERE task=? AND environment=? AND attempt=? AND trace_hash IS NULL",
        rusqlite::params![hash,task,environment,attempt])?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrowserSpec {
    pub version: u8,
    pub objective: String,
    pub assertions: Vec<Assertion>,
    pub values: BTreeMap<String, String>,
    pub max_steps: usize,
    pub timeout_seconds: u64,
}
impl Default for BrowserSpec {
    fn default() -> Self {
        Self {
            version: 1,
            objective: String::new(),
            assertions: vec![],
            values: BTreeMap::new(),
            max_steps: 12,
            timeout_seconds: 120,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Assertion {
    UrlPath { path: String },
    TextVisible { text: String },
}
impl BrowserSpec {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.version == 1, "unsupported browser spec version");
        ensure!(
            !self.objective.trim().is_empty() && self.objective.len() <= 2048,
            "browser objective must contain 1..=2048 bytes"
        );
        ensure!(
            (1..=16).contains(&self.assertions.len()),
            "browser test requires 1..=16 independent assertions"
        );
        for assertion in &self.assertions {
            match assertion {
                Assertion::UrlPath { path } => ensure!(
                    path.starts_with('/') && !path.starts_with("//") && path.len() <= 512,
                    "invalid URL path assertion"
                ),
                Assertion::TextVisible { text } => ensure!(
                    !text.trim().is_empty() && text.len() <= 512,
                    "invalid visible-text assertion"
                ),
            }
        }
        ensure!(
            (1..=32).contains(&self.max_steps) && (1..=600).contains(&self.timeout_seconds),
            "browser step or time budget outside supported range"
        );
        ensure!(
            self.values.len() <= 32
                && self
                    .values
                    .iter()
                    .all(|(k, v)| !k.is_empty() && k.len() <= 128 && v.len() <= 2048),
            "invalid browser input values"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Deserialize)]
struct Control {
    id: String,
    label: String,
    operations: Vec<String>,
}
#[derive(Clone, Debug, Deserialize)]
struct Observation {
    version: u64,
    url: String,
    controls: Vec<Control>,
    #[serde(default)]
    unsupported: Vec<String>,
}
fn catalog(observation: &Observation, values: &BTreeMap<String, String>) -> Result<Vec<String>> {
    ensure!(
        observation.controls.len() <= 128,
        "browser page has too many controls"
    );
    let mut options = Vec::new();
    let mut ids = BTreeSet::new();
    for c in &observation.controls {
        ensure!(
            c.id.len() <= 16 && !c.id.is_empty() && ids.insert(c.id.as_str()),
            "invalid or duplicate browser control ID"
        );
        ensure!(c.label.len() <= 256, "browser control label too long");
        for op in &c.operations {
            ensure!(
                ["click", "fill", "select"].contains(&op.as_str()),
                "unsupported browser operation"
            );
            if op == "click" || values.contains_key(&c.label) {
                options.push(format!("{op} {}", c.id));
            }
        }
    }
    // TypeSafe accepts at most 64 labels. Escalate rather than silently omit controls.
    ensure!(
        options.len() <= 63,
        "browser action catalog exceeds Jev's 64-choice limit"
    );
    options.push("DONE".into());
    Ok(options)
}
fn selected(response: &crate::decision::DecisionResponse, options: &[String]) -> Result<String> {
    let Some(Answer::Choice {
        answer, confidence, ..
    }) = response.answers.first()
    else {
        bail!("browser decision lacked a choice")
    };
    ensure!(
        *confidence >= 0.7,
        "browser decision confidence below threshold"
    );
    ensure!(
        options.contains(answer),
        "browser decision selected an unoffered action"
    );
    Ok(answer.clone())
}
fn checked_url(value: &str, origin: &reqwest::Url) -> Result<()> {
    let parsed = reqwest::Url::parse(value)?;
    ensure!(
        parsed.origin() == origin.origin(),
        "browser navigated away from the app origin"
    );
    Ok(())
}
fn spec_from_file(workspace: &Path, path: &Path) -> Result<BrowserSpec> {
    ensure!(
        !path.is_absolute(),
        "browser spec must be relative to the workspace"
    );
    let full = workspace
        .join(path)
        .canonicalize()
        .context("browser spec not found")?;
    let workspace = workspace.canonicalize()?;
    let relative = full
        .strip_prefix(&workspace)
        .context("browser spec escapes workspace")?;
    let tracked = clean_command("git")
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(relative)
        .current_dir(&workspace)
        .output()?;
    ensure!(
        tracked.status.success(),
        "browser spec must be tracked in the tested commit"
    );
    ensure!(
        std::fs::metadata(&full)?.len() <= 64 * 1024,
        "browser spec exceeds 64 KiB"
    );
    let spec: BrowserSpec = serde_json::from_slice(&std::fs::read(full)?)?;
    spec.validate()?;
    Ok(spec)
}
async fn send(child: &mut Child, value: &Value) -> Result<()> {
    let stdin = child
        .stdin
        .as_mut()
        .context("browser driver stdin closed")?;
    stdin.write_all(value.to_string().as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await?;
    Ok(())
}
async fn receive(reader: &mut BufReader<tokio::process::ChildStdout>) -> Result<Value> {
    let mut line = Vec::new();
    let count = reader
        .take(MAX_MESSAGE + 1)
        .read_until(b'\n', &mut line)
        .await?;
    ensure!(
        count > 0 && count as u64 <= MAX_MESSAGE && line.last() == Some(&b'\n'),
        "browser driver produced an invalid or oversized message"
    );
    serde_json::from_slice(&line).context("browser driver protocol is invalid")
}
/// The browser path only accepts a fully committed workspace. The app is checked
/// again after the browser/fallback test, so the recorded commit names what ran.
pub fn verified_commit(workspace: &Path) -> Result<String> {
    let head = clean_command("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(workspace)
        .output()?;
    ensure!(head.status.success(), "browser test requires a Git commit");
    let hash = String::from_utf8(head.stdout)?.trim().to_owned();
    ensure!(
        hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid tested commit"
    );
    let dirty = clean_command("git")
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .current_dir(workspace)
        .output()?;
    ensure!(
        dirty.status.success() && dirty.stdout.is_empty(),
        "browser test requires a clean committed workspace"
    );
    Ok(hash)
}
fn redactions(values: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut augmented = values.clone();
    for (name, value) in values {
        if value.is_empty() {
            continue;
        }
        if let Ok(encoded) = serde_json::to_string(value) {
            let escaped = &encoded[1..encoded.len() - 1];
            if escaped != value {
                augmented.insert(format!("__escaped_{name}"), escaped.to_owned());
            }
        }
    }
    augmented
}
fn write_trace(
    dir: &Path,
    trace: &[Value],
    commit: &str,
    secrets: &BTreeMap<String, String>,
) -> Result<()> {
    let report = json!({"tested_commit":commit,"trace":trace});
    let bytes = serde_json::to_vec(&secrets::redact_json(&report, secrets))?;
    ensure!(bytes.len() <= 64 * 1024, "browser trace exceeds 64 KiB");
    std::fs::write(dir.join("trace.json"), bytes)?;
    Ok(())
}
pub enum BrowserOutcome {
    Passed(Value),
    Fallback(String),
}
fn fallback(reason: impl Into<String>) -> Result<BrowserOutcome> {
    Ok(BrowserOutcome::Fallback(reason.into()))
}
fn operator_mode(i: &Invocation<'_>) -> Result<BrowserTestMode> {
    let current = Settings::load_user()?;
    ensure!(
        current.decision == i.settings.decision,
        "operator decision settings changed during browser test"
    );
    ensure!(
        current.decision.mode == DecisionMode::Shadow,
        "operator Jev decisions disabled"
    );
    Ok(current.decision.browser_test_mode)
}
struct BrowserGroup(u32);
impl Drop for BrowserGroup {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}
/// Runs in the daemon after app readiness. Only the embedded Playwright driver
/// enters a subprocess; it receives neither provider credentials nor bundle values.
pub async fn run(
    i: &Invocation<'_>,
    environment_id: &str,
    spec_path: &Path,
    url: &str,
    evidence: &Path,
    bundle_values: &BTreeMap<String, String>,
    commit: &str,
) -> Result<BrowserOutcome> {
    let spec = spec_from_file(i.workspace, spec_path)?;
    let origin = reqwest::Url::parse(url)?;
    ensure!(
        ["http", "https"].contains(&origin.scheme())
            && [Some("127.0.0.1"), Some("localhost"), Some("::1")].contains(&origin.host_str()),
        "browser test requires a loopback HTTP(S) app URL"
    );
    ensure!(
        verified_commit(i.workspace)? == commit,
        "browser workspace changed before test"
    );
    if i.settings.decision.mode != DecisionMode::Shadow
        || i.settings.decision.browser_test_mode == BrowserTestMode::Disabled
    {
        return fallback("operator browser decisions disabled");
    }
    if operator_mode(i).unwrap_or(BrowserTestMode::Disabled) == BrowserTestMode::Disabled {
        return fallback("operator browser decisions disabled");
    }
    let client = TypeSafe::new(i.settings.decision.clone())?;
    let dir = tempfile::tempdir()?;
    let driver = dir.path().join("driver.mjs");
    std::fs::write(&driver, DRIVER)?;
    std::fs::create_dir_all(evidence)?;
    let mut cmd = Command::from(clean_command("node"));
    cmd.arg(&driver)
        .current_dir(i.workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .context("Node.js is required for browser tests")?;
    let group = BrowserGroup(child.id().context("browser driver PID")?);
    if let Some(identity) = crate::environment::process_identity(group.0) {
        i.db.conn.execute(
            "INSERT OR REPLACE INTO app_process_groups SELECT id,?,? FROM app_environments WHERE attempt=? AND state!='removed'",
            rusqlite::params![group.0, identity, i.attempt],
        )?;
    }
    let mut reader = BufReader::new(child.stdout.take().context("browser driver stdout")?);
    let redactions = redactions(bundle_values);
    let capture = bundle_values.values().all(String::is_empty);
    let screenshot = capture.then(|| evidence.join("screenshot.png"));
    let operation = async {
        send(&mut child, &json!({"cmd":"start","url":url,"values":spec.values,"assertions":spec.assertions,"screenshot":screenshot})).await?;
        let mut trace = vec![json!({"event":"start","tested_commit":commit})];
        write_trace(evidence, &trace, commit, &redactions)?;
        let mut previous_action = String::new();
        let mut repeats = 0;
        let mut pending_receipt: Option<String> = None;
        for step in 0..=spec.max_steps {
            let message = receive(&mut reader).await?;
            match message["kind"].as_str() {
                Some("unsupported") => {
                    trace.push(json!({"step":step,"unsupported":message["reason"]}));
                    write_trace(evidence, &trace, commit, &redactions)?;
                    return fallback(secrets::redact_values(
                        message["reason"]
                            .as_str()
                            .unwrap_or("unsupported browser control"),
                        &redactions,
                    ));
                }
                Some("result") => {
                    if let Some(id) = pending_receipt.take() {
                        ensure!(message["ack"] == id, "browser assertion receipt mismatch");
                        confirm_receipt(i, &id)?;
                    }
                    let passed = message["passed"] == true;
                    trace.push(
                        json!({"step":step,"assertions":message["assertions"],"passed":passed}),
                    );
                    write_trace(evidence, &trace, commit, &redactions)?;
                    ensure!(passed, "independent browser assertions failed");
                    if capture {
                        ensure!(
                            evidence.join("screenshot.png").is_file(),
                            "browser screenshot evidence missing"
                        );
                    }
                    return Ok(BrowserOutcome::Passed(
                        json!({"accepted":true,"mode":"jev","tested_commit":commit,"steps":step,"assertions":message["assertions"]}),
                    ));
                }
                Some("observation") => {
                    if let Some(id) = pending_receipt.take() {
                        ensure!(message["ack"] == id, "browser action receipt mismatch");
                        confirm_receipt(i, &id)?;
                    }
                    if step == spec.max_steps {
                        return fallback("browser step budget exhausted");
                    }
                    let observed: Observation = serde_json::from_value(message.clone())?;
                    checked_url(&observed.url, &origin)?;
                    if !observed.unsupported.is_empty() {
                        trace.push(json!({"step":step,"unsupported":observed.unsupported}));
                        write_trace(evidence, &trace, commit, &redactions)?;
                        return fallback("unsupported browser resource or control");
                    }
                    let options = match catalog(&observed, &spec.values) {
                        Ok(value) => value,
                        Err(error) => return fallback(error.to_string()),
                    };
                    if options.len() == 1 {
                        return fallback("no supported actions");
                    }
                    let state = json!({"objective":spec.objective,"url":observed.url,"controls":observed.controls.iter().map(|c|json!({"id":c.id,"label":c.label,"operations":c.operations})).collect::<Vec<_>>()});
                    let request = DecisionRequest {
                        model:i.settings.decision.model.clone(),
                        state:secrets::redact_json(&state, &redactions),
                        questions:vec![Question::Choice(ChoiceQuestion { id:"action".into(), question:"Choose the next operation and observed target. DONE only requests independent assertions.".into(), options:options.clone() })],
                    };
                    if request.validate().is_err() {
                        return fallback("browser observation exceeds Jev request limits");
                    }
                    let decision_id = crate::store::id();
                    let has_step: bool = i.db.conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM steps WHERE id=?)",
                        [i.step],
                        |row| row.get(0),
                    )?;
                    let has_attempt: bool = i.db.conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM attempts WHERE id=?)",
                        [i.attempt],
                        |row| row.get(0),
                    )?;
                    let admitted = store::enqueue_bounded(
                        i.db,
                        &store::QueuedDecision {
                            id: decision_id.clone(),
                            task: i.task.into(),
                            step: has_step.then(|| i.step.into()),
                            attempt: has_attempt.then(|| i.attempt.into()),
                            purpose: "browser_action_proposal".into(),
                            policy: "browser-test-v1".into(),
                            backend: i.settings.decision.backend.clone(),
                            model: i.settings.decision.model.clone(),
                            baseline: Some("environment_test_fallback".into()),
                        },
                        i.settings.decision.max_decisions_per_task,
                    )?;
                    if !admitted {
                        return fallback("operator task decision budget exhausted");
                    }
                    let state_hash = crate::store::hash(&serde_json::to_vec(&request.state)?);
                    let request_hash = crate::store::hash(&serde_json::to_vec(&request)?);
                    let context_version = crate::delegation::tree(i.db, i.task)?["version"]
                        .as_i64()
                        .context("context version")?;
                    store::start(
                        i.db,
                        &decision_id,
                        &store::PreparedDecision {
                            state_hash: state_hash.clone(),
                            context_version,
                            policy_hash: crate::store::hash(b"browser-test-v1"),
                            catalog_hash: crate::store::hash(&serde_json::to_vec(&options)?),
                            candidate_hashes: json!(
                                observed
                                    .controls
                                    .iter()
                                    .map(|control| crate::store::hash(control.id.as_bytes()))
                                    .collect::<Vec<_>>()
                            ),
                            backend_fingerprint: crate::store::hash(&serde_json::to_vec(&json!(
                                [
                                    i.settings.decision.backend,
                                    i.settings.decision.base_url,
                                    i.settings.decision.model
                                ]
                            ))?),
                            evidence_hash: state_hash,
                            request_hash,
                            cache_hash: String::new(),
                            artifact_hash: None,
                        },
                    )?;
                    let provider_start = Instant::now();
                    let (response, attempts) = match client.decide_counted(&request).await {
                        Ok(value) => value,
                        Err(error) => {
                            store::finish_state(
                                i.db,
                                &decision_id,
                                "failed",
                                "provider_unavailable",
                                error.attempts,
                            )?;
                            return fallback("Jev decision unavailable");
                        }
                    };
                    let selected_action = selected(&response, &options);
                    let result_value = serde_json::to_value(&response)?;
                    let usage = serde_json::to_value(&response.usage)?;
                    store::complete(
                        i.db,
                        &decision_id,
                        &store::CompletedDecision {
                            result: &result_value,
                            proposed: selected_action.as_ref().ok().map(String::as_str),
                            abstention: selected_action.is_err(),
                            provider_ms: provider_start.elapsed().as_millis() as u64,
                            attempts,
                            usage: &usage,
                        },
                    )?;
                    let action = match selected_action {
                        Ok(value) => value,
                        Err(error) => return fallback(error.to_string()),
                    };
                    let confidence = match &response.answers[0] {
                        Answer::Choice { confidence, .. } => *confidence,
                        _ => 0.0,
                    };
                    trace.push(json!({"step":step,"decision":decision_id,"url":observed.url,"action":action,"version":observed.version,"confidence":confidence,"usage":response.usage,"attempts":attempts}));
                    write_trace(evidence, &trace, commit, &redactions)?;
                    let mode = operator_mode(i).unwrap_or(BrowserTestMode::Disabled);
                    ensure!(
                        verified_commit(i.workspace)? == commit,
                        "browser workspace changed during decision"
                    );
                    let current_context_version = crate::delegation::tree(i.db, i.task)?["version"]
                        .as_i64()
                        .context("context version")?;
                    if current_context_version != context_version {
                        return fallback("task context changed during browser decision");
                    }
                    if mode != BrowserTestMode::Active {
                        return fallback("operator browser decision shadow mode");
                    }
                    if action == "DONE" {
                        prepare_receipt(
                            i,
                            &Receipt {
                                environment: environment_id,
                                decision: &decision_id,
                                commit,
                                context_version,
                                version: observed.version,
                                action: &action,
                                target: None,
                            },
                        )?;
                        pending_receipt = Some(decision_id.clone());
                        send(&mut child, &json!({"cmd":"assert","decision":decision_id})).await?;
                        continue;
                    }
                    let (operation, id) =
                        action.split_once(' ').context("invalid browser action")?;
                    let control = observed
                        .controls
                        .iter()
                        .find(|control| control.id == id)
                        .context("browser target missing")?;
                    let value = spec.values.get(&control.label).cloned();
                    prepare_receipt(
                        i,
                        &Receipt {
                            environment: environment_id,
                            decision: &decision_id,
                            commit,
                            context_version,
                            version: observed.version,
                            action: &action,
                            target: Some(id),
                        },
                    )?;
                    pending_receipt = Some(decision_id.clone());
                    send(&mut child, &json!({"cmd":"act","decision":decision_id,"version":observed.version,"id":id,"operation":operation,"value":value})).await?;
                    if action == previous_action {
                        repeats += 1;
                    } else {
                        previous_action = action;
                        repeats = 1;
                    }
                    if repeats >= 3 {
                        return fallback("repeated browser action stalled");
                    }
                }
                _ => bail!("browser driver sent an unexpected message"),
            }
        }
        fallback("browser step budget exhausted")
    };
    let result = tokio::time::timeout(Duration::from_secs(spec.timeout_seconds), operation)
        .await
        .context("browser test timed out")
        .and_then(|value| value);
    drop(group);
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spec_requires_independent_assertions_and_bounds() {
        let mut spec = BrowserSpec {
            objective: "save a note".into(),
            ..Default::default()
        };
        assert!(spec.validate().is_err());
        spec.assertions.push(Assertion::TextVisible {
            text: "Saved".into(),
        });
        assert!(spec.validate().is_ok());
        spec.max_steps = 33;
        assert!(spec.validate().is_err());
    }
    #[test]
    fn catalog_never_truncates_and_needs_declared_values() {
        let observed = Observation {
            version: 1,
            url: "http://127.0.0.1/".into(),
            controls: vec![
                Control {
                    id: "c0".into(),
                    label: "Name".into(),
                    operations: vec!["fill".into()],
                },
                Control {
                    id: "c1".into(),
                    label: "Save".into(),
                    operations: vec!["click".into()],
                },
            ],
            unsupported: vec![],
        };
        assert_eq!(
            catalog(&observed, &BTreeMap::new()).unwrap(),
            vec!["click c1", "DONE"]
        );
        assert_eq!(
            catalog(&observed, &BTreeMap::from([("Name".into(), "test".into())])).unwrap(),
            vec!["fill c0", "click c1", "DONE"]
        );
        let many = Observation {
            controls: (0..64)
                .map(|n| Control {
                    id: format!("c{n}"),
                    label: "Button".into(),
                    operations: vec!["click".into()],
                })
                .collect(),
            ..observed
        };
        assert!(catalog(&many, &BTreeMap::new()).is_err());
    }
    #[test]
    fn cross_origin_rejected() {
        let app = reqwest::Url::parse("http://127.0.0.1:1234/").unwrap();
        assert!(checked_url("http://127.0.0.1:1234/next", &app).is_ok());
        assert!(checked_url("http://127.0.0.1:2222/", &app).is_err());
        assert!(checked_url("https://evil.example/", &app).is_err());
    }
    #[test]
    fn redacts_literal_and_json_escaped_bundle_values() {
        let values = BTreeMap::from([("APP_SECRET".into(), "private\"token".into())]);
        let map = redactions(&values);
        let state = json!({"label":"private\"token","encoded":"private\\\"token"});
        let safe = secrets::redact_json(&state, &map);
        assert!(!safe.to_string().contains("private"));
        let temp = tempfile::tempdir().unwrap();
        write_trace(
            temp.path(),
            &[state],
            "0123456789012345678901234567890123456789",
            &map,
        )
        .unwrap();
        assert!(
            !std::fs::read_to_string(temp.path().join("trace.json"))
                .unwrap()
                .contains("private")
        );
    }
    #[test]
    fn dirty_or_untracked_workspace_cannot_claim_a_commit() {
        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(
                clean_command("git")
                    .args(args)
                    .current_dir(repo.path())
                    .status()
                    .unwrap()
                    .success()
            );
        };
        run(&["init", "-q"]);
        std::fs::write(repo.path().join("page.html"), "first").unwrap();
        run(&["add", "."]);
        run(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-qm",
            "fixture",
        ]);
        assert!(verified_commit(repo.path()).is_ok());
        std::fs::write(repo.path().join("page.html"), "changed").unwrap();
        assert!(verified_commit(repo.path()).is_err());
        run(&["checkout", "--", "page.html"]);
        std::fs::write(repo.path().join("untracked"), "changed").unwrap();
        assert!(verified_commit(repo.path()).is_err());
    }
    #[test]
    fn ignored_browser_spec_is_not_a_checked_in_assertion() {
        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(
                clean_command("git")
                    .args(args)
                    .current_dir(repo.path())
                    .status()
                    .unwrap()
                    .success()
            );
        };
        run(&["init", "-q"]);
        std::fs::write(repo.path().join(".gitignore"), "browser.json\n").unwrap();
        run(&["add", ".gitignore"]);
        run(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "-qm",
            "fixture",
        ]);
        std::fs::write(
            repo.path().join("browser.json"),
            json!({"version":1,"objective":"inspect page","assertions":[{"kind":"url_path","path":"/"}]}).to_string(),
        ).unwrap();
        assert!(verified_commit(repo.path()).is_ok());
        assert!(spec_from_file(repo.path(), Path::new("browser.json")).is_err());
    }
}
