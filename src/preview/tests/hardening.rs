use super::*;
use std::time::Duration;

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
        stream.read(&mut [0; 4096]).unwrap();
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
