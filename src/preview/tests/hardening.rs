use super::*;
use std::time::Duration;

#[test]
fn first_enable_excludes_runs_completed_while_policy_was_disabled() {
    let (_dir, db, task, _review, mut config) = fixture();
    db.conn.execute("DELETE FROM preview_policies", []).unwrap();
    config["projects"][0]["enabled"] = json!(false);
    setup(&db, &config).unwrap();
    db.event(
        &task,
        "workflow.revised",
        json!({"source":"work while disabled"}),
    )
    .unwrap();
    config["projects"][0]["enabled"] = json!(true);
    setup(&db, &config).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(tokio::task::LocalSet::new().run_until(async {
        let mut queue = Queue::default();
        assert!(!queue.tick(&db).await.unwrap());
        assert_eq!(db.steps(&task).unwrap().len(), 2);
        queue.shutdown(&db).await.unwrap();
    }));
}

#[test]
fn controller_drain_api_responds_during_bounded_preview_validation() {
    drain_during_validation(
        "preview::tests::hardening::controller_drain_api_responds_during_bounded_preview_validation",
        Duration::ZERO,
        Duration::ZERO,
    );
}

#[test]
fn controller_drain_api_responds_with_delayed_handler_start() {
    drain_during_validation(
        "preview::tests::hardening::controller_drain_api_responds_with_delayed_handler_start",
        Duration::from_millis(750),
        Duration::ZERO,
    );
}

#[test]
fn controller_drain_wall_clock_guard_rejects_blocked_event_loop() {
    drain_during_validation(
        "preview::tests::hardening::controller_drain_wall_clock_guard_rejects_blocked_event_loop",
        Duration::ZERO,
        Duration::from_secs(3),
    );
}

