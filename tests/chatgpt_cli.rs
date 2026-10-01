use std::process::Command;

#[test]
fn chatgpt_preset_uses_subscription_login_without_api_key() {
    let preset = horde::provisioning::preset("chatgpt").expect("ChatGPT preset");
    assert_eq!(preset.kind, "chatgpt");
    assert_eq!(preset.auth_mode, "login");
    assert_eq!(preset.base_url, "https://api.openai.com/v1");
    assert!(preset.api_key_env.is_empty());
}

#[test]
fn account_help_exposes_safe_chatgpt_lifecycle_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_horde"))
        .args(["account", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for command in [
        "login",
        "login-status",
        "login-cancel",
        "credential-import",
        "credential-export",
        "sign-out",
        "models",
    ] {
        assert!(help.contains(command), "missing {command}");
    }
}

#[test]
fn chatgpt_protocol_requires_account_for_secret_import_but_does_not_export_secrets() {
    let schema = horde::protocol::admin_schema("account_credential_import");
    assert_eq!(schema["properties"]["credential"]["type"], "object");
    assert!(
        schema["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("account"))
    );
    assert!(
        !horde::protocol::OPERATIONS
            .iter()
            .any(|(name, _)| name.contains("credential_export"))
    );
    assert!(!horde::protocol::worker_allowed(
        "account_credential_import"
    ));
    let login = horde::protocol::admin_schema("provider_login");
    assert_eq!(login["properties"]["account"]["type"], "string");
}

#[test]
fn malformed_stdin_credential_never_appears_in_error_output() {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_horde"))
        .args(["account", "credential-import", "example"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"SECRET_TEST_TOKEN_invalid_json")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("invalid credential bundle JSON"));
    assert!(!error.contains("SECRET_TEST_TOKEN"));
    assert!(output.stdout.is_empty());
}
