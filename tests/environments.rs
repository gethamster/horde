use horde::{
    config::Settings,
    environment::{self, Environment},
    executor::Invocation,
    store::Store,
    template,
};
use serde_json::json;
use std::collections::BTreeMap;
struct Fixture {
    dir: tempfile::TempDir,
    db: Store,
    oid: String,
    step: template::Step,
    settings: Settings,
    worker: serde_json::Value,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // HTTPServer performs reverse DNS during bind, which can stall on CI.
        // TCPServer serves the same handler without external name resolution.
        std::fs::write(dir.path().join("server.py"),"import os,http.server,socketserver\nprint(os.getenv('APP_SECRET',''),flush=True)\nsocketserver.TCPServer(('127.0.0.1',int(os.environ['PORT'])),http.server.SimpleHTTPRequestHandler).serve_forever()\n").unwrap();
        let db = Store::open(&dir.path().join("data")).unwrap();
        let settings = Settings::default();
        let plan = template::compile(
            "simulated",
            &template::load_templates(dir.path()).unwrap(),
            BTreeMap::from([("task".into(), "app test".into())]),
        )
        .unwrap();
        let oid = db.submit("app test", dir.path(), &settings, &plan).unwrap();
        let worker = db.register(&oid, None).unwrap();
        let step = serde_json::from_value(json!({"id":"app","kind":"environment"})).unwrap();
        Self {
            dir,
            db,
            oid,
            step,
            settings,
            worker,
        }
    }
    async fn run(&self, spec: &Environment) -> anyhow::Result<serde_json::Value> {
        let i = Invocation {
            db: &self.db,
            task: &self.oid,
            step: "test",
            attempt: "test",
            worker: self.worker["id"].as_str().unwrap(),
            token: "test",
            workspace: self.dir.path(),
            spec: &self.step,
            settings: &self.settings,
            context: json!({}),
        };
        environment::execute(&i, spec).await
    }
}
fn process() -> Environment {
    Environment{start:vec!["python3".into(),"server.py".into()],test:vec!["python3".into(),"-c".into(),"import os,urllib.request; assert urllib.request.urlopen(os.environ['HORDE_APP_URL']).status==200".into()],timeout_seconds:60,readiness_seconds:30,..Default::default()}
}
#[tokio::test]
async fn process_environment_injects_secrets_tests_and_removes_resources() {
    let f = Fixture::new();
    let dir = f.db.root.join("remote-secrets").join(&f.oid);
    std::fs::create_dir_all(&dir).unwrap();
    horde::secrets::write_private(
        &dir.join(horde::store::hash(b"app")),
        json!({"version":"pinned","values":{"APP_SECRET":"fixture-sensitive-token"}})
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    f.db.conn
        .execute(
            "INSERT INTO task_bundles VALUES(?,'app','pinned')",
            [&f.oid],
        )
        .unwrap();
    let result = f.run(&process()).await.unwrap_or_else(|error| {
        panic!(
            "{error:#}; environments: {:?}",
            f.db.rows("SELECT state,evidence FROM app_environments", &[])
                .unwrap()
        )
    });
    assert_eq!(result["accepted"], true);
    assert!(!result.to_string().contains("fixture-sensitive-token"));
    let rows = f.db.rows("SELECT * FROM app_environments", &[]).unwrap();
    assert_eq!(rows[0]["state"], "removed");
    assert!(
        !rows[0]["evidence"]
            .as_str()
            .unwrap()
            .contains("fixture-sensitive-token")
    );
    assert!(
        !f.db
            .root
            .join("environment-private")
            .join(rows[0]["id"].as_str().unwrap())
            .exists()
    );
}
#[tokio::test]
// The allocated port is released just before the app starts, so a busy machine
// can take it in between. That is not a defect in the app, and the error must
// not read like one. Hand the port to a process that outlives the app to make
// the race deterministic.
async fn a_port_taken_before_startup_is_reported_as_a_race_not_an_app_failure() {
    let f = Fixture::new();
    let mut spec = process();
    spec.start = vec![
        "python3".into(),
        "-c".into(),
        // Bind, fork, let the parent exit: the child keeps the socket, so the
        // app has exited while something else holds its port.
        "import socket,os,sys,time\n\
         s=socket.socket()\n\
         s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)\n\
         s.bind(('127.0.0.1',int(os.environ['PORT'])))\n\
         s.listen()\n\
         if os.fork()>0: sys.exit(1)\n\
         time.sleep(30)\n"
            .into(),
    ];
    let error = f.run(&spec).await.unwrap_err().to_string();
    assert!(
        error.contains("taken by another process"),
        "expected a port-race diagnosis, got: {error}"
    );
}

#[tokio::test]
async fn failed_readiness_and_tests_still_remove_the_process() {
    let f = Fixture::new();
    let mut spec = process();
    spec.start = vec!["python3".into(), "-c".into(), "raise SystemExit(2)".into()];
    assert!(f.run(&spec).await.is_err());
    let mut spec = process();
    spec.test = vec!["false".into()];
    assert!(f.run(&spec).await.is_err());
    assert!(
        f.db.rows("SELECT state FROM app_environments", &[])
            .unwrap()
            .iter()
            .all(|r| r["state"] == "removed")
    );
}
#[tokio::test]
async fn lifetime_timeout_cleans_up_and_preserves_unrelated_process() {
    let f = Fixture::new();
    let mut unrelated = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let mut spec = process();
    spec.timeout_seconds = 1;
    spec.readiness_seconds = 1;
    spec.test = vec!["sleep".into(), "30".into()];
    assert!(f.run(&spec).await.is_err());
    assert!(unrelated.try_wait().unwrap().is_none());
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
    assert_eq!(
        f.db.rows("SELECT state FROM app_environments", &[])
            .unwrap()[0]["state"],
        "removed"
    );
}
#[test]
fn compose_rejects_shared_writes_and_fixed_identity() {
    let spec = Environment::default();
    for config in [
        json!({"services":{"app":{"container_name":"shared"}}}),
        json!({"services":{"app":{"network_mode":"host"}}}),
        json!({"services":{"app":{"volumes":[{"type":"bind","read_only":false}]}}}),
        json!({"services":{"app":{}},"volumes":{"db":{"external":true}}}),
    ] {
        assert!(environment::validate_compose(&config, &spec).is_err());
    }
    environment::validate_compose(
        &json!({"services":{"app":{"volumes":[{"type":"bind","read_only":true}]}}}),
        &spec,
    )
    .unwrap();
}
#[tokio::test]
#[ignore = "requires Docker Engine; uses only a uniquely named disposable Compose project"]
async fn real_compose_stack_is_ready_tested_and_torn_down() {
    let f = Fixture::new();
    std::fs::write(f.dir.path().join("compose.yaml"),r#"services:
  app:
    image: node:24-bookworm-slim
    command: ['node','-e',"require('fs').writeFileSync('/srv/index.html','ready'); require('http').createServer((q,r)=>r.end('ready')).listen(8080,'0.0.0.0')"]
    ports: ['18080:8080']
    volumes: ['scratch:/srv']
    healthcheck:
      test: ['CMD','test','-f','/srv/index.html']
      interval: 1s
      timeout: 1s
      retries: 10
volumes:
  scratch: {}
"#).unwrap();
    let compose = f.dir.path().join("compose.yaml");
    if let Ok(image) = std::env::var("HORDE_TEST_IMAGE") {
        let text = std::fs::read_to_string(&compose)
            .unwrap()
            .replace("node:24-bookworm-slim", &image);
        std::fs::write(&compose, text).unwrap();
    }
    let context = std::env::var("HORDE_TEST_DOCKER_CONTEXT").ok();
    let test = if let Some(c) = &context {
        format!(
            "docker --context {c} compose -p \"$HORDE_COMPOSE_PROJECT\" -f \"$HORDE_COMPOSE_FILE\" exec -T app test -f /srv/index.html"
        )
    } else {
        "docker compose -p \"$HORDE_COMPOSE_PROJECT\" -f \"$HORDE_COMPOSE_FILE\" exec -T app test -f /srv/index.html".into()
    };
    let spec = Environment {
        runner: "compose".into(),
        docker_context: context.clone(),
        test: vec![
            "sh".into(),
            "-c".into(),
            format!(
                r#"{test} && python3 -c 'import os,urllib.request; assert urllib.request.urlopen(os.environ["HORDE_APP_URL"]).read().strip()==b"ready"'"#
            ),
        ],
        timeout_seconds: 120,
        readiness_seconds: 90,
        ..Default::default()
    };
    let result = f.run(&spec).await.unwrap();
    assert_eq!(result["accepted"], true);
    let eid = result["environment"].as_str().unwrap();
    let mut command = std::process::Command::new("docker");
    if let Some(c) = context {
        command.args(["--context", &c]);
    }
    let output = command
        .args([
            "ps",
            "-aq",
            "--filter",
            &format!("label=com.docker.compose.project={eid}"),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    for resource in ["volume", "network"] {
        let mut command = std::process::Command::new("docker");
        if let Some(context) = &spec.docker_context {
            command.args(["--context", context]);
        }
        let output = command
            .args([
                resource,
                "ls",
                "-q",
                "--filter",
                &format!("label=com.docker.compose.project={eid}"),
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(
            output.stdout.is_empty(),
            "owned {resource} remains after teardown"
        );
    }
}

#[test]
fn daemon_restart_reaps_owned_app_and_test_process_groups() {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn wait(mut check: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !check() {
            assert!(
                Instant::now() < deadline,
                "restart reconciliation timed out"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let f = Fixture::new();
    let templates = f.dir.path().join(".horde/templates");
    std::fs::create_dir_all(&templates).unwrap();
    std::fs::write(templates.join("crash.toml"),"name='crash'\nversion='1.0.0'\ninputs=['task']\n[[steps]]\nid='app'\nkind='environment'\n[steps.environment]\nstart=['python3','server.py']\ntest=['sleep','300']\ntimeout_seconds=600\n").unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.name", "Test"],
        vec!["config", "user.email", "test@localhost"],
        vec!["add", "server.py", ".horde"],
        vec!["commit", "-m", "fixture"],
    ] {
        horde::git::run(f.dir.path(), &args).unwrap();
    }
    let user = f.dir.path().join("user");
    std::fs::create_dir_all(user.join("horde")).unwrap();
    let bundle = f.dir.path().join("fixture.env");
    horde::secrets::write_private(&bundle, b"APP_SECRET=restart-secret-fixture\n").unwrap();
    std::fs::write(
        user.join("horde/secrets.toml"),
        format!("[bundles]\napp={bundle:?}\n"),
    )
    .unwrap();
    std::fs::write(user.join("horde/config.toml"), "secret_bundles=['app']\n").unwrap();
    // Keep the fixture's initial simulated task from racing with this step.
    f.db.conn
        .execute("UPDATE tasks SET status='paused'", [])
        .unwrap();
    let spawn = || {
        ChildGuard(
            Command::new(env!("CARGO_BIN_EXE_horde"))
                .arg("--data-dir")
                .arg(&f.db.root)
                .arg("daemon")
                .env("XDG_CONFIG_HOME", &user)
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        )
    };
    let mut daemon = spawn();
    wait(|| f.db.root.join("daemon.sock").exists());
    let submit = Command::new(env!("CARGO_BIN_EXE_horde"))
        .arg("--data-dir")
        .arg(&f.db.root)
        .args(["submit", "Restart app", "--repo"])
        .arg(f.dir.path())
        .args(["--template", "crash"])
        .env("XDG_CONFIG_HOME", &user)
        .output()
        .unwrap();
    assert!(
        submit.status.success(),
        "{}",
        String::from_utf8_lossy(&submit.stderr)
    );
    wait(|| {
        !f.db
            .rows("SELECT pid FROM app_process_groups", &[])
            .unwrap()
            .is_empty()
    });
    let row = f.db.rows("SELECT * FROM app_environments", &[]).unwrap()[0].clone();
    let test_pid =
        f.db.rows("SELECT pid FROM app_process_groups", &[])
            .unwrap()[0]["pid"]
            .as_i64()
            .unwrap() as i32;
    let app_pid = row["pid"].as_i64().unwrap() as i32;
    let envfile = std::path::Path::new(row["workspace"].as_str().unwrap()).join(".env");
    assert!(envfile.exists());
    let mut unrelated = ChildGuard(Command::new("sleep").arg("300").spawn().unwrap());
    daemon.0.kill().unwrap();
    daemon.0.wait().unwrap();
    let _restarted = spawn();
    let deadline = Instant::now() + Duration::from_secs(60);
    while f
        .db
        .rows("SELECT state FROM app_environments", &[])
        .unwrap()[0]["state"]
        != "removed"
    {
        assert!(
            Instant::now() < deadline,
            "cleanup rows {:?}; groups {:?}; app ps {:?}; test ps {:?}",
            f.db.rows("SELECT id,state,pid FROM app_environments", &[])
                .unwrap(),
            f.db.rows("SELECT * FROM app_process_groups", &[]).unwrap(),
            Command::new("ps")
                .args([
                    "-p",
                    &app_pid.to_string(),
                    "-o",
                    "lstart=,pgid=,comm=,stat="
                ])
                .output()
                .unwrap(),
            Command::new("ps")
                .args([
                    "-p",
                    &test_pid.to_string(),
                    "-o",
                    "lstart=,pgid=,comm=,stat="
                ])
                .output()
                .unwrap()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!horde::executor::process_alive(app_pid));
    assert!(!horde::executor::process_alive(test_pid));
    assert!(!envfile.exists());
    assert!(unrelated.0.try_wait().unwrap().is_none());
    assert_eq!(
        f.db.rows("SELECT state FROM attempts", &[]).unwrap()[0]["state"],
        "uncertain"
    );
}

#[tokio::test]
async fn recovery_holds_identity_mismatch_without_killing_an_unrelated_process() {
    let f = Fixture::new();
    let mut other = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let eid = "held-fixture";
    f.db.conn.execute("INSERT INTO app_environments(id,task,kind,state,spec,workspace,pid,created,expires) VALUES(?,?,'process','starting',?,?,?,0,1)",rusqlite::params![eid,f.oid,serde_json::to_string(&process()).unwrap(),f.dir.path().to_str(),other.id()]).unwrap();
    f.db.conn
        .execute(
            "INSERT INTO app_process_identity VALUES(?,'different-process-start')",
            [eid],
        )
        .unwrap();
    environment::reconcile(&f.db).await.unwrap();
    assert!(other.try_wait().unwrap().is_none());
    assert_eq!(
        f.db.rows("SELECT state FROM app_environments", &[])
            .unwrap()[0]["state"],
        "held"
    );
    other.kill().unwrap();
    other.wait().unwrap();
    environment::cleanup_pending(&f.db).await.unwrap();
    assert_eq!(
        f.db.rows("SELECT state FROM app_environments", &[])
            .unwrap()[0]["state"],
        "removed"
    );
}

#[tokio::test]
async fn recovery_stops_app_descendants_after_the_group_leader_exits() {
    use std::os::unix::process::CommandExt;
    let f = Fixture::new();
    let mut command = std::process::Command::new("python3");
    command
        .args([
            "-c",
            "import os,time; pid=os.fork(); print(pid,flush=True) if pid else time.sleep(30)",
        ])
        .stdout(std::process::Stdio::piped());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut leader = command.spawn().unwrap();
    let pid = leader.id();
    use std::io::BufRead;
    let mut line = String::new();
    std::io::BufReader::new(leader.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let descendant: i32 = line.trim().parse().unwrap();
    struct Cleanup(i32);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            unsafe {
                libc::kill(-self.0, libc::SIGKILL);
            }
        }
    }
    let _cleanup = Cleanup(pid as i32);
    leader.wait().unwrap();
    assert!(horde::executor::process_alive(descendant));
    f.db.conn.execute("INSERT INTO app_environments(id,task,kind,state,spec,workspace,pid,created,expires) VALUES('orphan-app',?,'process','starting',?,?,?,0,1)", rusqlite::params![f.oid,serde_json::to_string(&process()).unwrap(),f.dir.path().to_str(),pid]).unwrap();
    environment::reconcile(&f.db).await.unwrap();
    for _ in 0..100 {
        if !horde::executor::process_alive(descendant) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(!horde::executor::process_alive(descendant));
    assert_eq!(
        f.db.rows("SELECT state FROM app_environments", &[])
            .unwrap()[0]["state"],
        "removed"
    );
}