fn drain_during_validation(test: &str, handler_delay: Duration, handler_stall: Duration) {
    const VALIDATION_TIMEOUT_SECONDS: u64 = 5;
    const DRAIN_RESPONSE_SECONDS: u64 = 2;
    if std::env::var_os("HORDE_PREVIEW_DRAIN_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("HORDE_PREVIEW_DRAIN_CHILD", "1")
            .env(
                "HORDE_RUN_ATTESTATION_KEY",
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [42; 32]),
            )
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if handler_stall.is_zero() {
            assert!(
                output.status.success(),
                "{} {stderr}",
                String::from_utf8_lossy(&output.stdout),
            );
        } else {
            assert!(
                !output.status.success()
                    && stderr.contains("drain response exceeded wall-clock limit"),
                "blocked event loop escaped the wall-clock guard: {} {stderr}",
                String::from_utf8_lossy(&output.stdout),
            );
        }
        return;
    }
    let (dir, db, task, review, mut config) = fixture();
    let script = dir.path().join("validation");
    let started = dir.path().join("validation-started");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\nsleep 30\n", started.display()),
    )
    .unwrap();
    config["projects"][0]["validation"] = json!(["sh", script]);
    config["projects"][0]["timeout_seconds"] = json!(VALIDATION_TIMEOUT_SECONDS);
    setup(&db, &config).unwrap();
    prepare_review(&db, &task, &review).unwrap();
    bind_attempt(&db, review["id"].as_str().unwrap(), "review-attempt").unwrap();
    fixture_execution(&db, &task, &review);
    pipeline::enqueue(&db, &task).unwrap().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(tokio::task::LocalSet::new().run_until(async {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let mut queue = Queue::default();
        assert!(queue.tick(&db).await.unwrap());
        // Git freshness checks happen before the validation process starts.
        // Synchronize on its marker rather than assuming fast runner startup.
        tokio::time::timeout(Duration::from_secs(10), async {
            while !started.exists() {
                let progress = status(&db, &task).unwrap();
                assert_ne!(progress["phase"], "held", "{progress}");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let (server, mut client) = tokio::net::UnixStream::pair().unwrap();
        let root = db.root.clone();
        let handler = tokio::task::spawn_local(async move {
            // Model a loaded runner scheduling the handler after the client.
            tokio::time::sleep(handler_delay).await;
            let result = crate::runtime::handle(server, root).await;
            // Negative control: the response is ready, but the single-thread
            // event loop cannot poll its reader or deadline during this stall.
            std::thread::sleep(handler_stall);
            result
        });
        let requested_at = std::time::Instant::now();
        client
            .write_all(b"{\"method\":\"runtime_drain\",\"args\":{}}\n")
            .await
            .unwrap();
        let mut line = String::new();
        tokio::time::timeout(
            Duration::from_secs(DRAIN_RESPONSE_SECONDS),
            tokio::io::BufReader::new(client).read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            requested_at.elapsed() <= Duration::from_secs(DRAIN_RESPONSE_SECONDS),
            "drain response exceeded wall-clock limit"
        );
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["result"]["draining"], true);
        assert_eq!(response["result"]["active"], 1);
        assert_eq!(status(&db, &task).unwrap()["phase"], "validating");
        handler.await.unwrap().unwrap();
        assert!(queue.tick(&db).await.unwrap());
        tokio::time::timeout(Duration::from_secs(8), async {
            while status(&db, &task).unwrap()["phase"] != "held" {
                tokio::time::sleep(Duration::from_millis(10)).await;
                queue.tick(&db).await.unwrap();
            }
        })
        .await
        .unwrap();
        let held = status(&db, &task).unwrap();
        assert_eq!(
            held["error"],
            format!(
                "executor timed out after {VALIDATION_TIMEOUT_SECONDS}s; process group stopped"
            )
        );
        assert_eq!(crate::management::status(&db).unwrap()["active"], 0);
        queue.shutdown(&db).await.unwrap();
    }));
}

#[test]
fn main_advancing_during_branch_push_holds_published_checkpoint() {
    main_advancing_during_push(
        "preview::tests::hardening::main_advancing_during_branch_push_holds_published_checkpoint",
        Duration::ZERO,
    );
}

#[test]
fn main_advancing_during_delayed_branch_push_holds_published_checkpoint() {
    main_advancing_during_push(
        "preview::tests::hardening::main_advancing_during_delayed_branch_push_holds_published_checkpoint",
        Duration::from_secs(8),
    );
}

fn main_advancing_during_push(test: &str, hook_delay: Duration) {
    if std::env::var_os("HORDE_PREVIEW_PUSH_RACE_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("HORDE_PREVIEW_PUSH_RACE_CHILD", "1")
            .env(
                "HORDE_RUN_ATTESTATION_KEY",
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [42; 32]),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (dir, db, task, review, mut config) = fixture();
    let (head, tree) = pipeline::identity(&db, &task).unwrap();
    let policy = configure(&config).unwrap().projects.remove(0);
    let receipt = json!({"schema_version":1,"scope":"local","project_id":policy.project_id,"project_slug":"hello","run_id":task,"built_commit":head,"tree_sha":tree,"recipe_hash":policy.recipe_hash(),"component":null,"artifact_digest":format!("sha256:{}","c".repeat(64)),"image":format!("registry:5000/local/hello@sha256:{}","c".repeat(64))});
    let publisher = dir.path().join("publisher");
    std::fs::write(&publisher, format!("#!/bin/sh\nrequest=$(cat)\ncase \"$request\" in *'\"mode\":\"probe\"'*) printf '%s' '{{\"reused\":true}}';; *) printf '%s' '{}' ;; esac\n", receipt)).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&publisher, std::fs::Permissions::from_mode(0o700)).unwrap();
    config["projects"][0]["publisher"] = json!(publisher);
    // The delayed hook must fit within a real publication deadline, while
    // remaining longer than the old fixed five-second polling window.
    config["projects"][0]["timeout_seconds"] = json!(15);
    setup(&db, &config).unwrap();
    prepare_review(&db, &task, &review).unwrap();
    bind_attempt(&db, review["id"].as_str().unwrap(), "review-attempt").unwrap();
    fixture_execution(&db, &task, &review);
    let job = pipeline::enqueue(&db, &task).unwrap().unwrap();
    let task_row = db.task(&task).unwrap();
    let repo = Path::new(task_row["repo"].as_str().unwrap());
    crate::git::run(
        repo,
        &[
            "commit",
            "--allow-empty",
            "-m",
            "advance during publication",
        ],
    )
    .unwrap();
    let advanced = crate::git::run(repo, &["rev-parse", "HEAD"]).unwrap();
    let remote = crate::git::run(repo, &["remote", "get-url", "origin"]).unwrap();
    crate::git::run(
        Path::new(&remote),
        &[
            "fetch",
            repo.to_str().unwrap(),
            "HEAD:refs/fixture/advanced",
        ],
    )
    .unwrap();
    let hook = Path::new(&remote).join("hooks/post-receive");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\ngit update-ref refs/heads/main {advanced}\nsleep {}\n",
            hook_delay.as_secs()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(tokio::task::LocalSet::new().run_until(async {
        let mut queue = Queue::default();
        assert!(queue.tick(&db).await.unwrap());
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let progress = status(&db, &task).unwrap();
            assert!(
                std::time::Instant::now() <= deadline,
                "preview did not settle before its fixture deadline: {progress}"
            );
            match progress["phase"].as_str() {
                Some("held") => break,
                Some("succeeded" | "superseded") => {
                    panic!("preview reached an unexpected terminal phase: {progress}");
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            queue.tick(&db).await.unwrap();
        }
        queue.shutdown(&db).await.unwrap();
    }));
    let report = status(&db, &task).unwrap();
    assert_eq!(report["phase"], "held");
    assert!(report["error"].as_str().unwrap().contains("main advanced"));
    assert_eq!(report["receipt"]["image"], receipt["image"]);
    assert_eq!(report["checkpoint"]["build_id"], job);
    assert_eq!(
        crate::git::run(Path::new(&remote), &["rev-parse", "refs/heads/main"]).unwrap(),
        advanced,
        "the controller must preserve the main advancement made by the fixture"
    );
    assert_eq!(
        crate::git::run(
            Path::new(&remote),
            &["rev-parse", &format!("refs/heads/horde/{task}")]
        )
        .unwrap(),
        head
    );
}

#[test]
fn activation_excludes_history_but_admits_explicit_new_work_and_preserves_cutoff() {
    let (_d, db, task, _row, mut c) = fixture();
    db.conn.execute("DELETE FROM preview_policies", []).unwrap();
    setup(&db, &c).unwrap();
    let project = p_project(&c);
    let cutoff: i64 = db
        .conn
        .query_row(
            "SELECT activation_seq FROM preview_policies WHERE project=?",
            [&project],
            |r| r.get(0),
        )
        .unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut q = Queue::default();
        assert!(!q.tick(&db).await.unwrap());
        assert_eq!(db.steps(&task).unwrap().len(), 2);
        q.shutdown(&db).await.unwrap();
    });
    db.event(
        &task,
        "workflow.revised",
        json!({"source":"explicit feedback"}),
    )
    .unwrap();
    c["projects"][0]["validation"] = json!(["true", "changed-policy"]);
    setup(&db, &c).unwrap();
    let after: i64 = db
        .conn
        .query_row(
            "SELECT activation_seq FROM preview_policies WHERE project=?",
            [&project],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cutoff, after);
    rt.block_on(async {
        let mut q = Queue::default();
        assert!(!q.tick(&db).await.unwrap());
        assert_eq!(db.steps(&task).unwrap().len(), 3);
        q.shutdown(&db).await.unwrap();
    });
}
#[test]
fn controller_checkpoint_deadline_does_not_block_event_loop() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let started = std::time::Instant::now();
        let work = crate::budget::blocking_timeout(std::time::Duration::from_millis(100), || {
            crate::budget::command_output(
                crate::executor::clean_command("sh").args(["-c", "sleep 10"]),
            )
        });
        let observation = async {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            assert!(started.elapsed() < std::time::Duration::from_secs(1));
        };
        let (result, _) = tokio::join!(work, observation);
        assert!(result.is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    });
}

