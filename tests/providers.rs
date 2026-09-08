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
    native_result_after_configured(turns, planner, |_| {}).await
}
async fn native_result_after_configured(
    turns: Vec<(u16, String)>,
    planner: bool,
    configure: impl FnOnce(&mut Settings),
) -> (Result<Value, String>, Vec<Value>, Vec<Value>) {
    let mut responses = vec![(
        200,
        json!({"data":[{"id":"z-ai/glm-5.3-flash"}]}).to_string(),
    )];
    responses.extend(turns);
    let (url, requests, handle) = server(responses);
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(&dir.path().join("data")).unwrap();
    let mut settings = Settings {
        providers: BTreeMap::from([("default".into(), provider(url))]),
        executors: BTreeMap::from([
            ("worker".into(), Default::default()),
            ("planner".into(), Default::default()),
        ]),
        ..Default::default()
    };
    configure(&mut settings);
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
        serde_json::from_value(json!({"id":"native","role":if planner {"planner"} else {"worker"},"tools":[],"instructions":"think","skills":settings.skills.keys().collect::<Vec<_>>()})).unwrap();
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

#[tokio::test]
async fn auto_model_requires_one_named_catalog_entry() {
    for (data, expected) in [
        (
            json!([{ "id":"/models/large-pack" }]),
            Some("/models/large-pack"),
        ),
        (json!([]), None),
        (json!([{ "id":"a" }, { "id":"b" }]), None),
        (json!([{ "id":"" }]), None),
    ] {
        let (url, _, handle) = server(vec![(200, json!({"data":data}).to_string())]);
        let mut cfg = config(url);
        cfg.model = Some("auto".into());
        let result = horde::executor::probe(&cfg).await;
        handle.join().unwrap();
        if let Some(model) = expected {
            assert_eq!(result.unwrap()["model"], model);
        } else {
            assert!(result.is_err());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_options_are_merged_and_auto_model_is_resolved_before_dispatch() {
    let (result, sent, _) = native_result_after_configured(
        vec![turn(json!(
            json!({"result":"done","accepted":true}).to_string()
        ))],
        false,
        |settings| {
            let provider = settings.providers.get_mut("default").unwrap();
            provider.model = Some("auto".into());
            provider.extra_body = BTreeMap::from([
                ("temperature".into(), json!(0.2)),
                ("top_p".into(), json!(0.9)),
                ("repetition_penalty".into(), json!(1.1)),
                (
                    "chat_template_kwargs".into(),
                    json!({"enable_thinking":false}),
                ),
            ]);
            settings.executors.get_mut("worker").unwrap().max_tokens = Some(37);
        },
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(sent[1]["model"], "z-ai/glm-5.3-flash");
    assert_eq!(sent[1]["max_tokens"], 37);
    assert_eq!(sent[1]["temperature"], json!(0.2));
    assert_eq!(sent[1]["chat_template_kwargs"]["enable_thinking"], false);
    assert!(sent[1].get("extra_body").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn native_repeated_tool_call_gets_a_completion_reminder_then_stops() {
    let response = (200,json!({"choices":[{"message":{"role":"assistant","content":"","tool_calls":[{"id":"call","type":"function","function":{"name":"pending_questions","arguments":"{}"}}]}}]}).to_string());
    let (result, sent, events) = native_result_after(vec![response; 4]).await;
    assert!(result.unwrap_err().contains("task held for inspection"));
    assert_eq!(events.len(), 3, "fourth identical call must not execute");
    assert!(
        sent[4]["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("final JSON")
    );
    assert!(
        sent[1]["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("Completion:")
    );
}

fn streamed(values: Vec<Value>, done: bool) -> (u16, String) {
    let mut body = values
        .into_iter()
        .map(|v| format!("data: {v}\n\n"))
        .collect::<String>();
    if done {
        body.push_str("data: [DONE]\n\n");
    }
    (200, body)
}
#[tokio::test(flavor = "current_thread")]
async fn native_stream_executes_completed_calls_and_keeps_provider_telemetry() {
    let (result, sent, events) = native_result_after_configured(vec![
        streamed(vec![
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call1","function":{"name":"pending_","arguments":"{"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"questions","arguments":"}"}}]},"finish_reason":"tool_calls"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":20,"completion_tokens":10,"prefix_tokens_reused":15},"speculation":{"draft_tokens":12,"accepted_tokens":8}}),
        ], true),
        streamed(vec![
            json!({"choices":[{"delta":{"content":json!({"result":"done","accepted":true}).to_string()},"finish_reason":"stop"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":30,"completion_tokens":5}}),
        ], true),
    ], false, |s| s.providers.get_mut("default").unwrap().stream = true).await;
    let result = result.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["result_summary"], "[]");
    assert_eq!(events[0]["result_truncated"], false);
    assert_eq!(sent[1]["stream"], true);
    assert_eq!(
        sent[2]["messages"][2]["tool_calls"][0]["function"]["name"],
        "pending_questions"
    );
    assert_eq!(
        result["usage"]["requests"][0]["provider"]["prefix_tokens_reused"],
        15
    );
    assert_eq!(
        result["usage"]["requests"][0]["provider_extras"]["speculation"]["accepted_tokens"],
        8
    );
    assert_eq!(horde::metrics::totals(&result["usage"]).0, Some(65));
}
#[tokio::test(flavor = "current_thread")]
async fn interrupted_native_stream_does_not_execute_a_tool() {
    let (result, _, events) = native_result_after_configured(vec![streamed(vec![
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call1","function":{"name":"pending_questions","arguments":"{}"}}]}}]})
    ], false)], false, |s| s.providers.get_mut("default").unwrap().stream = true).await;
    assert!(result.is_err());
    assert!(events.is_empty());
}
#[test]
fn doctor_probes_a_named_provider_with_auto_model() {
    let (url, requests, handle) = server(vec![
        (200, json!({"data":[{"id":"/models/local"}]}).to_string()),
        streamed(
            vec![
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"ping1","function":{"name":"ping","arguments":"{\"value\":\"ok\"}"}}]}}]}),
            ],
            true,
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".horde")).unwrap();
    std::fs::write(dir.path().join(".horde.toml"), format!("[providers.local]\nkind = \"tuara\"\nauth_mode = \"api\"\nbase_url = {url:?}\napi_key_env = \"PATH\"\nmodel = \"auto\"\n")).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_horde"))
        .args(["doctor", "--probe", "--provider", "local", "--repo"])
        .arg(dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["streaming_tool_calls_verified"], true);
    assert_eq!(result["model"], "/models/local");
    handle.join().unwrap();
    assert_eq!(requests.lock().unwrap()[1]["stream"], true);
}

#[tokio::test(flavor = "current_thread")]
async fn native_worker_receives_selected_skill_and_reads_its_pinned_reference() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("references")).unwrap();
    std::fs::write(
        dir.path().join("SKILL.md"),
        "Follow the report skill. Read references/style.md before writing.",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("references/style.md"),
        "Name each metric and its unit.",
    )
    .unwrap();
    let (result, sent, events) = native_result_after_configured(vec![
        (200,json!({"choices":[{"message":{"role":"assistant","content":"","tool_calls":[{"id":"read1","type":"function","function":{"name":"read_skill","arguments":json!({"name":"report","path":"references/style.md"}).to_string()}}]}}]}).to_string()),
        turn(json!(json!({"result":"report complete","accepted":true}).to_string())),
    ], false, |s| {s.skills.insert("report".into(),dir.path().to_owned());}).await;
    assert!(result.is_ok(), "{result:?}");
    assert!(
        sent[1]["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("Follow the report skill.")
    );
    let reply = sent[2]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap();
    assert!(
        reply["content"]
            .as_str()
            .unwrap()
            .contains("Name each metric and its unit.")
    );
    assert_eq!(events[0]["tool"], "read_skill");
    assert_eq!(events[0]["success"], true);
}

#[tokio::test]
async fn ambiguous_auto_model_names_every_catalog_id() {
    let (url, _, handle) = server(vec![(
        200,
        json!({"data":[{"id":"/models/first"},{"id":"/models/second"}]}).to_string(),
    )]);
    let mut cfg = config(url);
    cfg.model = Some("auto".into());
    let error = horde::executor::probe(&cfg).await.unwrap_err().to_string();
    handle.join().unwrap();
    assert!(
        error.contains("/models/first") && error.contains("/models/second"),
        "{error}"
    );
    assert!(error.contains("explicit model ID"));
}
#[test]
fn ordinary_doctor_resolves_auto_from_a_minimal_default_provider_block() {
    let (url, requests, handle) = server(vec![(
        200,
        json!({"data":[{"id":"/models/local-only"}]}).to_string(),
    )]);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".horde.toml"),
        format!(
            "[providers.default]\nbase_url = {url:?}\napi_key_env = \"PATH\"\nmodel = \"auto\"\n"
        ),
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_horde"))
        .args(["doctor", "--repo"])
        .arg(dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["resolved_models"]["default"]["model"],
        "/models/local-only"
    );
    handle.join().unwrap();
    assert_eq!(
        requests.lock().unwrap().as_slice(),
        &[json!({"method":"GET"})]
    );
}
