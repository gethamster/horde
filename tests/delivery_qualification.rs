//! Offline CLI boundaries; no provider or live runtime is contacted.
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    os::unix::net::UnixListener,
    process::Command,
};

#[test]
fn retained_evidence_collection_uses_read_only_project_scoped_rpc_without_opening_a_store() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = directory.path().join("runtime");
    std::fs::create_dir(&runtime).unwrap();
    let socket = UnixListener::bind(runtime.join("daemon.sock")).unwrap();
    let cases:Vec<_>=(0..50).map(|index|json!({"id":format!("case-{index}"),"repository":"gethamster/adam","base_commit":"b".repeat(40),
        "objective":format!("offline fixture {index}"),"inputs_sha256":format!("{index:064x}"),"verification_argv_sha256":"a".repeat(64),"baseline_acceptance_question_sha256":"a".repeat(64),"critical":index==0,
        "baseline_task":format!("baseline-{index}"),"candidate_task":format!("candidate-{index}")})).collect();
    let source = directory.path().join("manifest.json");
    let frozen = directory.path().join("frozen.json");
    let evidence = directory.path().join("evidence");
    std::fs::write(&source,json!({"schema_version":1,"qualification_id":"held-out-1","spend_reference":"budget-1","oracle_id":"independent-oracle","oracle_spec_sha256":"a".repeat(64),
        "decision":{"mode":"shadow","backend":"tuara","base_url":"https://tuara.com/router","api_key_env":"FIXTURE_KEY"},
        "delivery_policy":{"enabled":true,"gh_program":"/usr/bin/gh","qualification_id":"held-out-1","repositories":["gethamster/adam"],"bases":["main"],"allowed_paths":["src"],"required_checks":["test"],"minimum_approvals":1,"require_independent_review":true,"minimum_routine_probability":0.8,"minimum_review_confidence":0.8,"package_release":{"repository":"gethamster/adam","workflow":"release.yml"}},"cases":cases}).to_string()).unwrap();
    let untouched = directory.path().join("must-not-create-store");
    let prepare = Command::new(env!("CARGO_BIN_EXE_horde"))
        .arg("--data-dir")
        .arg(&untouched)
        .args(["delivery-qualification", "prepare", "--manifest"])
        .arg(&source)
        .arg("--output")
        .arg(&frozen)
        .output()
        .unwrap();
    assert!(
        prepare.status.success(),
        "{}",
        String::from_utf8_lossy(&prepare.stderr)
    );
    assert!(!untouched.exists());
    let server = std::thread::spawn(move || {
        for _ in 0..700 {
            let (mut stream, _) = socket.accept().unwrap();
            let mut line = String::new();
            std::io::BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["project"], "fixture-project");
            let task = request["args"]["task"].as_str().unwrap();
            let result = match request["method"].as_str().unwrap() {
                "inspect" => json!({"task":{"id":task},"workers":[],"attempts":[],"artifacts":[]}),
                "summary" => {
                    json!({"task":task,"project":"fixture-project","status":"succeeded","integrated_head":"c".repeat(40),"run":{"run_id":task,"checkpoint":null}})
                }
                "events" | "decisions" | "reviews" => json!([]),
                "run_changes" => json!({}),
                other => panic!("collector attempted unsupported effectful method {other}"),
            };
            writeln!(stream, "{}", json!({"result":result})).unwrap();
        }
    });
    let collect = Command::new(env!("CARGO_BIN_EXE_horde"))
        .arg("--data-dir")
        .arg(&runtime)
        .args([
            "--project",
            "fixture-project",
            "delivery-qualification",
            "collect",
            "--frozen",
        ])
        .arg(&frozen)
        .arg("--output")
        .arg(&evidence)
        .output()
        .unwrap();
    assert!(
        collect.status.success(),
        "{}",
        String::from_utf8_lossy(&collect.stderr)
    );
    server.join().unwrap();
    assert_eq!(
        std::fs::read_dir(&runtime).unwrap().count(),
        1,
        "collector must not open/migrate a Store or install runtime files"
    );
    let validation = Command::new(env!("CARGO_BIN_EXE_horde"))
        .args(["delivery-qualification", "validate", "--frozen"])
        .arg(&frozen)
        .arg("--evidence")
        .arg(&evidence)
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&validation.stdout).unwrap();
    assert!(
        !validation.status.success(),
        "validation must fail its process status when qualification is blocked"
    );
    assert_eq!(report["qualified"], false);
    assert!(
        report.get("held_out").is_none(),
        "missing real Run/oracle evidence must not become qualification metrics"
    );
}
