//! The Claude harness asks Claude Code for a schema-validated final answer and
//! makes at most one repair turn when the answer is not the JSON object.
use serde_json::{Value, json};
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, path::Path};

/// A fake `claude` that logs each launch's argv and stdin, then prints the
/// next scripted result event. A reply with `"exit"` exits with that code.
fn mock(dir: &Path, replies: &[Value]) -> std::path::PathBuf {
    let program = dir.join("mock-claude");
    let replies_file = dir.join("replies.json");
    std::fs::write(&replies_file, serde_json::to_vec(replies).unwrap()).unwrap();
    let log = dir.join("calls.jsonl");
    std::fs::write(
        &program,
        format!(
            r#"#!/usr/bin/env python3
import json, sys
log = {log:?}
calls = sum(1 for _ in open(log)) if __import__('os').path.exists(log) else 0
with open(log, 'a') as f:
    f.write(json.dumps({{"argv": sys.argv[1:], "stdin": sys.stdin.read()}}) + "\n")
reply = json.load(open({replies:?}))[calls]
code = reply.pop("exit", 0)
print(json.dumps(reply))
sys.exit(code)
"#,
            log = log.to_str().unwrap(),
            replies = replies_file.to_str().unwrap()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    program
}

fn calls(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join("calls.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn event(extra: Value) -> Value {
    let mut event = json!({
        "type":"result","subtype":"success","is_error":false,
        "session_id":"4f0c1d2e-0000-4000-8000-000000000001",
        "usage":{"input_tokens":10,"output_tokens":5},"total_cost_usd":0.25,
        "result":"**Result**: Repository inspected; plan follows."
    });
    for (key, value) in extra.as_object().unwrap() {
        event[key] = value.clone();
    }
    event
}

async fn run(
    replies: &[Value],
    output_types: BTreeMap<String, String>,
) -> (
    anyhow::Result<Value>,
    Vec<Value>,
    horde::store::Store,
    tempfile::TempDir,
) {
    let temp = tempfile::tempdir().unwrap();
    let program = mock(temp.path(), replies);
    let db = horde::store::Store::open(&temp.path().join("data")).unwrap();
    let settings = horde::config::Settings {
        allow_commands: true,
        providers: BTreeMap::from([(
            "claude".into(),
            horde::config::Provider {
                kind: "claude".into(),
                auth_mode: "login".into(),
                base_url: "".into(),
                program: Some(program.to_string_lossy().into_owned()),
                ..Default::default()
            },
        )]),
        executors: BTreeMap::from([(
            "worker".into(),
            horde::config::Executor {
                provider: Some("claude".into()),
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    let plan = horde::template::compile(
        "simulated",
        &horde::template::load_templates(temp.path()).unwrap(),
        BTreeMap::from([("task".into(), "test".into())]),
    )
    .unwrap();
    let task = db.submit("test", temp.path(), &settings, &plan).unwrap();
    let row = db.steps(&task).unwrap()[0].clone();
    let mut step = horde::store::Store::step(&row).unwrap();
    step.kind = "agent".into();
    step.role = "worker".into();
    step.output_types = output_types;
    let worker = db.register(&task, row["id"].as_str()).unwrap();
    db.conn
        .execute(
            "INSERT INTO attempts(id,step,worker,state,started) VALUES('test',?,?,'running',?)",
            rusqlite::params![
                row["id"].as_str().unwrap(),
                worker["id"].as_str().unwrap(),
                horde::store::now()
            ],
        )
        .unwrap();
    let invocation = horde::executor::Invocation {
        db: &db,
        task: &task,
        step: row["id"].as_str().unwrap(),
        attempt: "test",
        worker: worker["id"].as_str().unwrap(),
        token: worker["token"].as_str().unwrap(),
        workspace: temp.path(),
        spec: &step,
        settings: &settings,
        context: json!({}),
    };
    let result = horde::executor::execute(&invocation).await;
    let calls = calls(temp.path());
    (result, calls, db, temp)
}

fn flag<'a>(call: &'a Value, name: &str) -> Option<&'a str> {
    let argv = call["argv"].as_array().unwrap();
    argv.iter()
        .position(|arg| arg == name)
        .and_then(|index| argv.get(index + 1))
        .and_then(Value::as_str)
}

#[tokio::test]
async fn structured_output_is_used_even_when_the_result_text_is_prose() {
    let (result, calls, _db, _temp) = run(
        &[event(json!({"structured_output":{"result":"plan ready","accepted":true,"artifacts":[],"count":3}}))],
        BTreeMap::from([("count".into(), "integer".into())]),
    )
    .await;
    let result = result.unwrap();
    assert_eq!(result["result"], "plan ready");
    assert_eq!(result["count"], 3);
    assert_eq!(calls.len(), 1);
    let schema: Value = serde_json::from_str(flag(&calls[0], "--json-schema").unwrap()).unwrap();
    assert_eq!(schema["required"], json!(["result", "accepted"]));
    assert_eq!(schema["properties"]["accepted"]["type"], "boolean");
    assert_eq!(schema["properties"]["count"]["type"], "integer");
    assert!(flag(&calls[0], "--resume").is_none());
    assert_eq!(flag(&calls[0], "--output-format"), Some("json"));
}

#[tokio::test]
async fn prose_answer_gets_one_repair_turn_in_the_same_session() {
    let (result, calls, db, _temp) = run(
        &[
            event(json!({})),
            event(json!({"result":"{\"result\":\"plan ready\",\"accepted\":true}","usage":{"input_tokens":2,"output_tokens":1},"total_cost_usd":0.05})),
        ],
        BTreeMap::new(),
    )
    .await;
    let result = result.unwrap();
    assert_eq!(result["result"], "plan ready");
    assert_eq!(calls.len(), 2);
    assert_eq!(
        flag(&calls[1], "--resume"),
        Some("4f0c1d2e-0000-4000-8000-000000000001")
    );
    assert!(flag(&calls[1], "--json-schema").is_some());
    let stdin = calls[1]["stdin"].as_str().unwrap();
    assert!(stdin.contains("Reply with only the JSON object"));
    assert_eq!(result["usage"]["provider"]["input_tokens"], 12);
    assert_eq!(result["usage"]["provider"]["output_tokens"], 6);
    assert!((result["usage"]["api_cost_usd"].as_f64().unwrap() - 0.30).abs() < 1e-9);
    assert_eq!(result["usage"]["repair_turns"], 1);
    let stored: Value = serde_json::from_str(
        &db.conn
            .query_row("SELECT usage FROM attempts WHERE id='test'", [], |r| {
                r.get::<_, String>(0)
            })
            .unwrap(),
    )
    .unwrap();
    assert_eq!(stored["provider"]["input_tokens"], 12);
    assert!(
        db.rows(
            "SELECT kind FROM events WHERE kind='executor.result_repair'",
            &[]
        )
        .unwrap()
        .len()
            == 1
    );
}

#[tokio::test]
async fn a_second_prose_answer_fails_without_further_turns() {
    let (result, calls, _db, _temp) = run(
        &[
            event(json!({})),
            event(json!({"result":"Still prose."})),
            event(json!({"result":"{\"result\":\"late\",\"accepted\":true}"})),
        ],
        BTreeMap::new(),
    )
    .await;
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("one repair turn"), "{error}");
    assert!(
        error.contains("must be JSON with result and accepted"),
        "{error}"
    );
    assert_eq!(calls.len(), 2);
}

#[tokio::test]
async fn exhausted_structured_output_retries_are_repaired_once() {
    let (result, calls, _db, _temp) = run(
        &[
            event(json!({"subtype":"error_max_structured_output_retries","is_error":true,"errors":["schema validation failed"],"exit":1})),
            event(json!({"structured_output":{"result":"plan ready","accepted":true}})),
        ],
        BTreeMap::new(),
    )
    .await;
    assert_eq!(result.unwrap()["result"], "plan ready");
    assert_eq!(calls.len(), 2);
}

#[tokio::test]
async fn a_declined_step_is_not_repaired() {
    let (result, calls, _db, _temp) = run(
        &[event(
            json!({"structured_output":{"result":"tests fail","accepted":false}}),
        )],
        BTreeMap::new(),
    )
    .await;
    let error = result.unwrap_err().to_string();
    assert!(error.contains("did not accept"), "{error}");
    assert_eq!(calls.len(), 1);
}

#[tokio::test]
async fn an_error_result_is_not_repaired() {
    let (result, calls, _db, _temp) = run(
        &[event(
            json!({"is_error":true,"result":"Rate limit exceeded","exit":1}),
        )],
        BTreeMap::new(),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(calls.len(), 1);
}
