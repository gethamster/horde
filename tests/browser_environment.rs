use horde::{
    config::Settings,
    environment::{self, Environment},
    executor::Invocation,
    projects,
    store::Store,
    template,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const FAKE_PLAYWRIGHT: &str = r#"
const fs = require('node:fs');
let saved = false, currentUrl = '';
const element = { tagName:'BUTTON', labels:[], get innerText(){return fs.existsSync('secret-label')?fs.readFileSync('secret-label','utf8'):'Save'}, isConnected:true, getAttribute(){ return null; } };
const handle = { async isVisible(){return true}, async isEnabled(){return true}, async evaluate(fn){return fn(element)}, async click(){saved=true;if(fs.existsSync('lose-ack')) process.exit(0)}, async fill(){throw Error('unexpected fill')}, async selectOption(){throw Error('unexpected select')} };
const page = { url(){return currentUrl}, async goto(url){currentUrl=url;if(fs.existsSync('hang-browser')) await new Promise(()=>{})}, on(){},
  locator(){return {async count(){return 1}, nth(){return {async elementHandle(){return handle}}}}},
  async waitForTimeout(){}, getByText(){return {first(){return {async isVisible(){return saved}}}}},
  async screenshot({path}){fs.writeFileSync(path,Buffer.from('89504e470d0a1a0a','hex'))} };
module.exports={chromium:{async launch(){
  if(process.env.TYPESAFE_API_KEY || process.env.XDG_CONFIG_HOME) throw Error('credential reached browser driver');
  return {async newContext(){return {async route(){}, async routeWebSocket(){}, async newPage(){return page}, async close(){}}},async close(){}};
}}};
"#;

fn git(repo: &Path, args: &[&str]) {
    let result = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&result.stderr)
    );
}
fn decision_server(choices: Vec<(&'static str, f64)>) -> (String, std::thread::JoinHandle<()>) {
    decision_server_with_hook(choices, None)
}
fn decision_server_with_hook(
    choices: Vec<(&'static str, f64)>,
    mut hook: Option<Box<dyn FnOnce() + Send>>,
) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let thread = std::thread::spawn(move || {
        let started = Instant::now();
        for (choice, confidence) in choices {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && started.elapsed() < Duration::from_secs(15) =>
                    {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("mock Jev accept: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 4096];
            let body = loop {
                let n = stream.read(&mut buffer).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
                let Some(split) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&bytes[..split]);
                assert!(
                    headers
                        .to_ascii_lowercase()
                        .contains("authorization: bearer mock-dev-key")
                );
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap();
                if bytes.len() < split + 4 + length {
                    continue;
                }
                break bytes[split + 4..split + 4 + length].to_vec();
            };
            let request: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["model"], "jev-1.13.0");
            assert!(!String::from_utf8_lossy(&body).contains("mock-dev-key"));
            assert!(!String::from_utf8_lossy(&body).contains("fixture-sensitive"));
            let options = request["questions"]["action"]["criteria"]
                .as_object()
                .unwrap();
            assert!(options.contains_key(choice));
            if let Some(hook) = hook.take() {
                hook();
            }
            let other = if choice == "DONE" { "click c0" } else { "DONE" };
            let response=json!({"model":"jev-1.13.0","answers":{"action":{"type":"choice","choice":choice,"confidence":confidence,"probabilities":{choice:confidence,other:1.0-confidence}}}}).to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response).unwrap();
        }
    });
    (url, thread)
}
struct Fixture {
    repo: tempfile::TempDir,
    _data: tempfile::TempDir,
    _config: tempfile::TempDir,
    db: Store,
    task: String,
    worker: Value,
    step: template::Step,
    settings: Settings,
}
impl Fixture {
    fn new(base_url: &str, browser_mode: &str, budget: usize) -> Self {
        Self::new_with_project(base_url, browser_mode, budget, false)
    }

