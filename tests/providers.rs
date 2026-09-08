use horde::{
    config::{ExecutorConfig, Settings},
    executor::{Invocation, execute, probe_tools},
    store::Store,
    template::{self, Step},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    path::Path,
    sync::{Arc, Mutex},
    thread,
};
fn server(replies: Vec<(u16, String)>) -> (String, Arc<Mutex<Vec<Value>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(vec![]));
    let recorded = requests.clone();
    let handle = thread::spawn(move || {
        for (status, body) in replies {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = vec![];
            let mut b = [0; 1];
            while !bytes.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut b).unwrap();
                bytes.push(b[0]);
                assert!(bytes.len() < 65536);
            }
            let headers = String::from_utf8(bytes).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            let mut data = vec![0; length];
            stream.read_exact(&mut data).unwrap();
            recorded.lock().unwrap().push(if data.is_empty() {
                json!({"method":"GET"})
            } else {
                serde_json::from_slice(&data).unwrap()
            });
            write!(stream,"HTTP/1.1 {status} OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",if body.starts_with("data:"){"text/event-stream"}else{"application/json"},body.len(),body).unwrap();
        }
    });
    (format!("http://{address}/v1"), requests, handle)
}
fn config(url: String) -> ExecutorConfig {
    ExecutorConfig {
        kind: "tuara".into(),
        model: Some("z-ai/glm-5.3-flash".into()),
        base_url: url,
        api_key_env: "PATH".into(),
        ..Default::default()
    }
}
fn provider(url: String) -> horde::config::Provider {
    horde::config::Provider {
        kind: "tuara".into(),
        auth_mode: "api".into(),
        model: Some("z-ai/glm-5.3-flash".into()),
        base_url: url,
        api_key_env: "PATH".into(),
        ..Default::default()
    }
}
#[tokio::test]
async fn exact_model_and_streaming_tool_fragments_are_validated() {
    let sse = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"name\":\"ping\",\"arguments\":\"{\\\"value\\\":\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"arguments\":\"\\\"ok\\\"}\"}}]}}]}\n\n",
        "data: [DONE]\n\n"
    );
    let (url, requests, handle) = server(vec![
        (
            200,
            json!({"data":[{"id":"z-ai/glm-5.3-flash"}]}).to_string(),
        ),
        (200, sse.into()),
    ]);
    let result = probe_tools(&config(url)).await.unwrap();
    assert_eq!(result["streaming_tool_calls_verified"], true);
    handle.join().unwrap();
    assert_eq!(requests.lock().unwrap()[1]["stream"], true);
}
#[tokio::test]
async fn unavailable_model_fails_without_substitution() {
    let (url, requests, handle) = server(vec![(
        200,
        json!({"data":[{"id":"another-model"}]}).to_string(),
    )]);
    let error = horde::executor::probe(&config(url))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("no model substituted"));
    handle.join().unwrap();
    assert_eq!(requests.lock().unwrap().len(), 1);
}
#[tokio::test(flavor = "current_thread")]
async fn native_worker_executes_claimed_file_tool_and_retains_usage() {
    let(url,requests,handle)=server(vec![
        (200,json!({"data":[{"id":"z-ai/glm-5.3-flash"}]}).to_string()),
        (200,json!({"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call1","type":"function","function":{"name":"write_file","arguments":json!({"path":"hello.txt","content":"hello"}).to_string()}}]}}],"usage":{"prompt_tokens":20,"completion_tokens":10}}).to_string()),
        (200,json!({"choices":[{"message":{"role":"assistant","content":json!({"result":"implemented","accepted":true}).to_string()}}],"usage":{"prompt_tokens":30,"completion_tokens":5}}).to_string()),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(&dir.path().join("data")).unwrap();
    let settings = Settings {
        providers: BTreeMap::from([("default".into(), provider(url))]),
        executors: BTreeMap::from([("worker".into(), Default::default())]),
        ..Default::default()
    };
    let plan = template::compile(
        "simulated",
        &template::load_templates(Path::new("absent")).unwrap(),
        BTreeMap::from([("task".into(), "write hello".into())]),
    )
    .unwrap();
    let oid = db.submit("hello", dir.path(), &settings, &plan).unwrap();
    let tid = db.steps(&oid).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let w = db.register(&oid, Some(&tid)).unwrap();
    let wid = w["id"].as_str().unwrap();
    let token = w["token"].as_str().unwrap();
    db.conn
        .execute(
            "UPDATE workers SET workspace=? WHERE id=?",
            rusqlite::params![dir.path().to_str(), wid],
        )
        .unwrap();
    db.claim(&oid, wid, &["hello.txt".into()]).unwrap();
    let step:Step=serde_json::from_value(json!({"id":"native","tools":["write_file"],"scope":["hello.txt"],"instructions":"write hello"})).unwrap();
    let i = Invocation {
        db: &db,
        task: &oid,
        step: &tid,
        attempt: "test",
        worker: wid,
        token,
        workspace: dir.path(),
        spec: &step,
        settings: &settings,
        context: json!({}),
    };
    let result = execute(&i).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("hello.txt")).unwrap(),
        "hello"
    );
    assert_eq!(result["usage"]["requests"].as_array().unwrap().len(), 2);
    handle.join().unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests[2]["messages"][2]["content"], "");
    assert_eq!(requests[2]["messages"][3]["role"], "tool");
}
/// A reasoning model can end a turn with its thinking in a separate field and
/// `content` blank or whitespace. That is no result yet, not a malformed one.
async fn native_result_after(
    turns: Vec<(u16, String)>,
) -> (Result<Value, String>, Vec<Value>, Vec<Value>) {
    native_result_after_as(turns, false).await
}
async fn native_result_after_as(
    turns: Vec<(u16, String)>,
    planner: bool,
) -> (Result<Value, String>, Vec<Value>, Vec<Value>) {
    let mut responses = vec![(
        200,
        json!({"data":[{"id":"z-ai/glm-5.3-flash"}]}).to_string(),
    )];
    responses.extend(turns);
    let (url, requests, handle) = server(responses);
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(&dir.path().join("data")).unwrap();
    let settings = Settings {
        providers: BTreeMap::from([("default".into(), provider(url))]),
        executors: BTreeMap::from([
            ("worker".into(), Default::default()),
            ("planner".into(), Default::default()),
        ]),
        ..Default::default()
    };
    let plan = template::compile(
        "simulated",
        &template::load_templates(Path::new("absent")).unwrap(),
        BTreeMap::from([("task".into(), "think".into())]),
    )
    .unwrap();
    let oid = db.submit("think", dir.path(), &settings, &plan).unwrap();
    let tid = db.steps(&oid).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    if planner {
        let row = db.steps(&oid).unwrap()[0].clone();
        let mut step = Store::step(&row).unwrap();
        step.role = "planner".into();
        db.conn
            .execute(
                "UPDATE steps SET state='running',spec=? WHERE id=?",
                rusqlite::params![serde_json::to_string(&step).unwrap(), tid],
            )
            .unwrap();
    }
    let w = db.register(&oid, Some(&tid)).unwrap();
    let wid = w["id"].as_str().unwrap();
    let step: Step =
        serde_json::from_value(json!({"id":"native","role":if planner {"planner"} else {"worker"},"tools":[],"instructions":"think"})).unwrap();
    let i = Invocation {
        db: &db,
        task: &oid,
        step: &tid,
        attempt: "test",
        worker: wid,
        token: w["token"].as_str().unwrap(),
        workspace: dir.path(),
        spec: &step,
        settings: &settings,
        context: json!({}),
    };
    let result = execute(&i).await.map_err(|e| e.to_string());
    handle.join().unwrap();
    let sent = requests.lock().unwrap().clone();
    let events = db
        .rows(
            "SELECT data FROM events WHERE task=? AND kind='tool.completed' ORDER BY seq",
            &[&oid],
        )
        .unwrap()
        .into_iter()
        .map(|row| serde_json::from_str(row["data"].as_str().unwrap()).unwrap())
        .collect();
    (result, sent, events)
}
fn turn(content: Value) -> (u16, String) {
    (
        200,
        json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":content}}],"usage":{}})
            .to_string(),
    )
}
#[tokio::test(flavor = "current_thread")]
async fn a_final_turn_without_the_object_is_asked_again_rather_than_failing() {
    let good = json!({"result":"planned","accepted":true}).to_string();
    // Prose is what a model actually answers when it ignores the format request.
    for first in [
        json!("Planning is complete; no files were edited."),
        json!("\n\n"),
    ] {
        let (result, sent, _) = native_result_after(vec![turn(first), turn(json!(good))]).await;
        assert_eq!(result.unwrap()["result"], "planned");
        // The nudge is a plain user turn naming the object that is wanted.
        let asked = sent[2]["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(asked.contains("\"accepted\""), "{asked}");
    }
}
#[tokio::test(flavor = "current_thread")]
async fn a_worker_that_never_produces_the_object_says_what_it_did_send() {
    // Exactly two turns: the fixture serves one reply per connection, and the
    // worker must give up after the second rather than asking again.
    let (result, _, _) =
        native_result_after(vec![turn(json!("still prose")), turn(Value::Null)]).await;
    let error = result.unwrap_err();
    assert!(
        error.contains("never produced the JSON result object"),
        "{error}"
    );
    assert!(error.contains("finish_reason stop"), "{error}");
}
#[tokio::test(flavor = "current_thread")]
async fn a_well_formed_refusal_is_an_answer_and_is_not_asked_again() {
    let refused = json!({"result":"tests fail","accepted":false}).to_string();
    let (result, sent, _) = native_result_after(vec![turn(json!(refused))]).await;
    let error = result.unwrap_err();
    assert!(error.contains("did not accept step"), "{error}");
    // Only the catalogue check and the one turn: no nudge was sent.
    assert_eq!(sent.len(), 2, "a declined step was asked again");
}
#[tokio::test]
async fn provider_failure_is_explicit_and_does_not_change_model() {
    let (url, _, handle) = server(vec![(
        402,
        json!({"error":"insufficient_balance"}).to_string(),
    )]);
    let error = horde::executor::probe(&config(url))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("402"));
    handle.join().unwrap();
}
#[tokio::test]
async fn command_timeout_stops_descendants_and_environment_is_allowlisted() {
    let dir = tempfile::tempdir().unwrap();
    let r = horde::executor::run_command(
        &[
            "sh".into(),
            "-c".into(),
            "sleep 30 & echo $! > child.pid; wait".into(),
        ],
        dir.path(),
        1,
        None,
    )
    .await;
    assert!(r.is_err());
    let pid: i32 = std::fs::read_to_string(dir.path().join("child.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!horde::executor::process_alive(pid));
    let c = horde::executor::clean_command("env");
    for (k, _) in c.get_envs() {
        assert!(!k.to_string_lossy().contains("API_KEY"));
    }
}

#[tokio::test]
async fn credential_broker_restricts_token_endpoint_and_model_and_streams_response() {
    let (url, requests, handle) = server(vec![(
        200,
        "data: {\"ok\":true}\n\ndata: [DONE]\n\n".into(),
    )]);
    let config = ExecutorConfig {
        kind: "codex".into(),
        base_url: url,
        model: Some("configured-model".into()),
        auth_mode: "api".into(),
        ..Default::default()
    };
    let broker =
        horde::credentials::Broker::with_key(&config, "scoped-token", "provider-secret".into(), 10)
            .await
            .unwrap();
    let client = reqwest::Client::new();
    let request = json!({"model":"configured-model","input":"hello","stream":true});
    assert_eq!(
        client
            .post(format!("{}/responses", broker.base_url))
            .bearer_auth("wrong-token")
            .json(&request)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .post(format!("{}/files", broker.base_url))
            .bearer_auth("scoped-token")
            .json(&request)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .post(format!("{}/responses", broker.base_url))
            .bearer_auth("scoped-token")
            .json(&json!({"model":"other-model"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let response = client
        .post(format!("{}/responses", broker.base_url))
        .bearer_auth("scoped-token")
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.text().await.unwrap().contains("[DONE]"));
    handle.join().unwrap();
    assert_eq!(requests.lock().unwrap().len(), 1);
    let address = broker.base_url.clone();
    drop(broker);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        client
            .post(format!("{address}/responses"))
            .bearer_auth("scoped-token")
            .json(&request)
            .send()
            .await
            .is_err()
    );
}
#[tokio::test]
async fn credential_broker_rejects_remote_plaintext() {
    let config = ExecutorConfig {
        kind: "claude".into(),
        base_url: "http://example.com/v1".into(),
        ..Default::default()
    };
    assert!(
        horde::credentials::Broker::with_key(&config, "scoped", "secret".into(), 10)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn health_checks_fail_explicitly_and_can_recover() {
    let (url, _, handle) = server(vec![(503, "{}".into()), (503, "{}".into())]);
    assert!(
        horde::delivery::check_health(&url, 2, std::time::Duration::ZERO)
            .await
            .is_err()
    );
    handle.join().unwrap();
    let (url, _, handle) = server(vec![(503, "{}".into()), (200, "{}".into())]);
    horde::delivery::check_health(&url, 2, std::time::Duration::ZERO)
        .await
        .unwrap();
    handle.join().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn native_tool_errors_are_visible_in_events_and_the_worker_can_continue() {
    let calls = json!({"choices":[{"message":{"role":"assistant","content":"","tool_calls":[
        {"id":"rejected","type":"function","function":{"name":"propose_steps","arguments":"{\"steps\":[]}"}},
        {"id":"malformed","type":"function","function":{"name":"propose_steps","arguments":"{"}},
        {"id":"nonstring","type":"function","function":{"name":"propose_steps","arguments":{}}},
        {"id":"success","type":"function","function":{"name":"pending_questions","arguments":"{}"}}
    ]}}]}).to_string();
    let (result, sent, events) = native_result_after(vec![
        (200, calls),
        turn(json!(json!({"result":"done","accepted":true}).to_string())),
    ])
    .await;
    assert!(result.is_ok());
    assert_eq!(events.len(), 4);
    for event in &events[..3] {
        assert_eq!(event["success"], false);
        assert_eq!(event["attempt"], "test");
        assert_eq!(event["tool"], "propose_steps");
        assert!(event["duration_ms"].is_u64());
        assert_eq!(event["error_truncated"], false);
        assert!(!event["error"].as_str().unwrap().is_empty());
    }
    assert!(
        events[0]["error"]
            .as_str()
            .unwrap()
            .contains("only an actively assigned planner")
    );
    assert!(events[1]["error"].as_str().unwrap().contains("EOF"));
    assert!(events[2]["error"].as_str().unwrap().contains("JSON string"));
    assert_eq!(events[3]["success"], true);
    assert!(events[3]["error"].is_null());
    assert!(events[3].get("result").is_none());
    let messages = sent[2]["messages"].as_array().unwrap();
    let replies: Vec<_> = messages.iter().filter(|m| m["role"] == "tool").collect();
    assert_eq!(replies.len(), 4);
    assert!(
        replies[0]["content"]
            .as_str()
            .unwrap()
            .contains("only an actively assigned planner")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn native_planner_corrects_a_nested_step_after_receiving_its_field_error() {
    let proposal = |id: &str, steps: Value| {
        (200, json!({"choices":[{"message":{
        "role":"assistant","content":"","tool_calls":[{"id":id,"type":"function","function":{
            "name":"propose_steps","arguments":json!({"steps":steps}).to_string()
        }}]
    }}]}).to_string())
    };
    let (result, sent, events) = native_result_after_as(vec![
        proposal("invalid", json!([{"id":"implement_json","needs":"init"}])),
        proposal("corrected", json!([
            {"id":"implement_json","instructions":"Add JSON output","scope":["cli.py"],"tools":["read_file","apply_patch","command"]},
            {"id":"verify_json","kind":"command","needs":["implement_json"],"command":["python3","-m","unittest"],"when":{"step":"implement_json","status":"succeeded"}}
        ])),
        turn(json!(json!({"result":"planned","accepted":true}).to_string())),
    ], true).await;
    assert!(result.is_ok(), "{result:?}");
    let error_reply = sent[2]["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(error_reply["role"], "tool");
    assert!(
        error_reply["content"]
            .as_str()
            .unwrap()
            .contains("steps[0].needs")
    );
    let accepted_reply: Value = serde_json::from_str(
        sent[3]["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        accepted_reply["steps"],
        json!(["implement_json", "verify_json"])
    );
    assert!(accepted_reply["revision"].is_number());
    assert_eq!(events[0]["success"], false);
    assert_eq!(events[1]["success"], true);
    let tools = sent[1]["tools"].as_array().unwrap();
    let proposal_tool = tools
        .iter()
        .find(|t| t["function"]["name"] == "propose_steps")
        .unwrap();
    assert_eq!(
        proposal_tool["function"]["parameters"]["properties"]["steps"]["items"]["properties"]["needs"]
            ["items"]["type"],
        "string"
    );
    assert_eq!(sent[1]["tools"], sent[2]["tools"]);
    assert_eq!(sent[2]["tools"], sent[3]["tools"]);
}
