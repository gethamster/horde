//! Agent-operated setup with credential updates and explicit readiness checks.
use crate::{
    config::Settings,
    daemon_client,
    fleet_enrollment::cli::{self, KeyCommands},
    network::{NetworkConfig, Provider},
    provisioning,
    store::Store,
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Action {
    Inspect,
    Verify,
    ConfigureProvider,
    ConfigureWorkers,
    ConfigureController,
    CreateFleetKey,
    JoinWorker,
    RestartLocal,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    action: Action,
    provider: Option<String>,
    kind: Option<String>,
    auth_mode: Option<String>,
    base_url: Option<String>,
    api_key_env: Option<String>,
    program: Option<String>,
    #[serde(default)]
    roles: Vec<String>,
    model: Option<String>,
    credential: Option<String>,
    credential_env: Option<String>,
    credential_file: Option<PathBuf>,
    repo: Option<PathBuf>,
    invitation_file: Option<PathBuf>,
    output_file: Option<PathBuf>,
    name: Option<String>,
    max_workers: Option<usize>,
    expires_in: Option<i64>,
    concurrency: Option<usize>,
    #[serde(default)]
    no_start: bool,
    #[serde(default)]
    explicit_root: bool,
}

pub fn dispatch(db: &Store, args: &Value) -> Result<Value> {
    let root = db.root.clone();
    let args = args.clone();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(run(&root, &args))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("agent setup worker failed"))?
}

pub async fn run(root: &Path, args: &Value) -> Result<Value> {
    crate::fleet_enrollment::admin()?;
    let request: Request = serde_json::from_value(args.clone())
        .map_err(|_| anyhow::anyhow!("invalid setup request"))?;
    ensure!(
        matches!(request.action, Action::ConfigureProvider)
            || (request.credential.is_none()
                && request.credential_env.is_none()
                && request.credential_file.is_none()),
        "provider credentials are only accepted by configure_provider"
    );
    match request.action {
        Action::Inspect | Action::Verify => inspect(root, &request),
        Action::ConfigureProvider => configure_provider(root, &request),
        Action::ConfigureWorkers => configure_workers(&request),
        Action::ConfigureController => configure_controller(root).await,
        Action::CreateFleetKey => create_key(root, &request).await,
        Action::JoinWorker => {
            let Some(path) = request.invitation_file.as_deref() else {
                return Ok(blocked(
                    "invitation_missing",
                    "Provide the private fleet credential through invitation_file; use the target's existing secret injection facility.",
                    vec![],
                ));
            };
            if !path.exists() {
                return Ok(blocked(
                    "invitation_missing",
                    "The fleet credential file is missing on this machine; inject the private file through the agent's existing access to this target.",
                    vec![],
                ));
            }
            crate::worker_join::join(
                root,
                request.explicit_root,
                path,
                request.name.as_deref(),
                request.no_start,
            )
            .await
        }
        Action::RestartLocal => restart_plan(root),
    }
}

fn tool(args: Value) -> Value {
    json!({"kind":"tool","tool":"agent_setup","arguments":args})
}
fn execute(argv: Value) -> Value {
    json!({"kind":"exec","argv":argv,"executor":"calling_agent"})
}
fn blocked(code: &str, message: &str, next: Vec<Value>) -> Value {
    json!({"status":"blocked","blockers":[{"code":code,"message":message}],"next_actions":next})
}
fn executable(program: &str) -> Option<PathBuf> {
    let candidates = if program.contains('/') {
        vec![PathBuf::from(program)]
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|directory| directory.join(program))
            .collect()
    };
    candidates.into_iter().find(|path| {
        std::fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    })
}
fn tailscale() -> Option<PathBuf> {
    executable("tailscale")
        .or_else(|| executable("/Applications/Tailscale.app/Contents/MacOS/Tailscale"))
}
fn network_file(root: &Path) -> Option<PathBuf> {
    ["managed-network.toml", "network-runtime.toml"]
        .into_iter()
        .map(|file| root.join(file))
        .find(|file| file.exists())
}
fn network(root: &Path) -> Result<NetworkConfig> {
    NetworkConfig::load(network_file(root).as_deref())
}