#[test]
fn environment_backed_agent_cannot_attest_model_review() {
    let (_d, db, task, mut row, _c) = fixture();
    let mut spec = Store::step(&row).unwrap();
    spec.environment = Some(crate::environment::Environment::default());
    row["spec"] = json!(serde_json::to_string(&spec).unwrap());
    assert!(prepare_review(&db, &task, &row).is_err());
    let x = db
        .rows("SELECT * FROM preview_review_execution", &[])
        .unwrap();
    let w = db
        .rows("SELECT worker FROM attempts WHERE id='review-attempt'", &[])
        .unwrap();
    let settings =
        serde_json::from_str(db.task(&task).unwrap()["settings"].as_str().unwrap()).unwrap();
    assert!(
        verify_execution(
            &db,
            &row,
            "review-attempt",
            w[0]["worker"].as_str().unwrap(),
            &settings,
            "reviewer",
            Path::new(x[0]["workspace"].as_str().unwrap())
        )
        .is_err()
    );
}

#[test]
fn active_preview_review_excludes_concurrent_step_dispatch() {
    let (_d, db, task, row, _c) = fixture();
    db.conn
        .execute(
            "UPDATE steps SET state='running' WHERE id=?",
            [row["id"].as_str().unwrap()],
        )
        .unwrap();
    let running = db
        .rows(
            "SELECT spec,name FROM steps WHERE task=? AND state='running'",
            &[&task],
        )
        .unwrap();
    assert!(
        running
            .iter()
            .any(|r| crate::runtime::exclusive_step(&db, &task, r).unwrap())
    );
    let ordinary = db
        .rows(
            "SELECT * FROM steps WHERE task=? AND name='implement'",
            &[&task],
        )
        .unwrap();
    assert!(!crate::runtime::exclusive_step(&db, &task, &ordinary[0]).unwrap());
}

#[test]
fn chunked_admission_response_is_bounded_before_full_buffering() {
    let d = tempfile::tempdir().unwrap();
    let mut p = configure(&config()).unwrap().projects.remove(0);
    p.admission_token_file = d.path().join("token");
    std::fs::write(
        &p.admission_token_file,
        "scoped-purpose-token-0123456789abcdef",
    )
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    p.admission_url = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let received = stream.read(&mut [0; 4096]).unwrap();
        assert!(received > 0);
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let chunk = "x".repeat(32768);
        for _ in 0..3 {
            if write!(stream, "8000\r\n{chunk}\r\n").is_err() {
                break;
            }
        }
        let _ = write!(stream, "0\r\n\r\n");
    });
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(process::admission(
            &p,
            reqwest::Method::GET,
            "/v1/reservations/fixture",
            None,
        ));
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("receipt exceeds bound")
    );
    server.join().unwrap();
}
