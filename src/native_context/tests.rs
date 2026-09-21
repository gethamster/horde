use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
static CONFIG_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
struct EnvironmentGuard {
    xdg: Option<std::ffi::OsString>,
    key: Option<std::ffi::OsString>,
}
impl EnvironmentGuard {
    fn install(xdg: &std::path::Path, key: &str) -> Self {
        let old = Self {
            xdg: std::env::var_os("XDG_CONFIG_HOME"),
            key: std::env::var_os("HORDE_NATIVE_CONTEXT_TEST_KEY"),
        };
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", xdg);
            std::env::set_var("HORDE_NATIVE_CONTEXT_TEST_KEY", key);
        }
        old
    }
}
impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = self.xdg.take() {
                std::env::set_var("XDG_CONFIG_HOME", value)
            } else {
                std::env::remove_var("XDG_CONFIG_HOME")
            }
            if let Some(value) = self.key.take() {
                std::env::set_var("HORDE_NATIVE_CONTEXT_TEST_KEY", value)
            } else {
                std::env::remove_var("HORDE_NATIVE_CONTEXT_TEST_KEY")
            }
        }
    }
}
fn pair(name: &str, id: &str, result: Value) -> Vec<Value> {
    vec![
        json!({"role":"assistant","content":"","tool_calls":[{"id":id,"type":"function","function":{"name":name,"arguments":"{\"path\":\"a\",\"pattern\":\"a\"}"}}]}),
        json!({"role":"tool","tool_call_id":id,"content":result.to_string()}),
    ]
}
fn prefix() -> Vec<Value> {
    vec![
        json!({"role":"system","content":"rules"}),
        json!({"role":"user","content":"task"}),
    ]
}
fn task(db: &Store, settings: &Settings) -> (String, String, String) {
    let plan = crate::template::compile(
        "simulated",
        &crate::template::load_templates(std::path::Path::new("absent")).unwrap(),
        std::collections::BTreeMap::from([("task".into(), "native context".into())]),
    )
    .unwrap();
    let task = db
        .submit("native context", &db.root, settings, &plan)
        .unwrap();
    let step = db.steps(&task).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let worker = db.register(&task, Some(&step)).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let attempt = crate::store::id();
    db.conn
        .execute(
            "INSERT INTO attempts(id,step,worker,state,started) VALUES(?,?,?,'running',0)",
            rusqlite::params![attempt, step, worker],
        )
        .unwrap();
    (task, step, attempt)
}
#[test]
fn only_old_completed_read_only_groups_are_candidates() {
    let mut messages = prefix();
    messages.extend(pair("read_file", "a", json!({"content":"ok"})));
    messages.extend(pair("write_file", "b", json!({"written":true})));
    messages.extend(pair("search", "c", json!({"matches":"ok"})));
    messages.extend(pair("read_file", "d", json!({"error":"failed"})));
    for id in ["e", "f", "g", "h"] {
        messages.extend(pair("read_file", id, json!({"content":"ok"})));
    }
    let found = groups(&messages).unwrap();
    assert_eq!(found.len(), 3);
    assert_eq!(found[0].start, 2);
    assert_eq!(found[1].start, 6);
}
#[test]
fn unmatched_multicall_and_narrative_are_preserved() {
    let mut messages = prefix();
    let mut call = pair("read_file", "a", json!({"content":"ok"}));
    call[0]["tool_calls"].as_array_mut().unwrap().push(json!({"id":"b","type":"function","function":{"name":"search","arguments":"{\"pattern\":\"x\"}"}}));
    messages.extend(call);
    for id in ["c", "d", "e", "f"] {
        messages.extend(pair("read_file", id, json!({"content":"ok"})));
    }
    assert_eq!(groups(&messages).unwrap().len(), 1);
    messages[4]["content"] = json!("I found a requirement");
    assert!(groups(&messages).unwrap().is_empty());
}
#[test]
fn replacement_preserves_later_user_and_tool_order() {
    let mut messages = prefix();
    messages.extend(pair("read_file", "a", json!({"content":"secret"})));
    messages.push(json!({"role":"user","content":"new constraint"}));
    let candidate = groups(
        &[
            messages.clone(),
            pair("read_file", "b", json!({"content":"x"})),
            pair("read_file", "c", json!({"content":"x"})),
            pair("read_file", "d", json!({"content":"x"})),
        ]
        .concat(),
    )
    .unwrap();
    let result = proposed_messages(&messages, &candidate[..1], &[candidate[0].hash.clone()]);
    assert_eq!(result[0], messages[0]);
    assert_eq!(result[1], messages[1]);
    assert_eq!(result[3]["content"], "new constraint");
    assert!(
        result[2]["content"]
            .as_str()
            .unwrap()
            .contains(&candidate[0].hash)
    );
}
#[test]
fn retrieval_is_paged_attempt_scoped_and_hash_verified() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path()).unwrap();
    let (task, step, attempt) = task(&db, &Settings::default());
    let original = "[\"héllo\",\"world\"]";
    let hash = db
        .artifact(
            &task,
            Some(&step),
            &format!("native-context/{attempt}/decision/0"),
            original.as_bytes(),
            &json!({}),
            true,
        )
        .unwrap();
    let first = retrieve(&db, &task, &attempt, &hash, 0, 5).unwrap();
    assert_eq!(first["content"], "[\"hé");
    let next = first["next_offset"].as_u64().unwrap() as usize;
    assert_eq!(
        retrieve(&db, &task, &attempt, &hash, next, 8192).unwrap()["next_offset"],
        Value::Null
    );
    assert!(retrieve(&db, &task, "other", &hash, 0, 8).is_err());
    assert!(retrieve(&db, &task, &attempt, &hash, 1, 8193).is_err());
    assert!(retrieve(&db, &task, &attempt, &hash, 4, 8).is_err());
    assert!(retrieve(&db, &task, &attempt, &hash, 3, 1).is_err());
    std::fs::write(db.root.join("artifacts").join(&hash), "tampered").unwrap();
    assert!(retrieve(&db, &task, &attempt, &hash, 0, 8).is_err());
}
#[test]
fn reasoning_and_failed_or_incomplete_multicalls_never_become_candidates() {
    let mut messages = prefix();
    let mut protected = pair("read_file", "a", json!({"content":"ok"}));
    protected[0]["reasoning_content"] = json!("a finding that must survive");
    messages.extend(protected);
    let mut mixed = pair("read_file", "b", json!({"content":"ok"}));
    mixed[0]["tool_calls"].as_array_mut().unwrap().push(json!({"id":"c","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"a\"}"}}));
    mixed.push(json!({"role":"tool","tool_call_id":"c","content":"{\"written\":true}"}));
    messages.extend(mixed);
    for id in ["d", "e", "f", "g"] {
        messages.extend(pair("search", id, json!({"matches":"ok"})));
    }
    assert_eq!(groups(&messages).unwrap().len(), 1);
    messages.push(json!({"role":"assistant","content":"","tool_calls":[{"id":"pending","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a\"}"}}]}));
    assert_eq!(groups(&messages).unwrap().len(), 1);
}
#[test]
fn active_pruning_excludes_instruction_and_contract_reads() {
    let mut messages = prefix();
    messages.extend(pair("read_file", "agents", json!({"content":"mandatory"})));
    messages[2]["tool_calls"][0]["function"]["arguments"] = json!("{\"path\":\"AGENTS.md\"}");
    messages.extend(pair(
        "read_file",
        "contract",
        json!({"content":"acceptance terms"}),
    ));
    messages[4]["tool_calls"][0]["function"]["arguments"] =
        json!("{\"path\":\"docs/task-contract.md\"}");
    messages.extend(pair(
        "search",
        "search",
        json!({"matches":"AGENTS.md:1:mandatory"}),
    ));
    for id in ["a", "b", "c"] {
        messages.extend(pair(
            "search",
            id,
            json!({"matches":"src/lib.rs:1:pub fn safe() {}"}),
        ));
    }
    let candidates = groups(&messages).unwrap();
    assert_eq!(candidates.len(), 3);
    assert!(!candidates[0].active_safe);
    assert!(!candidates[1].active_safe);
    assert!(!candidates[2].active_safe);
}
#[test]
fn next_action_holds_only_with_a_deterministic_byte_blocker() {
    let mut response = crate::decision::DecisionResponse {
        answers: vec![Answer::Choice {
            id: "next_action".into(),
            answer: "hold".into(),
            confidence: 0.99,
            probabilities: std::collections::BTreeMap::from([
                ("continue".into(), 0.005),
                ("prune".into(), 0.005),
                ("hold".into(), 0.99),
            ]),
        }],
        usage: crate::decision::DecisionUsage::default(),
    };
    assert_eq!(next_action(&response, false), NextAction::Continue);
    assert_eq!(next_action(&response, true), NextAction::Hold);
    if let Answer::Choice {
        answer, confidence, ..
    } = &mut response.answers[0]
    {
        *answer = "prune".into();
        *confidence = 0.8;
    }
    assert_eq!(next_action(&response, true), NextAction::Continue);
}