    fn new_with_project(
        base_url: &str,
        browser_mode: &str,
        budget: usize,
        separate_project: bool,
    ) -> Self {
        let repo = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let config_path = config.path().to_path_buf();
        // This integration binary has one test; the mock server never reads process environment.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &config_path);
        }
        let horde_config = config_path.join("horde");
        std::fs::create_dir_all(&horde_config).unwrap();
        std::fs::write(
            horde_config.join("config.toml"),
            if separate_project {
                "[decision]\nmode='disabled'\n".to_owned()
            } else {
                format!(
                    "[decision]\nmode='shadow'\nbrowser_test_mode='{browser_mode}'\nmax_decisions_per_task={budget}\nbackend='typesafe'\nbase_url='{base_url}'\napi_key_env='TYPESAFE_API_KEY'\n"
                )
            },
        )
        .unwrap();
        let credential = horde_config.join("credentials.env");
        std::fs::write(&credential, "TYPESAFE_API_KEY=mock-dev-key\n").unwrap();
        std::fs::set_permissions(&credential, std::fs::Permissions::from_mode(0o600)).unwrap();
        let workspace = repo.path();
        std::fs::create_dir_all(workspace.join(".horde")).unwrap();
        std::fs::create_dir_all(workspace.join("node_modules/playwright")).unwrap();
        std::fs::write(workspace.join("package.json"), "{}").unwrap();
        std::fs::write(
            workspace.join("node_modules/playwright/index.js"),
            FAKE_PLAYWRIGHT,
        )
        .unwrap();
        std::fs::write(workspace.join("server.py"),"import os,http.server,socketserver\nsocketserver.TCPServer(('127.0.0.1',int(os.environ['PORT'])),http.server.SimpleHTTPRequestHandler).serve_forever()\n").unwrap();
        std::fs::write(workspace.join(".horde/browser.json"),json!({"version":1,"objective":"save item","assertions":[{"kind":"text_visible","text":"Saved"}],"max_steps":4,"timeout_seconds":30}).to_string()).unwrap();
        git(workspace, &["init", "-q"]);
        git(workspace, &["add", "."]);
        git(
            workspace,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "commit",
                "-qm",
                "fixture",
            ],
        );
        let db = Store::open(&data.path().join("data")).unwrap();
        let project = separate_project.then(|| {
            let project = projects::dispatch(&db, "project_create", &json!({"slug":"browser"}))
                .unwrap()
                .unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned();
            let directory = projects::storage_root(&db, &project).unwrap();
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join("config.toml"),
                format!(
                    "[decision]\nmode='shadow'\nbrowser_test_mode='{browser_mode}'\nmax_decisions_per_task={budget}\nbackend='typesafe'\nbase_url='{base_url}'\napi_key_env='TYPESAFE_API_KEY'\n"
                ),
            )
            .unwrap();
            project
        });
        let settings = match project.as_deref() {
            Some(project) => Settings::load_project_user(&db, project).unwrap(),
            None => Settings::load_user().unwrap(),
        };
        let plan = template::compile(
            "simulated",
            &template::load_templates(workspace).unwrap(),
            BTreeMap::from([("task".into(), "browser test".into())]),
        )
        .unwrap();
        let task = match project.as_deref() {
            Some(project) => db
                .submit_project(project, "browser test", workspace, &settings, &plan)
                .unwrap(),
            None => db
                .submit("browser test", workspace, &settings, &plan)
                .unwrap(),
        };
        let worker = db.register(&task, None).unwrap();
        let step = serde_json::from_value(json!({"id":"app","kind":"environment"})).unwrap();
        Self {
            repo,
            _data: data,
            _config: config,
            db,
            task,
            worker,
            step,
            settings,
        }
    }
    async fn run(&self) -> anyhow::Result<Value> {
        let invocation = Invocation {
            db: &self.db,
            task: &self.task,
            step: "test",
            attempt: "test",
            worker: self.worker["id"].as_str().unwrap(),
            token: "test",
            workspace: self.repo.path(),
            spec: &self.step,
            settings: &self.settings,
            context: json!({}),
        };
        let environment = Environment {
            start: vec!["python3".into(), "server.py".into()],
            test: vec![
                "python3".into(),
                "-c".into(),
                "print('fallback ran')".into(),
            ],
            browser_test: Some(".horde/browser.json".into()),
            timeout_seconds: 30,
            readiness_seconds: 10,
            ..Default::default()
        };
        environment::execute(&invocation, &environment).await
    }
    fn hang(&self) {
        std::fs::write(self.repo.path().join("hang-browser"), "yes").unwrap();
        std::fs::write(self.repo.path().join(".horde/browser.json"),json!({"version":1,"objective":"save item","assertions":[{"kind":"text_visible","text":"Saved"}],"max_steps":4,"timeout_seconds":1}).to_string()).unwrap();
        git(self.repo.path(), &["add", "."]);
        git(
            self.repo.path(),
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "commit",
                "-qm",
                "hang fixture",
            ],
        );
    }
    fn bind_secret(&self) {
        let secret = "fixture-sensitive\"token";
        std::fs::write(
            self.repo.path().join("secret-label"),
            "fixture-sensitive\\\"token",
        )
        .unwrap();
        std::fs::write(self.repo.path().join(".horde/browser.json"), json!({"version":1,"objective":format!("save {secret}"),"assertions":[{"kind":"text_visible","text":"Saved"}],"max_steps":4,"timeout_seconds":30}).to_string()).unwrap();
        git(self.repo.path(), &["add", "."]);
        git(
            self.repo.path(),
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "commit",
                "-qm",
                "secret fixture",
            ],
        );
        let secret_dir = self.db.root.join("remote-secrets").join(&self.task);
        std::fs::create_dir_all(&secret_dir).unwrap();
        horde::secrets::write_private(
            &secret_dir.join(horde::store::hash(b"app")),
            json!({"version":"pinned","values":{"APP_SECRET":secret}})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        self.db
            .conn
            .execute(
                "INSERT INTO task_bundles VALUES(?,'app','pinned')",
                [&self.task],
            )
            .unwrap();
    }
    fn lose_ack(&self) {
        std::fs::write(self.repo.path().join("lose-ack"), "yes").unwrap();
        git(self.repo.path(), &["add", "."]);
        git(
            self.repo.path(),
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "commit",
                "-qm",
                "lost reply fixture",
            ],
        );
    }
}
#[tokio::test]
async fn browser_environment_success_false_done_and_fallback() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    for (choices, success, fallback, mode, budget) in [
        (
            vec![("click c0", 0.95), ("DONE", 0.95)],
            true,
            false,
            "active",
            64,
        ),
        (vec![("DONE", 0.95)], false, false, "active", 64),
        (vec![("click c0", 0.5)], true, true, "active", 64),
        (vec![("click c0", 0.95)], true, true, "active", 1),
        (vec![("click c0", 0.95)], true, true, "shadow", 64),
        (vec![], true, true, "disabled", 64),
    ] {
        let (url, server) = decision_server(choices);
        let f = Fixture::new(&url, mode, budget);
        if mode == "disabled" {
            std::fs::write(f.repo.path().join(".horde/browser.json"), "{not-json").unwrap();
            std::fs::write(f.repo.path().join("dirty-file"), "uncommitted").unwrap();
        }
        let result = f.run().await;
        let status = std::process::Command::new("git")
            .args(["status", "--porcelain=v1", "--untracked-files=all"])
            .current_dir(f.repo.path())
            .output()
            .unwrap();
        assert_eq!(
            result.is_ok(),
            success,
            "{result:?}; status={}",
            String::from_utf8_lossy(&status.stdout)
        );
        if fallback {
            assert!(
                result.as_ref().unwrap()["evidence"]["test"]["stdout"]
                    .as_str()
                    .unwrap()
                    .contains("fallback ran")
            );
        }
        let rows =
            f.db.rows("SELECT state,evidence FROM app_environments", &[])
                .unwrap();
        assert_eq!(rows[0]["state"], "removed");
        let evidence: Value = serde_json::from_str(rows[0]["evidence"].as_str().unwrap()).unwrap();
        if mode != "disabled" {
            assert!(evidence["browser"]["trace_hash"].is_string());
        }
        if !fallback {
            assert!(evidence["browser"]["screenshot_hash"].is_string());
        }
        assert!(evidence["tested_commit"].is_string());
        let decisions =
            f.db.rows(
                "SELECT state,error,purpose FROM decisions WHERE task=? ORDER BY seq",
                &[&f.task],
            )
            .unwrap();
        if mode == "disabled" {
            assert!(decisions.is_empty());
        } else if budget == 1 {
            assert_eq!(decisions.len(), 2);
            assert_eq!(decisions[0]["state"], "succeeded");
            assert_eq!(decisions[1]["error"], "task_decision_limit");
        } else {
            assert_eq!(
                decisions
                    .iter()
                    .filter(|row| row["state"] == "succeeded")
                    .count(),
                if success && !fallback { 2 } else { 1 }
            );
        }
        let listed = horde::decision::store::list(&f.db, &f.task, 0, 20).unwrap();
        if success && !fallback {
            assert!(listed.iter().all(|row| row["applied"] == 1
                && row["browser_action"]["state"] == "confirmed"
                && row["browser_action"]["trace_hash"].is_string()));
            let environment = result.as_ref().unwrap()["environment"].as_str().unwrap();
            let expected_hash = evidence["browser"]["trace_hash"].as_str().unwrap();
            f.db.conn.execute(
                "UPDATE browser_action_receipts SET trace_hash=NULL WHERE task=? AND environment=?",
                rusqlite::params![f.task, environment],
            ).unwrap();
            horde::browser::link_trace(
                &f.db,
                &f.task,
                environment,
                "different-attempt",
                "wrong-trace",
            )
            .unwrap();
            let traces =
                f.db.rows(
                    "SELECT trace_hash FROM browser_action_receipts WHERE task=? AND environment=?",
                    &[&f.task, &environment],
                )
                .unwrap();
            assert!(traces.iter().all(|row| row["trace_hash"].is_null()));
            horde::browser::link_trace(&f.db, &f.task, environment, "test", expected_hash).unwrap();
            let traces =
                f.db.rows(
                    "SELECT trace_hash FROM browser_action_receipts WHERE task=? AND environment=?",
                    &[&f.task, &environment],
                )
                .unwrap();
            assert!(traces.iter().all(|row| row["trace_hash"] == expected_hash));
        } else if mode == "shadow" {
            assert!(
                listed
                    .iter()
                    .all(|row| row["applied"] == 0 && row["browser_action"].is_null())
            );
        }
        server.join().unwrap();
    }
    // A hung Playwright driver must be killed with its process group before app cleanup.
    let (url, server) = decision_server(vec![]);
    let f = Fixture::new(&url, "active", 64);
    f.hang();
    let outcome = f.run().await;
    assert!(outcome.is_err());
    let rows =
        f.db.rows("SELECT state,evidence FROM app_environments", &[])
            .unwrap();
    assert_eq!(rows[0]["state"], "removed");
    let evidence: Value = serde_json::from_str(rows[0]["evidence"].as_str().unwrap()).unwrap();
    assert!(evidence["browser"]["trace_hash"].is_string());
    for group in
        f.db.rows("SELECT pid FROM app_process_groups", &[])
            .unwrap()
    {
        assert!(!horde::executor::process_alive(
            group["pid"].as_i64().unwrap() as i32
        ));
    }
    server.join().unwrap();
    let (url, server) = decision_server(vec![("click c0", 0.95), ("DONE", 0.95)]);
    let f = Fixture::new(&url, "active", 64);
    f.bind_secret();
    let result = f.run().await.unwrap();
    let evidence = &result["evidence"];
    assert!(evidence["browser"]["trace_hash"].is_string());
    assert!(evidence["browser"]["screenshot_hash"].is_null());
    let trace = std::fs::read_to_string(
        f.db.root
            .join("artifacts")
            .join(evidence["browser"]["trace_hash"].as_str().unwrap()),
    )
    .unwrap();
    assert!(!trace.contains("fixture-sensitive"));
    server.join().unwrap();
    let target: Arc<Mutex<Option<(PathBuf, String)>>> = Arc::new(Mutex::new(None));
    let for_server = Arc::clone(&target);
    let (url, server) = decision_server_with_hook(
        vec![("click c0", 0.95)],
        Some(Box::new(move || {
            let (root, task) = loop {
                if let Some(value) = for_server.lock().unwrap().clone() {
                    break value;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            let conn = rusqlite::Connection::open(root.join("state.sqlite3")).unwrap();
            conn.execute(
                "UPDATE task_tree SET version=version+1 WHERE task=?",
                [&task],
            )
            .unwrap();
        })),
    );
    let f = Fixture::new(&url, "active", 64);
    *target.lock().unwrap() = Some((f.db.root.clone(), f.task.clone()));
    let result = f.run().await.unwrap();
    assert!(
        result["evidence"]["test"]["browser_fallback_reason"]
            .as_str()
            .unwrap()
            .contains("context changed")
    );
    let version: i64 =
        f.db.conn
            .query_row(
                "SELECT version FROM task_tree WHERE task=?",
                [&f.task],
                |row| row.get(0),
            )
            .unwrap();
    let pinned: i64 =
        f.db.conn
            .query_row(
                "SELECT context_version FROM decisions WHERE task=?",
                [&f.task],
                |row| row.get(0),
            )
            .unwrap();
    assert!(version > pinned);
    server.join().unwrap();
    let target: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    let for_server = Arc::clone(&target);
    let (url, server) = decision_server_with_hook(
        vec![("click c0", 0.95)],
        Some(Box::new(move || {
            let repo = loop {
                if let Some(value) = for_server.lock().unwrap().clone() {
                    break value;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            std::fs::write(repo.join("package.json"), "{\"changed\":true}").unwrap();
            git(&repo, &["add", "package.json"]);
            git(
                &repo,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.test",
                    "commit",
                    "-qm",
                    "changed during browser decision",
                ],
            );
        })),
    );
    let f = Fixture::new(&url, "active", 64);
    *target.lock().unwrap() = Some(f.repo.path().to_path_buf());
    assert!(f.run().await.is_err());
    let listed = horde::decision::store::list(&f.db, &f.task, 0, 20).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["applied"], 0);
    assert!(listed[0]["browser_action"].is_null());
    server.join().unwrap();
    let (url, server) = decision_server(vec![("click c0", 0.95)]);
    let f = Fixture::new(&url, "active", 64);
    f.lose_ack();
    assert!(f.run().await.is_err());
    let listed = horde::decision::store::list(&f.db, &f.task, 0, 20).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["applied"], 0);
    assert_eq!(listed[0]["browser_action"]["state"], "pending");
    assert!(listed[0]["browser_action"]["trace_hash"].is_string());
    server.join().unwrap();

    // Project-owned browser decisions must work even when the default project
    // has disabled them. A pinned task must stop when its own project
    // authorization is removed, regardless of global settings.
    let (url, server) = decision_server(vec![("click c0", 0.95), ("DONE", 0.95)]);
    let f = Fixture::new_with_project(&url, "active", 64, true);
    assert_ne!(projects::task_project(&f.db, &f.task).unwrap(), "default");
    assert_eq!(
        Settings::load_user().unwrap().decision.mode,
        horde::config::DecisionMode::Disabled
    );
    let result = f.run().await.unwrap();
    assert!(result["evidence"]["browser"]["screenshot_hash"].is_string());
    server.join().unwrap();

    let (url, server) = decision_server(vec![]);
    let f = Fixture::new_with_project(&url, "active", 64, true);
    let project = projects::task_project(&f.db, &f.task).unwrap();
    std::fs::write(
        projects::storage_root(&f.db, &project)
            .unwrap()
            .join("config.toml"),
        "[decision]\nmode='disabled'\n",
    )
    .unwrap();
    std::fs::write(
        f._config.path().join("horde/config.toml"),
        format!("[decision]\nmode='shadow'\nbrowser_test_mode='active'\nbase_url='{url}'\napi_key_env='TYPESAFE_API_KEY'\n"),
    )
    .unwrap();
    let result = f.run().await.unwrap();
    assert!(
        result["evidence"]["test"]["stdout"]
            .as_str()
            .unwrap()
            .contains("fallback ran")
    );
    assert!(
        horde::decision::store::list(&f.db, &f.task, 0, 20)
            .unwrap()
            .is_empty()
    );
    server.join().unwrap();
}
