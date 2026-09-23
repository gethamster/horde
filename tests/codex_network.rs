//! `network = true` lets a Codex executor reach the network from its
//! workspace-write sandbox. The default keeps network access off.
use horde::config::Settings;
use serde_json::{Value, json};
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, path::Path};

const OVERRIDE: &str = "sandbox_workspace_write.network_access=true";

fn load(config: &str) -> Settings {
    let dir = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    // Operator (user) configuration: repository files cannot enable network.
    std::fs::write(user.path().join("config.toml"), config).unwrap();
    Settings::load_with_user_dir(dir.path(), user.path()).unwrap()
}

#[test]
fn network_defaults_off_and_a_role_overrides_its_provider() {
    let settings = load(
        r#"
[providers.sandboxed]
kind = "codex"
[providers.online]
kind = "codex"
network = true
[executors.worker]
provider = "sandboxed"
[executors.reviewer]
provider = "sandboxed"
network = true
[executors.planner]
provider = "online"
[executors.native]
provider = "online"
network = false
"#,
    );
    assert!(!settings.executor("worker").unwrap().network);
    assert!(settings.executor("reviewer").unwrap().network);
    assert!(settings.executor("planner").unwrap().network);
    assert!(!settings.executor("native").unwrap().network);
    // The shipped codex provider keeps the sandbox offline.
    assert!(!Settings::default().executor("codex").unwrap().network);
}

#[test]
fn an_unset_knob_leaves_serialized_configuration_unchanged() {
    // Configuration hashes and settings sent to older peers must not change
    // unless the operator turns network access on.
    let settings = Settings::default();
    let config = settings.executor("codex").unwrap();
    let serialized = serde_json::to_value(&config).unwrap();
    assert!(serialized.get("network").is_none());
    assert!(
        !serde_json::to_string(&settings)
            .unwrap()
            .contains("network")
    );
    let mut online = config.clone();
    online.network = true;
    assert_eq!(serde_json::to_value(&online).unwrap()["network"], true);
    assert_ne!(
        horde::capabilities::configuration_hash(&config).unwrap(),
        horde::capabilities::configuration_hash(&online).unwrap()
    );
}

/// A fake `codex exec` that records its argv and replies with an accepted step.
fn mock(dir: &Path) -> std::path::PathBuf {
    let program = dir.join("mock-codex");
    std::fs::write(
        &program,
        format!(
            r#"#!/usr/bin/env python3
import json, sys
sys.stdin.read()
json.dump(sys.argv[1:], open({log:?}, "w"))
print(json.dumps({{"type":"item.completed","item":{{"type":"agent_message","text":json.dumps({{"result":"checked","accepted":True}})}}}}))
print(json.dumps({{"type":"turn.completed","usage":{{"input_tokens":1,"output_tokens":1}}}}))
"#,
            log = dir.join("argv.json").to_str().unwrap()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    program
}

async fn codex_argv(network: Option<bool>) -> Vec<String> {
    let temp = tempfile::tempdir().unwrap();
    let program = mock(temp.path());
    let db = horde::store::Store::open(&temp.path().join("data")).unwrap();
    let settings = Settings {
        allow_commands: true,
        providers: BTreeMap::from([(
            "codex".into(),
            horde::config::Provider {
                kind: "codex".into(),
                auth_mode: "login".into(),
                program: Some(program.to_string_lossy().into_owned()),
                ..Default::default()
            },
        )]),
        executors: BTreeMap::from([(
            "reviewer".into(),
            horde::config::Executor {
                provider: Some("codex".into()),
                network,
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
    step.role = "reviewer".into();
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
    let result = horde::executor::execute(&invocation).await.unwrap();
    assert_eq!(result["result"], "checked");
    let argv: Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("argv.json")).unwrap()).unwrap();
    serde_json::from_value(argv).unwrap()
}

fn has_override(argv: &[String]) -> bool {
    argv.windows(2)
        .any(|pair| pair[0] == "-c" && pair[1] == OVERRIDE)
}

#[tokio::test]
async fn codex_sandbox_stays_offline_by_default() {
    let argv = codex_argv(None).await;
    assert!(argv.iter().any(|arg| arg == "workspace-write"));
    assert!(!argv.iter().any(|arg| arg.contains("network_access")));
    let argv = codex_argv(Some(false)).await;
    assert!(!argv.iter().any(|arg| arg.contains("network_access")));
}

#[tokio::test]
async fn network_setting_opens_the_codex_sandbox() {
    let argv = codex_argv(Some(true)).await;
    assert!(has_override(&argv), "{argv:?}");
    // The sandbox mode itself is unchanged; only its network rule is relaxed.
    assert!(
        argv.windows(2)
            .any(|pair| pair[0] == "--sandbox" && pair[1] == "workspace-write")
    );
    // The prompt still arrives on stdin as the final argument.
    assert_eq!(argv.last().map(String::as_str), Some("-"));
}
