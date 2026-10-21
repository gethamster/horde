//! Offline qualification reports from retained, independently reviewed real Runs.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::{Read, Write},
    path::Path,
};
mod evidence;
pub use evidence::{assess, collect};

#[derive(clap::Subcommand)]
pub enum Command {
    /// Freeze the operator-reviewed held-out cases before candidate execution.
    Prepare {
        #[arg(long)]
        manifest: std::path::PathBuf,
        #[arg(long)]
        output: std::path::PathBuf,
    },
    /// Read existing Run evidence through the selected daemon; never dispatch work.
    Collect {
        #[arg(long)]
        frozen: std::path::PathBuf,
        #[arg(long)]
        output: std::path::PathBuf,
    },
    /// Validate real paired evidence and compute qualification metrics.
    Validate {
        #[arg(long)]
        frozen: std::path::PathBuf,
        #[arg(long)]
        evidence: std::path::PathBuf,
    },
    /// Save the computed report for separate independent operator review/enrollment.
    Report {
        #[arg(long)]
        frozen: std::path::PathBuf,
        #[arg(long)]
        evidence: std::path::PathBuf,
        #[arg(long)]
        output: std::path::PathBuf,
    },
}
pub fn run(command: &Command, root: Option<&Path>, project: Option<&str>) -> Result<Value> {
    match command {
        Command::Prepare { manifest, output } => prepare(manifest, output),
        Command::Collect { frozen, output } => collect(
            frozen,
            root.context("collection requires explicit --data-dir of the existing daemon")?,
            project.context("collection requires explicit --project")?,
            output,
        ),
        Command::Validate { frozen, evidence } => assess(frozen, evidence),
        Command::Report {
            frozen,
            evidence,
            output,
        } => {
            let report = assess(frozen, evidence)?;
            ensure!(
                serde_json::to_vec(&report)?.len() <= 64 * 1024,
                "qualification report exceeds the existing 64 KiB enrollment bound"
            );
            if output.exists() {
                ensure!(
                    read(output)? == report,
                    "report already exists with different evidence"
                );
            } else {
                persist(output, &report)?;
            }
            Ok(
                json!({"qualified":report["qualified"],"report_sha256":digest(&report)?,"manifest_hash":report["manifest_hash"],"enrolled":false}),
            )
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    qualification_id: String,
    spend_reference: String,
    oracle_id: String,
    oracle_spec_sha256: String,
    decision: crate::config::Decision,
    delivery_policy: crate::config::AutomaticDelivery,
    cases: Vec<Case>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    repository: String,
    base_commit: String,
    objective: String,
    inputs_sha256: String,
    verification_argv_sha256: String,
    baseline_acceptance_question_sha256: String,
    critical: bool,
    baseline_task: String,
    candidate_task: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frozen {
    manifest_hash: String,
    frozen_at: i64,
    manifest: Manifest,
}
fn digest(value: &impl Serialize) -> Result<String> {
    Ok(crate::store::hash(&serde_json::to_vec(value)?))
}
fn frozen_id(manifest: &Manifest, frozen_at: i64) -> Result<String> {
    digest(&json!({"manifest":manifest,"frozen_at":frozen_at}))
}
fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn label(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 256
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':')),
        "invalid qualification identifier"
    );
    Ok(())
}
fn parse_manifest(value: &Value) -> Result<Manifest> {
    let manifest: Manifest = serde_json::from_value(value.clone())?;
    ensure!(
        manifest.schema_version == 1,
        "unsupported qualification manifest"
    );
    for value in [
        &manifest.qualification_id,
        &manifest.spend_reference,
        &manifest.oracle_id,
    ] {
        label(value)?;
    }
    ensure!(
        hex(&manifest.oracle_spec_sha256, 64),
        "missing independent oracle specification hash"
    );
    manifest.decision.validate()?;
    manifest.delivery_policy.validate()?;
    ensure!(
        manifest.decision.mode == crate::config::DecisionMode::Shadow
            && manifest.delivery_policy.enabled
            && manifest.delivery_policy.qualification_id == manifest.qualification_id,
        "qualification must pin the exact enabled candidate decision and delivery policy"
    );
    manifest.decision.fingerprint()?;
    ensure!(
        manifest.cases.len() >= 50,
        "at least 50 held-out case pairs are required"
    );
    let mut identities = BTreeSet::new();
    let mut inputs = BTreeSet::new();
    let mut tasks = BTreeSet::new();
    for case in &manifest.cases {
        for value in [&case.id, &case.baseline_task, &case.candidate_task] {
            label(value)?;
        }
        ensure!(
            identities.insert(&case.id)
                && inputs.insert(&case.inputs_sha256)
                && tasks.insert(&case.baseline_task)
                && tasks.insert(&case.candidate_task),
            "duplicate held-out case, inputs or Run identity"
        );
        ensure!(
            hex(&case.inputs_sha256, 64)
                && hex(&case.verification_argv_sha256, 64)
                && hex(&case.baseline_acceptance_question_sha256, 64)
                && hex(&case.base_commit, 40)
                && !case.objective.trim().is_empty()
                && case.objective.len() <= 30_000
                && manifest
                    .delivery_policy
                    .repositories
                    .contains(&case.repository),
            "invalid frozen case inputs or repository scope"
        );
    }
    ensure!(
        manifest.cases.iter().any(|case| case.critical),
        "critical-defect cases are required to measure recall"
    );
    Ok(manifest)
}
fn read(path: &Path) -> Result<Value> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "qualification evidence exceeds 16 MiB"
    );
    Ok(serde_json::from_slice(&bytes)?)
}
fn persist(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path)?;
    Ok(())
}
fn frozen(path: &Path) -> Result<Frozen> {
    let value = read(path)?;
    let result: Frozen = serde_json::from_value(value.clone())?;
    parse_manifest(&value["manifest"])?;
    ensure!(
        result.frozen_at > 0
            && result.manifest_hash == frozen_id(&result.manifest, result.frozen_at)?,
        "frozen qualification manifest integrity failure"
    );
    Ok(result)
}
pub fn prepare(source: &Path, destination: &Path) -> Result<Value> {
    let manifest = parse_manifest(&read(source)?)?;
    if destination.exists() {
        let previous = frozen(destination)?;
        ensure!(
            digest(&previous.manifest)? == digest(&manifest)?,
            "qualification corpus is already frozen with different inputs"
        );
        return Ok(
            json!({"manifest_hash":previous.manifest_hash,"frozen_at":previous.frozen_at,"cases":manifest.cases.len(),"enrolled":false}),
        );
    }
    let frozen_at = crate::store::now();
    let manifest_hash = frozen_id(&manifest, frozen_at)?;
    persist(
        destination,
        &Frozen {
            manifest_hash: manifest_hash.clone(),
            frozen_at,
            manifest,
        },
    )?;
    Ok(
        json!({"manifest_hash":manifest_hash,"frozen_at":frozen_at,"cases":read(destination)?["manifest"]["cases"].as_array().context("frozen cases")?.len(),"enrolled":false}),
    )
}

