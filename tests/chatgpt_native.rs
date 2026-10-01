use horde::{config::ExecutorConfig, executor::chatgpt_models};
use serde_json::json;
use std::{
    io::{Read, Write},
    net::TcpListener,
};

fn catalog(body: serde_json::Value) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            socket.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with("GET /v1/models "));
        assert!(
            request
                .to_lowercase()
                .contains("authorization: bearer mock-token")
        );
        let body = body.to_string();
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    });
    (format!("http://{address}/v1"), handle)
}

#[tokio::test]
async fn model_catalog_preserves_account_specific_slug() {
    let (base_url, server) = catalog(
        json!({"models":[{"slug":"account-model-exact","display_name":"Account model","visibility":"list"}]}),
    );
    let config = ExecutorConfig {
        kind: "chatgpt".into(),
        base_url,
        model: Some("account-model-exact".into()),
        ..Default::default()
    };
    let result = chatgpt_models(&config, "mock-token").await.unwrap();
    assert_eq!(result["model"], "account-model-exact");
    assert_eq!(result["models"][0]["slug"], "account-model-exact");
    server.join().unwrap();
}

#[tokio::test]
async fn hidden_model_cannot_be_selected_or_substituted() {
    let (base_url, server) = catalog(
        json!({"models":[{"slug":"hidden-model","visibility":"hidden"},{"slug":"visible-model","visibility":"list"}]}),
    );
    let config = ExecutorConfig {
        kind: "chatgpt".into(),
        base_url,
        model: Some("hidden-model".into()),
        ..Default::default()
    };
    let error = chatgpt_models(&config, "mock-token").await.unwrap_err();
    assert!(error.to_string().contains("no model substituted"));
    server.join().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn native_worker_completes_only_after_completed_stream_and_retains_reasoning() {
    native_case(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn signout_during_stream_prevents_tool_execution() {
    native_case(true).await;
}

async fn native_case(signout: bool) {
    use horde::{
        accounts,
        config::{Provider, Settings},
        executor::{Invocation, execute},
        store::{Store, now},
        template::{self, Step},
    };
    use std::collections::BTreeMap;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (sender, receiver) = std::sync::mpsc::channel::<(std::path::PathBuf, String)>();
    let server = std::thread::spawn(move || {
        let (root, account) = receiver.recv().unwrap();
        for turn in 0..if signout { 2 } else { 3 } {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            let mut headers = Vec::new();
            let mut byte = [0];
            while !headers.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
            }
            let headers = String::from_utf8(headers).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|v| v.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let mut body = vec![0; length];
            socket.read_exact(&mut body).unwrap();
            let response = if turn == 0 {
                json!({"models":[{"slug":"exact-model","visibility":"list"}]}).to_string()
            } else {
                let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(request["store"], false);
                assert_eq!(request["stream"], true);
                assert!(request.get("max_tokens").is_none());
                assert!(request.get("previous_response_id").is_none());
                if turn == 2 {
                    assert!(
                        request["input"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|item| item["type"] == "reasoning"
                                && item["encrypted_content"] == "opaque-reasoning")
                    );
                    assert!(
                        request["input"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|item| item["type"] == "function_call_output"
                                && item["call_id"] == "questions")
                    );
                }
                let output = if turn == 1 {
                    json!([
                        {"id":"rs_1","type":"reasoning","summary":[],"encrypted_content":"opaque-reasoning"},
                        {"id":"fc_1","type":"function_call","namespace":"horde","name":"pending_questions","call_id":"questions","arguments":"{}","status":"completed"}
                    ])
                } else {
                    json!([{ "id":"fc_2","type":"function_call","namespace":"horde","name":"complete_step","call_id":"finish","arguments":json!({"result":"done","accepted":true,"artifacts":[]}).to_string(),"status":"completed"}])
                };
                format!(
                    "event: response.completed\ndata: {}\n\n",
                    json!({"type":"response.completed","response":{"id":"resp_1","status":"completed","output":output,"usage":{"input_tokens":5,"output_tokens":7}}})
                )
            };
            if signout && turn == 1 {
                let db = Store::open(&root).unwrap();
                db.conn
                    .execute("UPDATE accounts SET authenticated=0 WHERE id=?", [&account])
                    .unwrap();
            }
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",if turn==0 {"application/json"}else{"text/event-stream"},response.len(),response).unwrap();
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(&dir.path().join("data")).unwrap();
    let account = accounts::dispatch(&db,"account_create",&json!({"project":"default","name":"chatgpt","provider":"chatgpt","auth_mode":"login","base_url":"https://api.openai.com/v1"})).unwrap().unwrap()["id"].as_str().unwrap().to_owned();
    sender.send((db.root.clone(), account.clone())).unwrap();
    let registration = json!({"issuer":"https://auth.openai.com","subject":"mock-user","email":null,"client_id":"oaiapp_test","ext_agent_host_id":"urn:uuid:00000000-0000-4000-8000-000000000001","access_token":"mock-token","refresh_token":"mock-refresh","id_token":"mock-id","token_type":"Bearer","scopes":["resource.invoke","chatgpt.tokens.use.direct"],"expires_at":now()+3600});
    accounts::set_credential(
        &db,
        "default",
        &account,
        &accounts::Credential {
            kind: "chatgpt_oauth".into(),
            secret: registration.to_string(),
            expires_at: None,
            metadata: json!({}),
        },
    )
    .unwrap();
    let settings = Settings {
        providers: BTreeMap::from([(
            "default".into(),
            Provider {
                kind: "chatgpt".into(),
                auth_mode: "login".into(),
                account: Some(account.clone()),
                model: Some("exact-model".into()),
                base_url,
                ..Default::default()
            },
        )]),
        executors: BTreeMap::from([("worker".into(), Default::default())]),
        ..Default::default()
    };
    let plan = template::compile(
        "simulated",
        &template::load_templates(std::path::Path::new("absent")).unwrap(),
        BTreeMap::from([("task".into(), "think".into())]),
    )
    .unwrap();
    let task = db.submit("think", dir.path(), &settings, &plan).unwrap();
    let step_id = db.steps(&task).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let worker = db.register(&task, Some(&step_id)).unwrap();
    db.conn
        .execute(
            "INSERT INTO attempts(id,step,worker,state,started) VALUES('test',?,?,'running',?)",
            rusqlite::params![step_id, worker["id"].as_str().unwrap(), now()],
        )
        .unwrap();
    let profile: String = db
        .conn
        .query_row(
            "SELECT id FROM auth_profiles WHERE account=?",
            [&account],
            |row| row.get(0),
        )
        .unwrap();
    horde::project_runtime::record(
        &db,
        &task,
        "test",
        Some(&accounts::Binding {
            role: "worker".into(),
            account: Some(account),
            profile: Some(profile),
            credential_version: Some(1),
        }),
    )
    .unwrap();
    let spec:Step = serde_json::from_value(json!({"id":"native","role":"worker","tools":[],"instructions":"think","skills":settings.skills.keys().collect::<Vec<_>>()})).unwrap();
    let invocation = Invocation {
        db: &db,
        task: &task,
        step: &step_id,
        attempt: "test",
        worker: worker["id"].as_str().unwrap(),
        token: worker["token"].as_str().unwrap(),
        workspace: dir.path(),
        spec: &spec,
        settings: &settings,
        context: json!({}),
    };
    let result = execute(&invocation).await;
    if signout {
        assert!(result.unwrap_err().to_string().contains("signed out"));
        let events = db
            .rows(
                "SELECT data FROM events WHERE task=? AND kind='tool.completed'",
                &[&task],
            )
            .unwrap();
        assert!(events.is_empty());
    } else {
        let result = result.unwrap();
        assert_eq!(result["result"], "done");
        assert_eq!(result["usage"]["requests"].as_array().unwrap().len(), 2);
    }
    server.join().unwrap();
}
