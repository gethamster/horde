use horde::{
    config::{Provider, Settings},
    protocol,
};
use serde_json::json;

fn settings(endpoint: &str, mode: &str) -> anyhow::Result<Settings> {
    let mut settings = Settings::default();
    settings.providers.insert(
        "chatgpt".into(),
        Provider {
            kind: "chatgpt".into(),
            auth_mode: mode.into(),
            base_url: endpoint.into(),
            ..Default::default()
        },
    );
    let directory = tempfile::tempdir()?;
    std::fs::write(
        directory.path().join("config.toml"),
        toml::to_string(&settings)?,
    )?;
    Settings::load_dir(directory.path())
}

#[test]
fn chatgpt_credentials_cannot_be_sent_to_arbitrary_endpoints_or_api_mode() {
    assert!(settings("https://api.openai.com/v1", "login").is_ok());
    assert!(settings("https://example.com/v1", "login").is_err());
    assert!(settings("https://api.openai.com/v1", "api").is_err());
}

#[test]
fn project_bound_connections_cannot_import_or_sign_out_host_credentials() {
    assert!(!protocol::project_allowed("account_credential_import"));
    assert!(!protocol::project_allowed("account_sign_out"));
    assert!(protocol::project_allowed("account_models"));
}

#[test]
fn refresh_material_never_enters_worker_provisioning() {
    let directory = tempfile::tempdir().unwrap();
    let db = horde::store::Store::open(directory.path()).unwrap();
    let account = horde::accounts::dispatch(&db, "account_create", &json!({"name":"subscription","provider":"chatgpt","auth_mode":"login","base_url":"https://api.openai.com/v1"})).unwrap().unwrap()["id"].as_str().unwrap().to_owned();
    let registration = json!({"issuer":"https://auth.openai.com","subject":"synthetic-subject","email":null,"client_id":"oaiapp_test","ext_agent_host_id":"urn:uuid:00000000-0000-4000-8000-000000000001","access_token":"synthetic-access","refresh_token":"synthetic-refresh","id_token":"synthetic-id","token_type":"Bearer","scopes":["resource.invoke","chatgpt.tokens.use.direct"],"expires_at":horde::store::now()+3600});
    let credential = horde::accounts::Credential {
        kind: "chatgpt_oauth".into(),
        secret: registration.to_string(),
        expires_at: None,
        metadata: json!({"plan_usage_enabled":true}),
    };
    horde::accounts::set_credential(&db, "default", &account, &credential).unwrap();
    let mut envelope =
        horde::accounts::provision(&db, "default", &account, "local", "test-delivery").unwrap();
    assert!(envelope.credential.is_none());
    let encoded = serde_json::to_string(&envelope).unwrap();
    assert!(!encoded.contains("synthetic-refresh"));
    assert!(!encoded.contains("synthetic-access"));
    let runtime_directory = tempfile::tempdir().unwrap();
    let runtime = horde::store::Store::open(runtime_directory.path()).unwrap();
    horde::accounts::receive(&runtime, &envelope).unwrap();
    let inspected =
        horde::accounts::dispatch(&runtime, "account_inspect", &json!({"account":account}))
            .unwrap()
            .unwrap();
    assert_eq!(inspected["credential_material"], "not_local");
    assert!(inspected["plan_usage_enabled"].is_null());
    envelope.credential = Some(credential);
    assert!(horde::accounts::receive(&runtime, &envelope).is_err());
}

#[test]
fn usage_limit_holds_work_without_fabricating_usage_or_reset() {
    let directory = tempfile::tempdir().unwrap();
    let db = horde::store::Store::open(directory.path()).unwrap();
    let config = horde::config::ExecutorConfig {
        kind: "chatgpt".into(),
        auth_mode: "login".into(),
        ..Default::default()
    };
    horde::capacity::hold_chatgpt_usage(&db, &config, None).unwrap();
    assert!(!horde::capacity::available(&db, "chatgpt:login").unwrap());
    assert!(
        horde::capacity::chatgpt_usage_hold(&db, "chatgpt:login")
            .unwrap()
            .is_some()
    );
    assert!(
        db.rows("SELECT * FROM account_capacity", &[])
            .unwrap()
            .is_empty()
    );
}