#[test]
fn proposed_action_respects_validated_action_and_savings() {
    let decision = Decision {
        native_context_min_savings_bytes: 1024,
        native_context_min_savings_ratio_percent: 15,
        ..Decision::default()
    };
    assert_eq!(
        proposed_action(NextAction::Continue, 1, 10000, 20000, &decision),
        "continue"
    );
    assert_eq!(
        proposed_action(NextAction::Hold, 1, 10000, 20000, &decision),
        "hold"
    );
    assert_eq!(
        proposed_action(NextAction::Prune, 1, 1000, 20000, &decision),
        "continue"
    );
    assert_eq!(
        proposed_action(NextAction::Prune, 1, 2000, 20000, &decision),
        "continue"
    );
    assert_eq!(
        proposed_action(NextAction::Prune, 1, 4000, 20000, &decision),
        "prune"
    );
}

#[test]
fn evidence_redacts_before_truncating_at_a_secret_boundary() {
    let secret = "secret-with-a-long-tail";
    let text = format!("{}{}", "x".repeat(1098), secret);
    let values = std::collections::BTreeMap::from([("key".into(), secret.into())]);
    let excerpt = redacted_short(&text, &redactions(values, "jev-key", "provider-key"), 1100);
    assert!(!excerpt.contains("secret"));
    assert!(!excerpt.ends_with('s'));
}

