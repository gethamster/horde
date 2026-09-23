use anyhow::{Result, anyhow, ensure};
use serde_json::Value;
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::AsyncReadExt;

const OUTPUT_LIMIT: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct SpendRequest {
    pub id: String,
    pub status: String,
    pub approval_url: Option<String>,
    pub token: Option<String>,
    amount: Option<u64>,
    currency: Option<String>,
    network_id: Option<String>,
    credential_type: Option<String>,
}

pub(super) async fn create(
    root: &Path,
    operation_id: &str,
    network_id: &str,
    charge_cents: u64,
    test_mode: bool,
) -> Result<SpendRequest> {
    create_for(
        root,
        operation_id,
        network_id,
        charge_cents,
        false,
        test_mode,
    )
    .await
}

pub(super) async fn create_topup(
    root: &Path,
    operation_id: &str,
    network_id: &str,
    charge_cents: u64,
    test_mode: bool,
) -> Result<SpendRequest> {
    create_for(
        root,
        operation_id,
        network_id,
        charge_cents,
        true,
        test_mode,
    )
    .await
}

async fn create_for(
    root: &Path,
    operation_id: &str,
    network_id: &str,
    charge_cents: u64,
    topup: bool,
    test_mode: bool,
) -> Result<SpendRequest> {
    ensure!(
        identifier(operation_id, "", 128),
        "invalid signup operation ID"
    );
    ensure!(
        identifier(network_id, "", 200),
        "invalid payment network ID"
    );
    ensure!(
        (1..=50_000).contains(&charge_cents),
        "Link wallet charge must be between 1 and 50000 cents"
    );
    let _lock = crate::provider_login::process::lock("link")?;
    let command = if topup {
        topup_command(
            command()?,
            operation_id,
            network_id,
            charge_cents,
            test_mode,
        )
    } else {
        create_command(
            command()?,
            operation_id,
            network_id,
            charge_cents,
            test_mode,
        )
    };
    let request = run(root, operation_id, command, TIMEOUT).await?;
    validate_binding(&request, network_id, charge_cents)?;
    Ok(request)
}

fn topup_command(
    mut command: tokio::process::Command,
    operation_id: &str,
    network_id: &str,
    charge_cents: u64,
    test_mode: bool,
) -> tokio::process::Command {
    command.args([
        "spend-request", "create", "--credential-type", "shared_payment_token",
        "--network-id", network_id, "--amount", &charge_cents.to_string(),
        "--currency", "usd", "--idempotency-key", operation_id,
        "--context", "Top up the existing Tuara organization used by Horde because its verified balance is below the operator's threshold. This payment is within the explicitly authorized per-charge and monthly automatic top-up limits.",
        "--format", "json",
    ]);
    if test_mode {
        command.arg("--test");
    }
    command
}

fn create_command(
    mut command: tokio::process::Command,
    operation_id: &str,
    network_id: &str,
    charge_cents: u64,
    test_mode: bool,
) -> tokio::process::Command {
    command.args([
        "spend-request", "create", "--credential-type", "shared_payment_token",
        "--network-id", network_id, "--amount", &charge_cents.to_string(),
        "--currency", "usd", "--idempotency-key", operation_id,
        "--context", "Fund a new Tuara organization for Horde inference with the explicitly approved initial credit and card funding fee. This authorizes one signup payment only, with no automatic top-ups.",
        "--format", "json",
    ]);
    if test_mode {
        command.arg("--test");
    }
    command
}

pub(super) async fn retrieve(
    root: &Path,
    operation_id: &str,
    spend_id: &str,
    network_id: &str,
    charge_cents: u64,
) -> Result<SpendRequest> {
    ensure!(
        identifier(operation_id, "", 128),
        "invalid signup operation ID"
    );
    ensure!(
        identifier(spend_id, "lsrq_", 128),
        "invalid Link spend request ID"
    );
    let _lock = crate::provider_login::process::lock("link")?;
    let mut command = command()?;
    command.args([
        "spend-request",
        "retrieve",
        spend_id,
        "--include",
        "shared_payment_token",
        "--format",
        "json",
    ]);
    let request = run(root, operation_id, command, TIMEOUT).await?;
    validate_binding(&request, network_id, charge_cents)?;
    ensure!(
        request.id == spend_id,
        "Link wallet returned a different spend request"
    );
    Ok(request)
}

