use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    os::unix::net::UnixListener,
    process::{Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

fn run(args: &[&str], responses: Vec<Value>) -> (Output, Vec<Value>) {
    run_with_input(args, responses, None)
}

fn run_with_input(
    args: &[&str],
    responses: Vec<Value>,
    input: Option<&[u8]>,
) -> (Output, Vec<Value>) {
    let root = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(root.path().join("daemon.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let finished = done.clone();
    let recorded = requests.clone();
    let worker = thread::spawn(move || {
        let mut responses = responses.into_iter();
        while !finished.load(Ordering::SeqCst) {
            let Ok((mut stream, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(2));
                continue;
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut line = String::new();
            std::io::BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            recorded
                .lock()
                .unwrap()
                .push(serde_json::from_str::<Value>(&line).unwrap());
            writeln!(
                stream,
                "{}",
                json!({"result":responses.next().expect("unexpected RPC")})
            )
            .unwrap();
        }
    });
    let mut command = Command::new(env!("CARGO_BIN_EXE_horde"));
    command
        .arg("--data-dir")
        .arg(root.path())
        .args(["config", "provider"])
        .args(args)
        .env_remove("HORDE_WORKER_TOKEN");
    let output = match input {
        Some(input) => terminal_output(command, input),
        None => command.stdin(Stdio::null()).output().unwrap(),
    };
    done.store(true, Ordering::SeqCst);
    worker.join().unwrap();
    let requests = requests.lock().unwrap().clone();
    (output, requests)
}

#[test]
fn noninteractive_signup_requires_explicit_budget_and_terms_without_contacting_daemon() {
    let (output, requests) = run(&["signup", "tuara"], vec![]);
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("--organization"), "{error}");
    assert!(error.contains("--accept-terms"), "{error}");
    assert!(requests.is_empty());
}

#[test]
fn signup_flags_use_exact_cents_and_complete_without_exposing_json_or_keys() {
    let (output, requests) = run(
        &[
            "signup",
            "tuara",
            "--organization",
            "Example Co",
            "--amount",
            "20",
            "--max-charge",
            "20.48",
            "--terms-version",
            "2026-09",
            "--accept-terms",
        ],
        vec![
            json!({"status":"awaiting_wallet"}),
            json!({"status":"awaiting_approval"}),
            json!({"status":"credential_received"}),
            json!({"status":"succeeded", "result":{"organization_id":"org_example", "raw_key":"DO_NOT_PRINT"}}),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("ready"), "{text}");
    assert!(!text.contains("DO_NOT_PRINT"));
    assert!(!text.contains("{\""));
    assert_eq!(requests[0]["method"], "provider_signup");
    assert_eq!(requests[0]["args"]["amount_cents"], 2000);
    assert_eq!(requests[0]["args"]["max_charge_cents"], 2048);
    assert_eq!(requests[0]["args"]["accept_terms"], true);
    assert_eq!(requests[0]["args"]["agent_name"], "horde");
    let id = requests[0]["args"]["request_id"].as_str().unwrap();
    assert!(!id.is_empty());
    for request in &requests[1..] {
        assert_eq!(request["args"]["request_id"], id);
        assert_eq!(request["args"]["action"], "resume");
    }
}

#[test]
fn resuming_uncertain_signup_only_reads_status_and_explains_reconciliation() {
    let (output, requests) = run(
        &["signup", "tuara", "--request-id", "existing-signup"],
        vec![json!({"status":"uncertain"})],
    );
    assert!(!output.status.success());
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["args"]["action"], "status");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("Reconcile"), "{text}");
}

#[test]
fn topup_flags_configure_recurring_caps_without_json_and_status_is_read_only() {
    let (output, requests) = run(
        &[
            "topup",
            "tuara",
            "--threshold",
            "5",
            "--amount",
            "20",
            "--max-charge",
            "20.48",
            "--monthly-limit",
            "100",
            "--terms-version",
            "2026-09",
            "--accept-terms",
        ],
        vec![json!({"enabled":true})],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(requests[0]["method"], "provider_topup");
    assert_eq!(requests[0]["args"]["action"], "configure");
    assert_eq!(requests[0]["args"]["threshold_cents"], 500);
    assert_eq!(requests[0]["args"]["monthly_limit_cents"], 10000);
    assert!(String::from_utf8_lossy(&output.stdout).contains("enabled"));
    let (output, requests) = run(
        &["topup", "tuara", "--status"],
        vec![json!({"enabled":false})],
    );
    assert!(output.status.success());
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["args"]["action"], "status");
}

#[test]
fn fractional_cents_are_rejected_before_any_funding_request() {
    let (output, requests) = run(
        &[
            "signup",
            "tuara",
            "--organization",
            "Example",
            "--amount",
            "20.005",
            "--max-charge",
            "20.48",
            "--terms-version",
            "2026-09",
            "--accept-terms",
        ],
        vec![],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("two decimal places"));
    assert!(requests.is_empty());
}

#[test]
fn wallet_action_can_be_rechecked_without_restarting_the_signup() {
    let (output, requests) = run(
        &["signup", "tuara", "--request-id", "existing-signup"],
        vec![
            json!({"status":"awaiting_approval", "wallet_action_required":true}),
            json!({"status":"credential_received"}),
            json!({"status":"succeeded"}),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1]["args"]["action"], "resume");
    assert!(String::from_utf8_lossy(&output.stdout).contains("ready"));
}

#[test]
fn topup_read_actions_reject_configuration_flags_and_missing_recurring_consent() {
    for args in [
        vec!["topup", "tuara", "--status", "--amount", "20"],
        vec![
            "topup",
            "tuara",
            "--threshold",
            "5",
            "--amount",
            "20",
            "--max-charge",
            "20.48",
            "--monthly-limit",
            "100",
            "--terms-version",
            "2026-09",
        ],
    ] {
        let (output, requests) = run(&args, vec![]);
        assert!(!output.status.success());
        assert!(requests.is_empty());
    }
}

#[test]
fn resumed_signup_uses_saved_provider_and_explains_key_verification_stalls() {
    let (output, requests) = run(
        &["signup", "tuara", "--request-id", "existing-signup"],
        vec![
            json!({"provider":"my-tuara", "status":"credential_received"}),
            json!({"provider":"my-tuara", "status":"credential_received", "message":"The saved signup key could not be verified. Resume without another payment."}),
        ],
    );
    assert!(output.status.success());
    assert_eq!(requests.len(), 2);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("signup my-tuara --request-id existing-signup"),
        "{text}"
    );
    assert!(text.contains("could not be verified"), "{text}");
    assert!(!text.contains("waiting for wallet setup"), "{text}");
}

#[test]
fn topup_policy_bounds_are_checked_before_rpc() {
    for (threshold, monthly) in [("500.01", "100"), ("5", "1000000.01")] {
        let (output, requests) = run(
            &[
                "topup",
                "tuara",
                "--threshold",
                threshold,
                "--amount",
                "20",
                "--max-charge",
                "20.48",
                "--monthly-limit",
                monthly,
                "--terms-version",
                "2026-09",
                "--accept-terms",
            ],
            vec![json!({"enabled":true})],
        );
        assert!(!output.status.success());
        assert!(requests.is_empty());
    }
}

#[test]
fn guided_signup_defaults_to_declining_terms_and_payment() {
    let (output, requests) =
        run_with_input(&["signup", "tuara"], vec![], Some(b"Example Co\n\n\n\n\n"));
    assert!(!output.status.success());
    let output = String::from_utf8_lossy(&output.stdout);
    assert!(output.contains("https://tuara.com/terms/"), "{output}");
    assert!(output.contains("$20.48"), "{output}");
    assert!(
        output.contains("Signup cancelled before any account or payment request"),
        "{output}"
    );
    assert!(requests.is_empty());
}

#[test]
fn guided_signup_can_finish_and_offer_explicit_recurring_topup_consent() {
    let input = [
        "Example Co",
        "",
        "",
        "",
        "yes",
        "yes",
        "",
        "",
        "",
        "",
        "",
        "yes",
        "",
    ]
    .join("\n");
    let (output, requests) = run_with_input(
        &["signup", "tuara"],
        vec![
            json!({"provider":"default", "status":"awaiting_wallet"}),
            json!({"provider":"default", "status":"awaiting_approval"}),
            json!({"provider":"default", "status":"credential_received"}),
            json!({"provider":"default", "status":"succeeded"}),
            json!({"enabled":true, "status":"watching"}),
        ],
        Some(input.as_bytes()),
    );
    let output_text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{output_text}");
    assert!(
        output_text.contains("Set up automatic Tuara top-ups now?"),
        "{output_text}"
    );
    assert!(
        output_text.contains("Maximum recurring charges per UTC calendar month"),
        "{output_text}"
    );
    assert!(
        output_text.contains("$100.00 per UTC calendar month including fees"),
        "{output_text}"
    );
    assert!(!output_text.contains("{\""), "{output_text}");
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[0]["args"]["accept_terms"], true);
    assert_eq!(requests[4]["method"], "provider_topup");
    assert_eq!(requests[4]["args"]["provider"], "default");
    assert_eq!(requests[4]["args"]["monthly_limit_cents"], 10000);
    assert_eq!(requests[4]["args"]["accept_terms"], true);
}

fn terminal_output(mut command: Command, input: &[u8]) -> Output {
    use std::{
        fs::File,
        io::{ErrorKind, Read},
        os::fd::{AsRawFd, FromRawFd},
        time::Instant,
    };
    let mut master = -1;
    let mut slave = -1;
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let mut master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    let mut child = command
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave.try_clone().unwrap())
        .spawn()
        .unwrap();
    master.write_all(input).unwrap();
    unsafe {
        libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut output = Vec::new();
    let mut buffer = [0; 4096];
    let mut status = None;
    loop {
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => output.extend_from_slice(&buffer[..count]),
                Err(error)
                    if error.kind() == ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(libc::EIO) =>
                {
                    break;
                }
                Err(error) => panic!("PTY read failed: {error}"),
            }
        }
        if status.is_some() {
            break;
        }
        status = child.try_wait().unwrap();
        if Instant::now() >= deadline && status.is_none() {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!(
                "guided funding timed out: {}",
                String::from_utf8_lossy(&output)
            );
        }
        thread::sleep(Duration::from_millis(5));
    }
    let status = status.unwrap();
    Output {
        status,
        stdout: output,
        stderr: Vec::new(),
    }
}