#[test]
fn synthetic_credentials_cannot_overwrite_task_bundle_redactions() {
    let bundle = std::collections::BTreeMap::from([
        ("__native_context_key".into(), "bundle-secret-a".into()),
        ("__native_provider_key".into(), "bundle-secret-b".into()),
        (
            "__native_context_escaped_0".into(),
            "bundle-secret-c".into(),
        ),
    ]);
    let values = redactions(bundle, "jev-secret", "provider\"secret");
    let text = "bundle-secret-a bundle-secret-b bundle-secret-c jev-secret provider\\\"secret";
    let redacted = redacted_short(text, &values, 1000);
    for leaked in [
        "bundle-secret-a",
        "bundle-secret-b",
        "bundle-secret-c",
        "jev-secret",
        "provider\\\"secret",
    ] {
        assert!(!redacted.contains(leaked));
    }
}

#[test]
fn candidate_cap_keeps_the_largest_old_group_even_when_it_is_ninth() {
    let mut messages = prefix();
    for index in 0..9 {
        messages.extend(pair(
            "search",
            &format!("small{index}"),
            json!({"matches":"small"}),
        ));
    }
    let large_start = messages.len();
    messages.extend(pair(
        "search",
        "large",
        json!({"matches":"x".repeat(50000)}),
    ));
    for index in 0..3 {
        messages.extend(pair(
            "search",
            &format!("recent{index}"),
            json!({"matches":"recent"}),
        ));
    }
    let candidates = choose_candidates(
        groups(&messages).unwrap(),
        NativeContextMode::Active,
        &HashMap::new(),
    );
    assert_eq!(candidates.len(), 8);
    assert!(candidates.iter().any(|group| group.start == large_start));
}
#[tokio::test(flavor = "current_thread")]
async fn active_pruning_archives_exact_bytes_and_preserves_attempt_state() {
    let _config_lock = CONFIG_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = socket.read(&mut buffer).await.unwrap();
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|part| part.parse::<usize>().ok())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let body = String::from_utf8_lossy(&bytes);
        assert!(!body.contains("native-secret"));
        assert!(!body.contains("native\\\"secret"));
        assert!(!body.contains("tuara-secret"));
        assert!(!body.contains("tuara\\\"secret"));
        let response = r#"{"model":"jev-1.13.0","answers":{"g0":{"type":"choice","choice":"omit","confidence":0.99,"probabilities":{"keep":0.01,"omit":0.99}},"next_action":{"type":"choice","choice":"prune","confidence":0.99,"probabilities":{"continue":0.01,"prune":0.99}}},"usage":{"input_tokens":100,"output_tokens":1}}"#;
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.unwrap();
    });
    let config_dir = dir.path().join("xdg");
    std::fs::create_dir_all(config_dir.join("horde")).unwrap();
    let _environment = EnvironmentGuard::install(&config_dir, "native\"secret");
    let decision = Decision {
        mode: DecisionMode::Shadow,
        native_context_mode: NativeContextMode::Active,
        native_context_trigger_bytes: 8192,
        native_context_min_savings_bytes: 1024,
        native_context_min_savings_ratio_percent: 5,
        base_url,
        api_key_env: "HORDE_NATIVE_CONTEXT_TEST_KEY".into(),
        ..Decision::default()
    };
    let settings = Settings {
        decision: decision.clone(),
        ..Settings::default()
    };
    std::fs::write(
        config_dir.join("horde/config.toml"),
        toml::to_string(&settings).unwrap(),
    )
    .unwrap();
    let db = Store::open(dir.path()).unwrap();
    let (task, step, attempt) = task(&db, &settings);
    let mut messages = prefix();
    messages.extend(pair(
        "search",
        "a",
        json!({"matches":format!("{}native\"secret tuara\"secret", "x".repeat(20000))}),
    ));
    for id in ["b", "c", "d"] {
        messages.extend(pair("search", id, json!({"matches":"ok"})));
    }
    let mut conversation = Conversation::new(messages.clone()).unwrap();
    let old_bytes = conversation.range_bytes(0, messages.len()).unwrap();
    let mut seen = HashMap::new();
    maybe_prune(
        PruningContext {
            db: &db,
            task: &task,
            step: &step,
            attempt: &attempt,
            decision: &decision,
            provider_key: "tuara\"secret",
        },
        &mut conversation,
        &mut seen,
        1000,
    )
    .await
    .unwrap();
    assert_eq!(
        conversation.range_bytes(0, messages.len()).unwrap(),
        old_bytes
    );
    assert!(
        db.rows("SELECT id FROM decisions WHERE task=?", &[&task])
            .unwrap()
            .is_empty()
    );
    maybe_prune(
        PruningContext {
            db: &db,
            task: &task,
            step: &step,
            attempt: &attempt,
            decision: &decision,
            provider_key: "tuara\"secret",
        },
        &mut conversation,
        &mut seen,
        old_bytes.len(),
    )
    .await
    .unwrap();
    server.await.unwrap();
    let now = conversation.values().unwrap();
    assert_eq!(now.len(), 9);
    assert_eq!(now[0], messages[0]);
    assert_eq!(now[1], messages[1]);
    assert_eq!(now[3], messages[4]);
    let epoch = db
        .rows("SELECT * FROM native_context_epochs WHERE task=?", &[&task])
        .unwrap();
    assert_eq!(epoch.len(), 1);
    assert_eq!(epoch[0]["state"], "prepared");
    assert_eq!(epoch[0]["attempt"], attempt);
    assert_eq!(epoch[0]["old_hash"], crate::store::hash(&old_bytes));
    let rows = db
        .rows(
            "SELECT result,artifact_hash FROM decisions WHERE task=?",
            &[&task],
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    let listed = crate::decision::store::list(&db, &task, 0, 10).unwrap();
    assert_eq!(listed[0]["applied"], 0);
    assert_eq!(
        listed[0]["context_version"],
        crate::delegation::mandatory(&db, &task).unwrap()["version"]
    );
    assert_eq!(
        serde_json::from_str::<Value>(rows[0]["result"].as_str().unwrap()).unwrap()["applied"],
        false
    );
    mark_used(&db, &task, &attempt, "wrong-hash").unwrap();
    assert_eq!(
        crate::decision::store::list(&db, &task, 0, 10).unwrap()[0]["applied"],
        0
    );
    mark_used(&db, &task, &attempt, &conversation.hash().unwrap()).unwrap();
    assert_eq!(
        crate::decision::store::list(&db, &task, 0, 10).unwrap()[0]["applied"],
        1
    );
    assert_eq!(
        db.rows(
            "SELECT state FROM native_context_epochs WHERE task=?",
            &[&task]
        )
        .unwrap()[0]["state"],
        "used"
    );
    assert_eq!(
        serde_json::from_str::<Value>(
            db.rows("SELECT result FROM decisions WHERE task=?", &[&task])
                .unwrap()[0]["result"]
                .as_str()
                .unwrap()
        )
        .unwrap()["applied"],
        true
    );
    let evidence = std::fs::read(
        db.root
            .join("artifacts")
            .join(rows[0]["artifact_hash"].as_str().unwrap()),
    )
    .unwrap();
    let evidence_text = String::from_utf8(evidence).unwrap();
    assert!(!evidence_text.contains("native-secret"));
    assert!(!evidence_text.contains("native\\\"secret"));
    assert!(!evidence_text.contains("tuara-secret"));
    assert!(!evidence_text.contains("tuara\\\"secret"));
    let hash = now[2]["content"]
        .as_str()
        .unwrap()
        .split("SHA-256 ")
        .nth(1)
        .unwrap()
        .split('.')
        .next()
        .unwrap();
    let archive = retrieve(&db, &task, &attempt, hash, 0, 8192).unwrap();
    assert!(archive["content"].as_str().unwrap().starts_with("[{"));
    assert_eq!(
        db.rows("SELECT state FROM attempts WHERE id=?", &[&attempt])
            .unwrap()[0]["state"],
        "running"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn operator_revocation_after_jev_reply_preserves_the_old_conversation() {
    let _config_lock = CONFIG_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (observed_tx, observed_rx) = tokio::sync::oneshot::channel();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = socket.read(&mut buffer).await.unwrap();
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|part| part.parse::<usize>().ok())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        observed_tx.send(()).unwrap();
        reply_rx.await.unwrap();
        let response = r#"{"model":"jev-1.13.0","answers":{"g0":{"type":"choice","choice":"omit","confidence":0.99,"probabilities":{"keep":0.01,"omit":0.99}},"next_action":{"type":"choice","choice":"prune","confidence":0.99,"probabilities":{"continue":0.01,"prune":0.99}}}}"#;
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.unwrap();
    });
    let config_dir = dir.path().join("xdg");
    std::fs::create_dir_all(config_dir.join("horde")).unwrap();
    let _environment = EnvironmentGuard::install(&config_dir, "test-jev-key");
    let decision = Decision {
        mode: DecisionMode::Shadow,
        native_context_mode: NativeContextMode::Active,
        native_context_trigger_bytes: 8192,
        native_context_min_savings_bytes: 1024,
        native_context_min_savings_ratio_percent: 5,
        base_url,
        api_key_env: "HORDE_NATIVE_CONTEXT_TEST_KEY".into(),
        ..Decision::default()
    };
    let settings = Settings {
        decision: decision.clone(),
        ..Settings::default()
    };
    let config_path = config_dir.join("horde/config.toml");
    std::fs::write(&config_path, toml::to_string(&settings).unwrap()).unwrap();
    let db = Store::open(dir.path()).unwrap();
    let (task, step, attempt) = task(&db, &settings);
    let mut messages = prefix();
    messages.extend(pair("search", "a", json!({"matches":"x".repeat(20000)})));
    for id in ["b", "c", "d"] {
        messages.extend(pair("search", id, json!({"matches":"small"})));
    }
    let mut conversation = Conversation::new(messages).unwrap();
    let before = conversation.hash().unwrap();
    let mut seen = HashMap::new();
    let change = async {
        observed_rx.await.unwrap();
        let mut revoked = decision.clone();
        revoked.native_context_mode = NativeContextMode::Shadow;
        std::fs::write(
            &config_path,
            toml::to_string(&Settings {
                decision: revoked,
                ..Settings::default()
            })
            .unwrap(),
        )
        .unwrap();
        reply_tx.send(()).unwrap();
    };
    let (outcome, ()) = tokio::join!(
        maybe_prune(
            PruningContext {
                db: &db,
                task: &task,
                step: &step,
                attempt: &attempt,
                decision: &decision,
                provider_key: "provider-key"
            },
            &mut conversation,
            &mut seen,
            30000,
        ),
        change
    );
    outcome.unwrap();
    server.await.unwrap();
    assert_eq!(conversation.hash().unwrap(), before);
    assert!(
        db.rows(
            "SELECT decision FROM native_context_epochs WHERE task=?",
            &[&task]
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        crate::decision::store::list(&db, &task, 0, 10).unwrap()[0]["applied"],
        0
    );
}