fn access() -> Value {
    json!({"ssh_required":false,"remote_execution":"Use the calling agent's existing access to execute on the target and inject a private secret.","adapters":["sandbox execution API","container exec or startup specification","VM execution or startup specification","existing SSH access"],"platform_management":"Use the agent's connected platform tools; this tool does not create remote infrastructure.","local_tools":{"tailscale":tailscale().is_some(),"docker":executable("docker").is_some(),"codex":executable("codex").is_some(),"claude":executable("claude").is_some()}})
}

fn inspect(root: &Path, request: &Request) -> Result<Value> {
    let mut blockers = vec![];
    let mut next = vec![];
    let providers = match provisioning::list(&crate::branding::config_dir()) {
        Ok(providers) => providers,
        Err(_) => {
            blockers.push(json!({"code":"provider_configuration_invalid","message":"Existing provider settings cannot be loaded; preserve the file and have the agent repair its schema."}));
            vec![]
        }
    };
    if let Some(repo) = request.repo.as_deref() {
        ensure!(repo.is_absolute(), "repository path must be absolute");
    }
    let settings = Settings::load_user().ok();
    let project_settings = request.repo.as_deref().map(Settings::load).transpose()?;
    let worker_settings = project_settings.as_ref().or(settings.as_ref());
    for provider in &providers {
        if request
            .provider
            .as_deref()
            .is_some_and(|name| provider["provider"].as_str() != Some(name))
        {
            continue;
        }
        if provider["roles"].as_array().is_none_or(Vec::is_empty) {
            continue;
        }
        if let Some(required) = request.model.as_deref()
            && (required == "auto"
                || provider["model"]
                    .as_str()
                    .is_none_or(|model| model == "auto" || model != required))
        {
            blockers.push(json!({"code":"model_not_configured","provider":provider["provider"],"requested_model":required}));
            next.push(tool(json!({"action":"configure_provider","provider":provider["provider"],"model":required})));
        }
        if provider["credential"] == "missing" {
            blockers.push(json!({"code":"provider_credential_missing","provider":provider["provider"],"credential_env":provider["api_key_env"]}));
            next.push(tool(json!({"action":"configure_provider","provider":provider["provider"],"credential_env":provider["api_key_env"]})));
            if provider["kind"] == "tuara" {
                next.push(json!({"kind":"tool","tool":"provider_wallet","arguments":{"action":"inspect"},"reason":"For a new Tuara account, set up the Link wallet through MCP, collect signup choices conversationally, then use provider_signup. Existing accounts can use provider_login."}));
            }
        }
        if let Some(kind @ ("codex" | "claude")) = provider["kind"].as_str() {
            let program = settings
                .as_ref()
                .and_then(|settings| {
                    settings
                        .providers
                        .get(provider["provider"].as_str().unwrap_or(""))
                })
                .and_then(|provider| provider.program.as_deref())
                .unwrap_or(kind);
            if executable(program).is_none() {
                blockers.push(json!({"code":"harness_missing","provider":provider["provider"],"program":program,"message":"Use the calling agent's package installation access to install this harness."}));
            } else if provider["auth_mode"] == "login" {
                next.push(execute(if kind == "codex" {
                    json!([program, "login", "status"])
                } else {
                    json!([program, "auth", "status"])
                }));
            }
        }
    }
    let daemon = daemon_client::probe(root, "runtime_status", json!({})).ok();
    if daemon.is_none() {
        blockers.push(json!({"code":"daemon_unavailable","message":"The local daemon did not answer its private control socket."}));
        next.push(execute(json!(["horde", "--data-dir", root, "start"])));
    }
    let controller = match network(root) {
        Ok(config) => {
            json!({"configured":config.provider != Provider::Disabled,"worker":config.controller_peer.is_some(),"runtime":config.runtime_id})
        }
        Err(_) => json!({"configured":false,"error":"existing_network_configuration_invalid"}),
    };
    if controller["configured"] == false {
        next.push(tool(json!({"action":"configure_controller"})));
    }
    let children = worker_settings
        .map(|settings| {
            settings
                .executors
                .iter()
                .map(|(role, executor)| {
                    let provider = executor.provider();
                    json!({"role":role,"provider":provider,"kind":settings.providers.get(provider).map(|config|config.kind.as_str())})
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let selected = request.provider.as_deref().unwrap_or("default");
    let billing_provider = providers
        .iter()
        .find(|provider| provider["kind"] == "tuara" && provider["provider"] == selected)
        .or_else(|| {
            request
                .provider
                .is_none()
                .then(|| {
                    providers
                        .iter()
                        .find(|provider| provider["kind"] == "tuara")
                })
                .flatten()
        });
    let billing = if let Some(provider) = billing_provider {
        json!({
            "provider":provider["provider"],
            "credential":provider["credential"],
            "wallet":crate::provider_wallet::dispatch(&Store::open(root)?, &json!({"action":"inspect"}))?,
            "signup_tool":"provider_signup",
            "topup_tool":"provider_topup",
            "signup_choices":["organization_name","agent_name","amount_cents","max_charge_cents","terms_version","accept_terms"],
            "topup_choices":["threshold_cents","amount_cents","max_charge_cents","monthly_limit_cents","terms_version","accept_terms"]
        })
    } else {
        json!({"status":"no_tuara_provider_selected"})
    };
    let repository = if let Some(repo) = request.repo.as_deref() {
        let repo = repo.canonicalize().context("repository unavailable")?;
        let owner = crate::projects::infer(&Store::open(root)?, &repo)?;
        match owner.as_deref() {
            None => {
                next.push(json!({"kind":"tool","tool":"project_repo_add","arguments":{"project":"default","path":repo}}));
                json!({"status":"unregistered","path":repo})
            }
            Some(crate::projects::DEFAULT_PROJECT) => {
                json!({"status":"registered","path":repo,"project":"default"})
            }
            Some(project) => {
                blockers
                    .push(json!({"code":"repository_owned_by_other_project","project":project}));
                json!({"status":"other_project","path":repo,"project":project})
            }
        }
    } else {
        json!({"status":"not_selected"})
    };
    Ok(
        json!({"status":if blockers.is_empty(){"inspected"}else{"blocked"},"data_dir":root,"parent":{"connection":"admin_mcp","client":"any_mcp_client","repository":repository},"children":{"roles":children,"concurrency":worker_settings.map(|settings|settings.concurrency),"configure_tool":"agent_setup","configure_action":"configure_workers"},"billing":billing,"providers":providers,"daemon":daemon,"controller":controller,"checks":{"local_daemon":if daemon.is_some(){"responsive"}else{"unavailable"},"provider_api":"not_probed","subscription_authentication":"not_probed","controller_listener":"not_probed"},"access":access(),"presets":provisioning::PRESETS.iter().map(|preset|json!({"name":preset.name,"kind":preset.kind,"auth_mode":preset.auth_mode,"base_url":preset.base_url,"api_key_env":preset.api_key_env,"model":preset.model})).collect::<Vec<_>>(),"blockers":blockers,"next_actions":next}),
    )
}

fn validate_roles(roles: &[String]) -> Result<()> {
    ensure!(
        !roles.is_empty()
            && roles.len() <= 32
            && roles.iter().all(|role| !role.is_empty()
                && role.len() <= 48
                && role
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))),
        "provide 1-32 valid executor roles"
    );
    Ok(())
}

fn configure_workers(request: &Request) -> Result<Value> {
    let provider = request.provider.as_deref().context("provider required")?;
    validate_roles(&request.roles)?;
    let settings = Settings::load_user()?;
    ensure!(
        settings.providers.contains_key(provider),
        "provider is not configured"
    );
    let summary = provisioning::apply(
        &crate::branding::config_dir(),
        &provisioning::Spec {
            name: provider.into(),
            roles: request.roles.clone(),
            ..Default::default()
        },
        None,
    )?;
    let configured = provisioning::list(&crate::branding::config_dir())?
        .into_iter()
        .find(|item| item["provider"] == provider)
        .context("configured provider missing")?;
    let mut next = vec![tool(json!({"action":"verify"}))];
    if configured["kind"] == "tuara" && configured["credential"] == "missing" {
        next.push(json!({"kind":"tool","tool":"provider_wallet","arguments":{"action":"inspect"}}));
    }
    Ok(
        json!({"status":"configured","provider":provider,"roles":request.roles,"credential":configured["credential"],"summary":summary,"next_actions":next}),
    )
}

fn read_secret(request: &Request, default_env: &str) -> Result<Option<String>> {
    ensure!(
        [
            request.credential.is_some(),
            request.credential_env.is_some(),
            request.credential_file.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count()
            <= 1,
        "use only one of credential, credential_env, or credential_file"
    );
    let value = if let Some(value) = &request.credential {
        Some(value.clone())
    } else if let Some(path) = &request.credential_file {
        let file = std::fs::File::open(path).context("credential file unavailable")?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file()
                && metadata.permissions().mode() & 0o077 == 0
                && metadata.len() <= 16384,
            "credential must be a private regular file of at most 16 KiB"
        );
        let mut value = String::new();
        file.take(16385).read_to_string(&mut value)?;
        Some(value.trim().to_owned())
    } else if let Some(variable) = &request.credential_env {
        ensure!(
            !variable.is_empty()
                && variable.len() <= 128
                && variable.bytes().enumerate().all(|(i, c)| c == b'_'
                    || c.is_ascii_alphabetic()
                    || i > 0 && c.is_ascii_digit()),
            "invalid credential environment variable name"
        );
        crate::config::credential(variable).ok()
    } else {
        crate::config::credential(default_env).ok()
    };
    if let Some(value) = &value {
        ensure!(
            !value.trim().is_empty() && value.len() <= 16384 && !value.contains(['\n', '\r', '\0']),
            "credential is empty or has invalid content"
        );
    }
    Ok(value)
}
fn configure_provider(root: &Path, request: &Request) -> Result<Value> {
    let provider = request
        .provider
        .as_deref()
        .context("provider preset required")?;
    if let Some(url) = &request.base_url {
        let parsed = reqwest::Url::parse(url).context("invalid provider endpoint")?;
        ensure!(
            matches!(parsed.scheme(), "http" | "https")
                && parsed.host_str().is_some()
                && parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.query().is_none()
                && parsed.fragment().is_none(),
            "provider endpoint must be HTTP(S) without embedded credentials, query, or fragment"
        );
    }
    let program = request
        .program
        .as_ref()
        .map(|program| -> Result<String> {
            ensure!(
                !program.is_empty() && !program.chars().any(char::is_control),
                "invalid provider program"
            );
            Ok(if program.contains('/') {
                std::path::absolute(program)?
                    .to_str()
                    .context("provider program must be UTF-8")?
                    .into()
            } else {
                program.clone()
            })
        })
        .transpose()?;
    let base = provisioning::Spec {
        name: provider.into(),
        kind: request.kind.clone(),
        auth_mode: request.auth_mode.clone(),
        base_url: request.base_url.clone(),
        api_key_env: request.api_key_env.clone(),
        program,
        roles: request.roles.clone(),
        model: request.model.clone(),
    };
    let settings = Settings::load_user()?;
    let existing = settings.providers.get(provider);
    let spec = if let Some(existing) = existing {
        provisioning::Spec {
            kind: base.kind.or_else(|| Some(existing.kind.clone())),
            auth_mode: base.auth_mode.or_else(|| Some(existing.auth_mode.clone())),
            api_key_env: base
                .api_key_env
                .or_else(|| Some(existing.api_key_env.clone())),
            ..base
        }
    } else if let Some(preset) = provisioning::preset(provider) {
        base.with_preset(preset)
    } else {
        base
    };
    let spec = provisioning::Spec {
        name: provisioning::existing_equivalent(&settings, &spec)
            .unwrap_or_else(|| spec.name.clone()),
        ..spec
    };
    let existing = settings.providers.get(&spec.name);
    if let Some(account) = existing.and_then(|provider| provider.account.as_deref()) {
        let db = Store::open(root)?;
        let managed: bool = db.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?)",
            [account],
            |row| row.get(0),
        )?;
        ensure!(
            !managed,
            "provider selects a managed account; configure its credentials with account_credential_set"
        );
    }
    let Some(kind) = spec.kind.as_deref() else {
        return Ok(blocked(
            "provider_details_missing",
            "Discover the provider kind, endpoint, concrete model, and credential environment name from existing access or official documentation.",
            vec![tool(json!({"action":"inspect"}))],
        ));
    };
    ensure!(
        ["tuara", "codex", "claude", "grok", "simulated"].contains(&kind),
        "unsupported provider kind"
    );
    let auth_mode =
        spec.auth_mode
            .as_deref()
            .unwrap_or(if kind == "tuara" { "api" } else { "login" });
    ensure!(
        ["api", "login"].contains(&auth_mode),
        "invalid provider authentication mode"
    );
    let variable = spec.api_key_env.clone().unwrap_or_default();
    if auth_mode == "api" || kind == "tuara" {
        ensure!(
            !variable.is_empty()
                && variable.len() <= 128
                && variable.bytes().enumerate().all(|(i, c)| c == b'_'
                    || c.is_ascii_alphabetic()
                    || i > 0 && c.is_ascii_digit()),
            "provider requires a valid api_key_env"
        );
        ensure!(
            spec.base_url
                .as_ref()
                .or_else(|| existing.map(|provider| &provider.base_url))
                .is_some_and(|url| !url.is_empty()),
            "API provider requires its discovered base_url"
        );
    }
    let program = spec
        .program
        .as_deref()
        .or_else(|| existing.and_then(|provider| provider.program.as_deref()))
        .unwrap_or(kind);
    ensure!(
        request.roles.len() <= 32
            && request.roles.iter().all(|role| !role.is_empty()
                && role.len() <= 48
                && role
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))),
        "invalid executor roles"
    );
    ensure!(
        request.model.as_ref().is_none_or(|model| !model.is_empty()
            && model.len() <= 256
            && !model.chars().any(char::is_control)),
        "invalid model name"
    );
    if matches!(kind, "codex" | "claude") && executable(program).is_none() {
        return Ok(blocked(
            "harness_missing",
            "The required harness is not installed; use the calling agent's package installation access, then repeat this action.",
            vec![],
        ));
    }
    let key = if auth_mode == "api" || kind == "tuara" {
        match read_secret(request, &variable)? {
            Some(key) => Some(key),
            None => {
                return Ok(blocked(
                    "provider_credential_missing",
                    "The provider credential is unavailable. Supply credential, credential_env, or credential_file, then repeat this action.",
                    vec![],
                ));
            }
        }
    } else {
        ensure!(
            request.credential.is_none()
                && request.credential_env.is_none()
                && request.credential_file.is_none(),
            "subscription providers use their own login store, not an API key"
        );
        None
    };
    let spec = provisioning::Spec {
        auth_mode: Some(auth_mode.into()),
        ..spec
    };
    let changed = key
        .as_ref()
        .is_some_and(|key| crate::config::credential(&variable).ok().as_ref() != Some(key));
    let key_supplied = key.is_some();
    let summary = provisioning::apply(&crate::branding::config_dir(), &spec, key)?;
    if key_supplied
        && (request.credential.is_some()
            || request.credential_env.is_some()
            || request.credential_file.is_some())
    {
        crate::config::activate_saved_credential(&variable)?;
    }
    if changed {
        let updated = Settings::load_user()?;
        let config = updated
            .provider(&spec.name)
            .context("updated API provider is unavailable")?;
        crate::capacity::credentials_changed(&Store::open(root)?, &config)?;
    }
    let next = vec![tool(json!({"action":"verify"}))];
    Ok(
        json!({"status":"configured","provider":provider,"summary":summary,"provider_api":"not_probed","credential_activation":if key_supplied {"next_invocation"} else {"not_applicable"},"restart_required":false,"activation_message":if key_supplied {"Saved credentials are read for the next provider invocation; running invocations keep their existing credentials."} else {"This provider uses its harness login store; no API credential was changed."},"next_actions":next}),
    )
}

