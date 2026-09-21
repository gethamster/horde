use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    process::{Command, Stdio},
};

const FAKE_PLAYWRIGHT: &str = r#"
const fs = require('node:fs');
let saved = false;
let webSocketHandler;
const element = { tagName: 'BUTTON', labels: [], innerText: 'Save', isConnected: true,
  getAttribute() { return null; } };
const handle = { async isVisible() { return true; }, async isEnabled() { return true; },
  async evaluate(fn) { return fn(element); }, async click() { saved = true; },
  async fill() { throw new Error('unexpected fill'); }, async selectOption() { throw new Error('unexpected select'); } };
const page = { url() { return 'http://127.0.0.1:1234/'; }, async goto() {
    if (process.env.HORDE_TEST_WEBSOCKET) {
      if (!webSocketHandler) throw new Error('WebSocket routing not installed');
      const connections = [];
      for (const url of ['ws://127.0.0.1:1234/ws', 'wss://elsewhere.example/ws']) {
        const route = { url() { return url; }, async close() { connections.push('closed'); },
          connectToServer() { connections.push('connected'); } };
        await webSocketHandler(route);
      }
      if (connections.join(',') !== 'connected,closed') throw new Error('WebSocket origin rule failed');
    }
  }, on() {},
  locator() { return { async count() { return 1; }, nth() { return { async elementHandle() { return handle; } }; } }; },
  async waitForTimeout() {}, getByText() { return { first() { return { async isVisible() { return saved; } }; } }; },
  async screenshot({path}) { fs.writeFileSync(path, Buffer.from('89504e470d0a1a0a','hex')); } };
module.exports = { chromium: { async launch() {
  if (process.env.TYPESAFE_API_KEY) throw new Error('provider key reached driver');
  return { async newContext() { return { async route() {}, async routeWebSocket(_pattern, handler) { webSocketHandler = handler; }, async newPage() { return page; }, async close() {} }; }, async close() {} };
} } };
"#;

fn run_case(first: Value, expect_pass: bool) {
    if Command::new("node").arg("--version").output().is_err() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path();
    std::fs::create_dir_all(workspace.join("node_modules/playwright")).unwrap();
    std::fs::write(workspace.join("package.json"), "{}").unwrap();
    std::fs::write(
        workspace.join("node_modules/playwright/index.js"),
        FAKE_PLAYWRIGHT,
    )
    .unwrap();
    std::fs::write(
        workspace.join("driver.mjs"),
        include_str!("../src/browser_driver.mjs"),
    )
    .unwrap();
    let screenshot = workspace.join("shot.png");
    let mut child = Command::new("node")
        .arg(workspace.join("driver.mjs"))
        .current_dir(workspace)
        .env_remove("TYPESAFE_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    writeln!(stdin, "{}", json!({"cmd":"start","url":"http://127.0.0.1:1234/","values":{},"assertions":[{"kind":"text_visible","text":"Saved"}],"screenshot":screenshot})).unwrap();
    stdin.flush().unwrap();
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let observation: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(observation["kind"], "observation");
    assert_eq!(observation["controls"][0]["id"], "c0");
    let first = if first["cmd"] == "act" {
        json!({"cmd":"act","version":observation["version"],"id":"c0","operation":"click"})
    } else {
        first
    };
    writeln!(stdin, "{first}").unwrap();
    stdin.flush().unwrap();
    line.clear();
    stdout.read_line(&mut line).unwrap();
    let result: Value = serde_json::from_str(&line).unwrap();
    if expect_pass {
        assert_eq!(result["kind"], "observation");
        writeln!(stdin, "{}", json!({"cmd":"assert"})).unwrap();
        stdin.flush().unwrap();
        line.clear();
        stdout.read_line(&mut line).unwrap();
        let result: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(result["kind"], "result");
        assert_eq!(result["passed"], true);
        assert!(screenshot.exists());
    } else {
        assert_eq!(result["kind"], "result");
        assert_eq!(result["passed"], false);
    }
    drop(stdin);
    assert!(child.wait().unwrap().success());
}
#[test]
fn embedded_driver_checks_actions_and_independent_done_assertions() {
    run_case(json!({"cmd":"act"}), true);
    run_case(json!({"cmd":"assert"}), false);
}
#[test]
fn embedded_driver_rejects_stale_control_versions() {
    if Command::new("node").arg("--version").output().is_err() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path();
    std::fs::create_dir_all(workspace.join("node_modules/playwright")).unwrap();
    std::fs::write(workspace.join("package.json"), "{}").unwrap();
    std::fs::write(
        workspace.join("node_modules/playwright/index.js"),
        FAKE_PLAYWRIGHT,
    )
    .unwrap();
    std::fs::write(
        workspace.join("driver.mjs"),
        include_str!("../src/browser_driver.mjs"),
    )
    .unwrap();
    let mut child = Command::new("node")
        .arg(workspace.join("driver.mjs"))
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    writeln!(stdin, "{}", json!({"cmd":"start","url":"http://127.0.0.1:1234/","values":{},"assertions":[{"kind":"text_visible","text":"Saved"}],"screenshot":null})).unwrap();
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    writeln!(
        stdin,
        "{}",
        json!({"cmd":"act","version":0,"id":"c0","operation":"click"})
    )
    .unwrap();
    line.clear();
    stdout.read_line(&mut line).unwrap();
    let result: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(result["kind"], "unsupported");
    assert!(result["reason"].as_str().unwrap().contains("stale"));
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

#[test]
fn embedded_driver_blocks_cross_origin_websockets() {
    if Command::new("node").arg("--version").output().is_err() {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path();
    std::fs::create_dir_all(workspace.join("node_modules/playwright")).unwrap();
    std::fs::write(workspace.join("package.json"), "{}").unwrap();
    std::fs::write(
        workspace.join("node_modules/playwright/index.js"),
        FAKE_PLAYWRIGHT,
    )
    .unwrap();
    std::fs::write(
        workspace.join("driver.mjs"),
        include_str!("../src/browser_driver.mjs"),
    )
    .unwrap();
    let mut child = Command::new("node")
        .arg(workspace.join("driver.mjs"))
        .current_dir(workspace)
        .env("HORDE_TEST_WEBSOCKET", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    writeln!(stdin, "{}", json!({"cmd":"start","url":"http://127.0.0.1:1234/","assertions":[{"kind":"url_path","path":"/"}],"screenshot":null})).unwrap();
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let observation: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(observation["kind"], "observation");
    assert_eq!(
        observation["unsupported"][0],
        "cross-origin WebSocket blocked"
    );
    drop(stdin);
    assert!(child.wait().unwrap().success());
}