#[cfg(test)]
mod manifest_tests {
    use super::*;
    use serde_json::{Value, json};

    pub(crate) fn input() -> Value {
        json!({"schema_version":1,"qualification_id":"held-out-real-1",
            "spend_reference":"operator-approved-evaluation-budget",
            "oracle_id":"independent-operator-1","oracle_spec_sha256":"a".repeat(64),
            "decision":{"mode":"shadow","backend":"tuara","base_url":"https://tuara.com/router","api_key_env":"DECISION_KEY"},
            "delivery_policy":{"enabled":true,"gh_program":"/usr/bin/gh","repositories":["gethamster/adam"],"bases":["main"],"allowed_paths":["src"],"required_checks":["test"],"minimum_approvals":1,"require_independent_review":true,"qualification_id":"held-out-real-1","minimum_routine_probability":0.8,"minimum_review_confidence":0.8,"package_release":{"repository":"gethamster/adam","workflow":"release.yml"}},
            "cases":(0..50).map(|index| json!({"id":format!("case-{index}"),"repository":"gethamster/adam","base_commit":"b".repeat(40),"objective":format!("Independent held-out fixture {index}"),"inputs_sha256":format!("{index:064x}"),"verification_argv_sha256":digest(&json!(["independent-fixture-verifier"])).unwrap(),"baseline_acceptance_question_sha256":crate::store::hash(b"Accept this exact evaluated change or escalate."),"critical":index==0,"baseline_task":format!("baseline-{index}"),"candidate_task":format!("candidate-{index}")})).collect::<Vec<_>>()})
    }

    #[test]
    fn frozen_manifest_requires_fifty_unique_real_pairs_and_a_critical_oracle() {
        let manifest = parse_manifest(&input()).unwrap();
        assert_eq!(manifest.cases.len(), 50);
        let mut invalid = input();
        invalid["cases"].as_array_mut().unwrap().pop();
        assert!(parse_manifest(&invalid).is_err());
        let mut duplicate = input();
        duplicate["cases"][1] = duplicate["cases"][0].clone();
        assert!(parse_manifest(&duplicate).is_err());
        let mut missing_oracle = input();
        missing_oracle["oracle_spec_sha256"] = json!("");
        assert!(parse_manifest(&missing_oracle).is_err());
        let mut no_critical = input();
        for case in no_critical["cases"].as_array_mut().unwrap() {
            case["critical"] = json!(false);
        }
        assert!(parse_manifest(&no_critical).is_err());
    }

    #[test]
    fn manifest_never_accepts_caller_provided_aggregate_metrics() {
        let mut forged = input();
        forged["held_out"] =
            json!({"cases":50,"candidate_quality":1,"candidate_verified_completion_ms":1});
        assert!(parse_manifest(&forged).is_err());
    }

    #[test]
    fn preparing_is_repeatable_and_refuses_a_changed_frozen_corpus() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.json");
        let destination = directory.path().join("frozen.json");
        std::fs::write(&source, serde_json::to_vec(&input()).unwrap()).unwrap();
        let first = prepare(&source, &destination).unwrap();
        assert_eq!(prepare(&source, &destination).unwrap(), first);
        let mut changed = input();
        changed["cases"][0]["objective"] = json!("Changed after freeze");
        std::fs::write(&source, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(prepare(&source, &destination).is_err());
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(&destination).unwrap()).unwrap()["manifest_hash"],
            first["manifest_hash"]
        );
    }

    #[test]
    fn freeze_identity_also_binds_the_original_collection_time() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.json");
        let output = directory.path().join("frozen.json");
        std::fs::write(&source, serde_json::to_vec(&input()).unwrap()).unwrap();
        prepare(&source, &output).unwrap();
        let mut document = read(&output).unwrap();
        document["frozen_at"] = json!(1);
        std::fs::write(&output, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(
            frozen(&output).is_err(),
            "candidate evidence cannot be grandfathered by moving the freeze timestamp"
        );
    }
}