fn restart_plan(root: &Path) -> Result<Value> {
    let db = Store::open(root)?;
    let unsafe_work: bool = db.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts WHERE state IN ('running','uncertain'))",
        [],
        |row| row.get(0),
    )?;
    if unsafe_work {
        return Ok(blocked(
            "runtime_busy",
            "Active or uncertain work must be completed or reconciled before restarting this daemon.",
            vec![],
        ));
    }
    Ok(
        json!({"status":"action_required","reason":"restart_local","next_actions":[execute(json!(["horde","--data-dir",root,"stop"])),execute(json!(["horde","--data-dir",root,"start"])),tool(json!({"action":"verify"}))]}),
    )
}

async fn configure_controller(root: &Path) -> Result<Value> {
    let existing = match network(root) {
        Ok(config) => config,
        Err(_) => {
            return Ok(blocked(
                "existing_network_invalid",
                "The existing network identity cannot be loaded; preserve it and repair its configuration before provisioning.",
                vec![],
            ));
        }
    };
    if existing.controller_peer.is_some() {
        return Ok(blocked(
            "worker_identity_present",
            "This data directory already belongs to a worker; configure the controller in its own data directory.",
            vec![],
        ));
    }
    if existing.provider != Provider::Disabled {
        return Ok(
            json!({"status":"configured","runtime":existing.runtime_id,"existing_identity":true,"controller_ready":false,"next_actions":[tool(json!({"action":"verify"}))]}),
        );
    }
    if daemon_client::running(root) {
        let plan = restart_plan(root)?;
        if plan["status"] == "blocked" {
            return Ok(plan);
        }
        return Ok(blocked(
            "controller_restart_required",
            "Horde is running without networking. The calling agent must stop this idle daemon, configure networking, and verify the restarted controller.",
            vec![
                execute(json!(["horde", "--data-dir", root, "stop"])),
                execute(json!(["horde", "--data-dir", root, "network", "setup"])),
                tool(json!({"action":"verify"})),
            ],
        ));
    }
    if root.join("network-runtime.toml").exists() || root.join("managed-network.toml").exists() {
        return Ok(blocked(
            "existing_network_identity",
            "Existing network files must be preserved; select a dedicated controller data directory.",
            vec![],
        ));
    }
    let Some(program) = tailscale() else {
        return Ok(blocked(
            "network_access_missing",
            "No existing private network access was found. Use the calling agent's access to install and authenticate Tailscale, or supply an already configured reachable controller.",
            vec![execute(json!(["horde", "network", "setup"]))],
        ));
    };
    let config = NetworkConfig {
        provider: Provider::Tailscale,
        tailscale_program: program.clone(),
        discover_all: true,
        ..Default::default()
    };
    let discovery = match crate::network::discover(&config).await {
        Ok(discovery) => discovery,
        Err(_) => {
            return Ok(blocked(
                "tailnet_access_unavailable",
                "The installed Tailscale client did not provide authenticated network access; the calling agent must restore its login or daemon access before retrying.",
                vec![
                    execute(json!([program, "status", "--json"])),
                    execute(json!([program, "up"])),
                ],
            ));
        }
    };
    let network = crate::pairing::initialize(root, config, &discovery)?;
    Ok(
        json!({"status":"configured","runtime":network.runtime_id,"controller_ready":false,"next_actions":[execute(json!(["horde","--data-dir",root,"start"])),tool(json!({"action":"verify"}))]}),
    )
}