/// Cancel an unconsumed Link authorization before Horde clears its local
/// pending record. The private response is parsed but never exposed.
pub(super) async fn cancel(root: &Path, operation_id: &str, spend_id: &str) -> Result<()> {
    ensure!(
        identifier(operation_id, "", 128),
        "invalid signup operation ID"
    );
    ensure!(
        identifier(spend_id, "lsrq_", 128),
        "invalid Link spend request ID"
    );
    let _lock = crate::provider_login::process::lock("link")?;
    let mut command = command()?;
    command.args(["spend-request", "cancel", spend_id, "--format", "json"]);
    let request = run(root, operation_id, command, TIMEOUT).await?;
    ensure!(
        request.id == spend_id,
        "Link wallet returned a different spend request"
    );
    ensure!(
        request.status == "canceled",
        "Link wallet did not cancel the spend request"
    );
    Ok(())
}

fn identifier(value: &str, prefix: &str, limit: usize) -> bool {
    value.len() > prefix.len()
        && value.len() <= limit
        && value.starts_with(prefix)
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn validate_binding(request: &SpendRequest, network_id: &str, charge_cents: u64) -> Result<()> {
    ensure!(
        request.amount == Some(charge_cents)
            && request.currency.as_deref() == Some("usd")
            && request.network_id.as_deref() == Some(network_id)
            && request.credential_type.as_deref() == Some("shared_payment_token"),
        "Link wallet authorization does not match the approved signup quote"
    );
    Ok(())
}

fn command() -> Result<tokio::process::Command> {
    let mut command = crate::provider_wallet::command()?;
    command
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .env("BROWSER", "/usr/bin/false");
    Ok(command)
}

fn parse(bytes: &[u8]) -> Result<SpendRequest> {
    let invalid = || anyhow!("Link wallet returned an invalid private response");
    ensure!(
        bytes.len() <= OUTPUT_LIMIT,
        "Link wallet response exceeded its private output limit"
    );
    let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    // Link CLI prints streaming commands (create, retrieve) as a one-row array and
    // single-result commands (cancel) as the object itself.
    let row = match &value {
        Value::Array(rows) if rows.len() == 1 => rows[0].as_object(),
        Value::Object(row) => Some(row),
        _ => None,
    }
    .ok_or_else(invalid)?;
    let id = row
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| identifier(id, "lsrq_", 128))
        .ok_or_else(invalid)?;
    let status = row
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    ensure!(
        matches!(
            status,
            "created"
                | "pending_approval"
                | "expired"
                | "approved"
                | "denied"
                | "submitted"
                | "succeeded"
                | "failed"
                | "canceled"
                | "requires_action"
        ),
        "Link wallet returned an unsupported spend status"
    );
    let approval_url = match row.get("approval_url").filter(|value| !value.is_null()) {
        Some(value) => {
            let text = value
                .as_str()
                .filter(|text| text.len() <= 2048 && !text.chars().any(char::is_control))
                .ok_or_else(invalid)?;
            let url = reqwest::Url::parse(text).map_err(|_| invalid())?;
            ensure!(
                url.scheme() == "https"
                    && matches!(url.host_str(), Some("app.link.com" | "link.com"))
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.port_or_known_default() == Some(443)
                    && url.fragment().is_none(),
                "Link wallet returned an invalid approval URL"
            );
            Some(text.to_owned())
        }
        None => None,
    };
    let token = match row
        .get("shared_payment_token")
        .filter(|value| !value.is_null())
    {
        Some(value) => {
            ensure!(
                status == "approved"
                    && row.get("credential_type").and_then(Value::as_str)
                        == Some("shared_payment_token"),
                "Link wallet returned an invalid payment credential"
            );
            Some(
                value
                    .as_str()
                    .or_else(|| value.get("id").and_then(Value::as_str))
                    .filter(|value| identifier(value, "spt_", 512))
                    .ok_or_else(invalid)?
                    .to_owned(),
            )
        }
        None => None,
    };
    Ok(SpendRequest {
        id: id.to_owned(),
        status: status.to_owned(),
        approval_url,
        token,
        amount: row.get("amount").and_then(Value::as_u64),
        currency: row
            .get("currency")
            .and_then(Value::as_str)
            .map(str::to_owned),
        network_id: row
            .get("network_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        credential_type: row
            .get("credential_type")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

async fn read_private(reader: impl tokio::io::AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| anyhow!("Link wallet private output could not be read"))?;
    ensure!(
        bytes.len() <= OUTPUT_LIMIT,
        "Link wallet response exceeded its private output limit"
    );
    Ok(bytes)
}

async fn run(
    root: &Path,
    operation_id: &str,
    mut command: tokio::process::Command,
    timeout: Duration,
) -> Result<SpendRequest> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = command.spawn().map_err(|_| {
        anyhow!("Link wallet could not start; install link-cli and authenticate its wallet first")
    })?;
    let pid = child
        .id()
        .ok_or_else(|| anyhow!("Link wallet process ID unavailable"))?;
    let guard = match crate::provider_login::process::Guard::record(
        root,
        &format!("signup-{operation_id}"),
        pid,
    ) {
        Ok(guard) => guard,
        Err(_) => {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            let _ = child.kill().await;
            return Err(anyhow!("Link wallet process could not be recorded safely"));
        }
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("Link wallet private output unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("Link wallet private output unavailable"))?;
    let result = tokio::time::timeout(timeout.min(TIMEOUT), async {
        let (bytes, _, status) =
            tokio::try_join!(read_private(stdout), read_private(stderr), async {
                child
                    .wait()
                    .await
                    .map_err(|_| anyhow!("Link wallet process failed"))
            })?;
        ensure!(
            status.success(),
            "Link wallet request failed; check wallet authentication and approval status"
        );
        parse(&bytes)
    })
    .await;
    guard.stop();
    let _ = child.wait().await;
    result.map_err(|_| {
        anyhow!(
            "Link wallet request timed out; inspect the existing signup operation before retrying"
        )
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn parses_only_private_spend_fields_from_link_json_arrays() {
        let value = json!([{
            "id": "lsrq_example_1", "status": "pending_approval",
            "approval_url": "https://app.link.com/approve/lsrq_example_1",
            "instruction": "untrusted instruction", "card": {"number": "secret"}
        }]);
        let parsed = parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(parsed.id, "lsrq_example_1");
        assert_eq!(parsed.status, "pending_approval");
        assert_eq!(
            parsed.approval_url.as_deref(),
            Some("https://app.link.com/approve/lsrq_example_1")
        );
        assert!(parsed.token.is_none());
    }

    #[test]
    fn parses_single_result_commands_that_print_one_object() {
        let value = json!({"id": "lsrq_example_1", "status": "canceled", "amount": 2048});
        let parsed = parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(parsed.id, "lsrq_example_1");
        assert_eq!(parsed.status, "canceled");
    }

    #[test]
    fn approved_spt_accepts_current_object_and_legacy_string() {
        for token in [json!({"id": "spt_private_123"}), json!("spt_private_123")] {
            let value = json!([{"id":"lsrq_example", "status":"approved", "credential_type":"shared_payment_token", "shared_payment_token":token}]);
            assert_eq!(
                parse(&serde_json::to_vec(&value).unwrap())
                    .unwrap()
                    .token
                    .as_deref(),
                Some("spt_private_123")
            );
        }
    }

    #[test]
    fn spend_authorization_must_match_the_pinned_amount_currency_and_recipient() {
        let base = json!({"id":"lsrq_example", "status":"approved", "credential_type":"shared_payment_token", "amount":2048, "currency":"usd", "network_id":"profile_expected"});
        let correct = parse(&serde_json::to_vec(&json!([base.clone()])).unwrap()).unwrap();
        validate_binding(&correct, "profile_expected", 2048).unwrap();
        for (field, value) in [
            ("amount", json!(2049)),
            ("currency", json!("eur")),
            ("network_id", json!("profile_other")),
            ("credential_type", json!("card")),
        ] {
            let mut changed = base.clone();
            changed[field] = value;
            let request = parse(&serde_json::to_vec(&json!([changed])).unwrap()).unwrap();
            assert!(
                validate_binding(&request, "profile_expected", 2048).is_err(),
                "accepted changed {field}"
            );
            let mut missing = base.clone();
            missing.as_object_mut().unwrap().remove(field);
            let request = parse(&serde_json::to_vec(&json!([missing])).unwrap()).unwrap();
            assert!(
                validate_binding(&request, "profile_expected", 2048).is_err(),
                "accepted missing {field}"
            );
        }
    }

    #[test]
    fn creation_pins_amount_currency_and_idempotency_without_payment_execution() {
        // Build from an explicit program. Setting the process-wide PATH to resolve a
        // fake CLI would hide tools such as ps from tests that spawn processes concurrently.
        let base = || crate::provider_wallet::command_for("/nonexistent/link-cli").unwrap();
        let command = create_command(base(), "signup-stable-id", "profile_recipient", 2048, false);
        let test_command = create_command(base(), "signup-test-id", "profile_recipient", 512, true);
        let test_topup_command =
            topup_command(base(), "topup-test-id", "profile_recipient", 512, true);
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert_eq!(&args[..2], &["spend-request", "create"]);
        for pair in [
            ["--amount", "2048"],
            ["--currency", "usd"],
            ["--idempotency-key", "signup-stable-id"],
            ["--network-id", "profile_recipient"],
            ["--credential-type", "shared_payment_token"],
            ["--format", "json"],
        ] {
            assert!(args.windows(2).any(|window| window == pair));
        }
        let context = args.iter().position(|arg| *arg == "--context").unwrap();
        assert!(args[context + 1].len() >= 100);
        assert!(!args.contains(&"--approve"));
        assert!(!args.contains(&"--test"));
        let test_args: Vec<_> = test_command
            .as_std()
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert!(test_args.contains(&"--test"));
        let test_topup_args: Vec<_> = test_topup_command
            .as_std()
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert!(test_topup_args.contains(&"--test"));
        for (name, _) in command.as_std().get_envs() {
            assert!(matches!(
                name.to_str().unwrap(),
                "PATH"
                    | "HOME"
                    | "USER"
                    | "LOGNAME"
                    | "TMPDIR"
                    | "LANG"
                    | "LC_ALL"
                    | "TERM"
                    | "SSH_AUTH_SOCK"
                    | "GIT_TERMINAL_PROMPT"
                    | "CI"
                    | "NO_COLOR"
                    | "NO_UPDATE_NOTIFIER"
                    | "BROWSER"
            ));
        }
    }

    #[tokio::test]
    async fn invalid_inputs_fail_before_starting_a_wallet_process() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            create(root.path(), "../escape", "profile_recipient", 2048, false)
                .await
                .is_err()
        );
        assert!(
            create(root.path(), "valid-id", "--network-override", 2048, false)
                .await
                .is_err()
        );
        assert!(
            create(root.path(), "valid-id", "profile_recipient", 50_001, false)
                .await
                .is_err()
        );
        assert!(
            retrieve(
                root.path(),
                "valid-id",
                "--other-option",
                "profile_recipient",
                2048
            )
            .await
            .is_err()
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn rejects_unsafe_or_ambiguous_output_without_echoing_it() {
        for value in [
            json!([]),
            json!([{"id":"lsrq_one", "status":"approved"},{"id":"lsrq_two", "status":"approved"}]),
            json!([{"id":"../../secret", "status":"approved"}]),
            json!([{"id":"lsrq_one", "status":"secret_unexpected"}]),
            json!([{"id":"lsrq_one", "status":"pending_approval", "approval_url":"https://app.link.com.evil.invalid/secret"}]),
            json!([{"id":"lsrq_one", "status":"pending_approval", "approval_url":"https://secret@app.link.com/approve"}]),
            json!([{"id":"lsrq_one", "status":"pending_approval", "approval_url":"https://app.link.com/\nsecret"}]),
            json!([{"id":"lsrq_one", "status":"approved", "credential_type":"card", "shared_payment_token":"spt_secret"}]),
        ] {
            let error = parse(&serde_json::to_vec(&value).unwrap())
                .err()
                .unwrap()
                .to_string();
            assert!(!error.contains("secret"), "{error}");
        }
        assert!(
            !parse(b"spt_secret malformed")
                .err()
                .unwrap()
                .to_string()
                .contains("secret")
        );
    }

    fn fake_command(directory: &Path, script: &str) -> tokio::process::Command {
        let executable = directory.join("link-cli");
        std::fs::write(&executable, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = tokio::process::Command::new("link-cli");
        command.env_clear().env("PATH", directory);
        command
    }

    #[tokio::test]
    async fn fake_cli_runs_privately_and_cleans_its_process_receipt() {
        let root = tempfile::tempdir().unwrap();
        let command = fake_command(
            root.path(),
            "printf '%s' '[{\"id\":\"lsrq_test\",\"status\":\"created\"}]'",
        );
        let result = run(
            root.path(),
            "test-operation",
            command,
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert_eq!(result.id, "lsrq_test");
        assert!(
            !root
                .path()
                .join("provider-logins/signup-test-operation.json")
                .exists()
        );
    }

    #[tokio::test]
    async fn oversized_or_failed_private_output_is_never_returned() {
        for (script, expected) in [
            ("printf spt_secret >&2; exit 1", "request failed"),
            (
                "printf spt_secret; i=0; while [ $i -lt 7000 ]; do printf 0123456789; i=$((i+1)); done",
                "output limit",
            ),
            (
                "printf spt_secret >&2; i=0; while [ $i -lt 7000 ]; do printf 0123456789 >&2; i=$((i+1)); done",
                "output limit",
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let command = fake_command(root.path(), script);
            let error = run(root.path(), "test-failure", command, Duration::from_secs(2))
                .await
                .err()
                .unwrap()
                .to_string();
            assert!(!error.contains("spt_secret"), "{error}");
            assert!(error.contains(expected), "{error}");
        }
    }

    #[tokio::test]
    async fn timeout_kills_the_cli_and_its_process_group() {
        let root = tempfile::tempdir().unwrap();
        let pid_file = root.path().join("pids");
        let script = format!(
            "/bin/sleep 30 &\nprintf '%s %s' $$ $! > '{}'\nwait",
            pid_file.display()
        );
        let command = fake_command(root.path(), &script);
        let error = run(
            root.path(),
            "test-timeout",
            command,
            Duration::from_millis(150),
        )
        .await
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("timed out"));
        let pids = std::fs::read_to_string(pid_file).unwrap();
        for (index, pid) in pids
            .split_whitespace()
            .map(|pid| pid.parse::<i32>().unwrap())
            .enumerate()
        {
            for _ in 0..30 {
                if !still_executing(pid, index != 0) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(!still_executing(pid, index != 0), "process {pid} survived");
        }
    }

    fn still_executing(pid: i32, _descendant: bool) -> bool {
        #[cfg(target_os = "linux")]
        if _descendant {
            // Container PID 1 may defer reaping orphaned grandchildren. A zombie
            // has terminated; the directly owned CLI must still be fully reaped.
            if std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
                stat.rsplit_once(") ")
                    .is_some_and(|(_, state)| state.starts_with("Z "))
            }) {
                return false;
            }
        }
        crate::executor::process_alive(pid)
    }
}
