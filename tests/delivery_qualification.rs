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
    let report_path = directory.path().join("blocked-report.json");
    let save_report = || {
        Command::new(env!("CARGO_BIN_EXE_horde"))
            .arg("--data-dir")
            .arg(&untouched)
            .args(["delivery-qualification", "report", "--frozen"])
            .arg(&frozen)
            .arg("--evidence")
            .arg(&evidence)
            .arg("--output")
            .arg(&report_path)
            .output()
            .unwrap()
    };
    let first = save_report();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let receipt: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(receipt["qualified"], false);
    assert_eq!(receipt["enrolled"], false);
    let bytes = std::fs::read(&report_path).unwrap();
    assert_eq!(receipt["report_sha256"], horde::store::hash(&bytes));
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), report);
    let replay = save_report();
    assert!(replay.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&replay.stdout).unwrap(),
        receipt
    );
    std::fs::write(&report_path, b"{\"replaced\":true}").unwrap();
    let conflict = save_report();
    assert!(!conflict.status.success());
    assert!(
        String::from_utf8_lossy(&conflict.stderr)
            .contains("already exists with different evidence")
    );
    assert_eq!(std::fs::read(&report_path).unwrap(), b"{\"replaced\":true}");
    assert!(!untouched.exists());
}

#[test]
fn advisory_observe_cli_routes_the_explicit_operator_request_without_opening_a_store() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = directory.path().join("runtime");
    std::fs::create_dir(&runtime).unwrap();
    let listener = UnixListener::bind(runtime.join("daemon.sock")).unwrap();
    let frozen = directory.path().join("frozen.json");
    let document = json!({"manifest_hash":"fixture-frozen-identity"});
    std::fs::write(&frozen, document.to_string()).unwrap();
    let server = std::thread::spawn(move || {
        for rejected in [false, true] {
            let (mut socket, _) = listener.accept().unwrap();
            let mut line = String::new();
            std::io::BufReader::new(socket.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let packet: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(packet["project"], Value::Null);
            assert_eq!(packet["token"], Value::Null);
            assert_eq!(packet["method"], "delivery_qualification_observe");
            assert_eq!(
                packet["args"],
                json!({"project":"owning-project","frozen":document,"case_id":"case-1","task":"candidate-1","expected_head":"c".repeat(40),"request_id":"observation-1"})
            );
            let response = if rejected {
                json!({"error":"operator observation denied"})
            } else {
                json!({"result":{"state":"held","advisory":true,"enrolled":false}})
            };
            writeln!(socket, "{response}").unwrap();
        }
    });
    for rejected in [false, true] {
        let output = Command::new(env!("CARGO_BIN_EXE_horde"))
            .arg("--data-dir")
            .arg(&runtime)
            .args([
                "--project",
                "owning-project",
                "delivery-qualification",
                "observe",
                "--frozen",
            ])
            .arg(&frozen)
            .args([
                "--case-id",
                "case-1",
                "--task",
                "candidate-1",
                "--expected-head",
                &"c".repeat(40),
                "--request-id",
                "observation-1",
            ])
            .env_remove("HORDE_WORKER_TOKEN")
            .output()
            .unwrap();
        if rejected {
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("operator observation denied")
            );
        } else {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stdout).unwrap()["state"],
                "held"
            );
        }
        assert_eq!(
            std::fs::read_dir(&runtime).unwrap().count(),
            1,
            "CLI must only connect to the existing socket"
        );
    }
    server.join().unwrap();
}