async fn create_key(root: &Path, request: &Request) -> Result<Value> {
    let setup = configure_controller(root).await?;
    if setup["status"] == "blocked" {
        return Ok(setup);
    }
    let name = request.name.as_deref().unwrap_or("workers");
    crate::runtime_directory::validate_name(name)?;
    let output = request
        .output_file
        .clone()
        .unwrap_or_else(|| root.join("fleet-credentials").join(format!("{name}.json")));
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let result = cli::run(
        root,
        network_file(root).as_deref(),
        &KeyCommands::Create {
            name: name.into(),
            listen: None,
            enrollment_address: None,
            controller_address: None,
            tls_name: None,
            issuer_key: None,
            output: Some(output.clone()),
            expires_in: request.expires_in.unwrap_or(2_592_000),
            max_workers: request.max_workers.unwrap_or(100),
            concurrency: request.concurrency.unwrap_or(4),
        },
    )
    .await?;
    Ok(
        json!({"status":"configured","credential_file":output,"key":result["key"],"controller_ready":false,"bootstrap":{"argv":["horde","daemon"],"environment":{"HORDE_ENROLLMENT_FILE":"/run/secrets/horde-fleet.json"},"secret_mount":{"source_file":output,"target_file":"/run/secrets/horde-fleet.json","mode":"0600"},"data_directory":"Give each worker its own persistent data directory for its private identity.","executor":"calling_agent_platform_access","ssh_required":false},"next_actions":[execute(json!(["horde","--data-dir",root,"start"])),tool(json!({"action":"verify"}))]}),
    )
}
