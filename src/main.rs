use anyhow::{Context, Result, bail};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    path::PathBuf,
};
mod watch;
#[derive(Parser)]
#[command(name = "horde", version, about = "Durable local task orchestration")]
struct Cli {
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Set up networking, discover hosts, and install authenticated remote runtimes.
    Network {
        /// User-owned network configuration; never merged with repository settings.
        #[arg(long, global = true)]
        config: Option<PathBuf>,
        #[command(subcommand)]
        command: NetworkCommands,
    },
    /// Run the durable execution service in the foreground.
    Daemon,
    /// Start the daemon detached from this client.
    Start,
    /// Stop the daemon gracefully, retaining durable work.
    Stop,
    /// Submit a task; starts execution immediately unless settings require confirmation.
    Submit {
        objective: String,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long)]
        template: Option<String>,
        /// Run on a connected runtime by name or ID.
        #[arg(long, alias = "runtime")]
        on: Option<String>,
    },
    Inspect {
        task: String,
    },
    /// Get a local checkout of a completed remote task for review.
    Result {
        task: String,
    },
    List,
    /// Summarize reported usage, cost, retries and coordination overhead.
    Metrics {
        task: String,
    },
    /// Print a task's durable events; with --follow, stream them as NDJSON until the task ends.
    Events {
        task: String,
        /// Keep streaming until the task is terminal (same as `watch`).
        #[arg(long)]
        follow: bool,
        #[command(flatten)]
        options: watch::Options,
    },
    /// Stream a task's durable events as NDJSON until it succeeds (exit 0), fails (1), or is cancelled (2).
    Watch {
        task: String,
        #[command(flatten)]
        options: watch::Options,
    },
    /// Print the terminal summary: status, step outcomes, integrated head, and delivery outcome.
    Summary {
        task: String,
    },
    Cancel {
        task: String,
    },
    Resume {
        task: String,
    },
    Answer {
        task: String,
        question: String,
        answer: String,
    },
    /// Send an operator message to workers on a task; omit --worker to fan out.
    Steer {
        task: String,
        body: String,
        /// Client message id for retry deduplication; generated when omitted.
        #[arg(long)]
        id: Option<String>,
        /// Deliver without waking idle workers.
        #[arg(long)]
        presence: bool,
        /// Target one worker from `list_workers`; omit to reach every worker.
        #[arg(long, visible_alias = "to")]
        worker: Option<String>,
    },
    /// Invoke any coordination/runtime operation using a JSON object (same API as MCP).
    Call {
        method: String,
        #[arg(default_value = "{}")]
        args: String,
    },
    /// Expose runtime tools to a personal agent over newline-delimited stdio MCP.
    Mcp,
    /// Validate a composed template without executing it.
    Validate {
        template: String,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, default_value = "example task")]
        objective: String,
    },
    /// Print merged settings or probe a native provider model and streaming tools.
    Doctor {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long)]
        probe_tuara: bool,
        #[arg(long, conflicts_with = "probe_tuara")]
        probe: bool,
        #[arg(long)]
        provider: Option<String>,
    },
    /// Print a starter TOML configuration.
    Config {
        #[command(subcommand)]
        command: Option<ConfigCommands>,
    },
    Usage,
    Service {
        #[command(subcommand)]
        command: ServiceCommands,
    },
    Update {
        #[arg(long)]
        check: bool,
        #[arg(long)]
        version: Option<String>,
        #[arg(long, hide = true)]
        operation: Option<String>,
    },
    /// Inspect or install workflow skills without replacing the runtime binary.
    Skills {
        #[command(subcommand)]
        command: SkillCommands,
    },
    Runtime {
        #[command(subcommand)]
        command: RuntimeCommands,
    },
}
#[derive(Subcommand)]
enum SkillCommands {
    /// Show the effective default skill pack and its content hash.
    List,
    /// Install every skill directory in a local pack for new tasks.
    Install { path: PathBuf },
}
#[derive(Subcommand)]
enum ServiceCommands {
    Install,
    Status,
    Uninstall,
}
#[derive(Subcommand)]
enum ConfigCommands {
    /// Write a starter config.toml and credentials.env, keeping anything already there.
    Init {
        /// Also walk through adding providers, when there is a terminal to ask on.
        #[arg(long)]
        interactive: bool,
    },
    /// Add or adjust a provider and store its API key.
    Provider {
        #[command(subcommand)]
        command: ProviderCommands,
    },
    /// List the models a configured provider's endpoint will accept.
    Models {
        provider: String,
    },
    Get {
        key: String,
    },
    Set {
        key: String,
        value: usize,
    },
}
#[derive(Subcommand)]
enum ProviderCommands {
    /// Configured providers, the roles on each, and whether its key is readable.
    List,
    /// Walk through adding a provider, or state it with flags for a script.
    ///
    /// The API key is never taken as an argument: it is prompted for without echo,
    /// or read from standard input with --key-stdin.
    Add {
        /// Provider name. With no name and a terminal, the walkthrough runs.
        name: Option<String>,
        /// Start from a ready-made provider: tuara, codex, claude, openai, anthropic.
        #[arg(long)]
        preset: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        auth_mode: Option<String>,
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        api_key_env: Option<String>,
        #[arg(long)]
        model: Option<String>,
        /// Executor roles to point at this provider.
        #[arg(long, value_delimiter = ',')]
        use_for: Vec<String>,
        /// Read the API key from standard input instead of prompting.
        #[arg(long)]
        key_stdin: bool,
    },
}
#[derive(Subcommand)]
enum RuntimeCommands {
    Reconcile {
        id: String,
        #[arg(long)]
        resource: String,
        #[arg(long)]
        request_id: String,
    },
    Status,
    Drain,
    Resume,
    List {
        /// Emit the complete machine-readable runtime listing.
        #[arg(long)]
        json: bool,
    },
    /// Give a connected runtime a memorable name.
    Rename {
        id: String,
        name: String,
    },
    /// Forget a disconnected runtime without deleting its machine or container.
    Remove {
        id: String,
    },
    Inspect {
        id: String,
    },
    Create {
        id: String,
        #[arg(long)]
        profile: String,
        #[arg(long)]
        request_id: String,
    },
    Destroy {
        id: String,
        #[arg(long)]
        request_id: String,
    },
    Restart {
        id: String,
        #[arg(long)]
        request_id: String,
    },
    Update {
        id: String,
        #[arg(long, required_unless_present = "skills", conflicts_with = "skills")]
        version: Option<String>,
        /// Send this controller's current skill pack; no binary restart is needed.
        #[arg(long)]
        skills: bool,
        /// Reuse this ID to reconcile a previous update request.
        #[arg(long)]
        request_id: Option<String>,
    },
    Start {
        id: String,
        #[arg(long)]
        request_id: String,
    },
    Stop {
        id: String,
        #[arg(long)]
        request_id: String,
    },
}

#[derive(Subcommand)]
enum NetworkCommands {
    /// Create, inspect, or revoke credentials for automatic fleet enrollment.
    Key {
        #[command(subcommand)]
        command: horde::fleet_enrollment::cli::KeyCommands,
    },
    /// Enroll this machine using a fleet credential file, without SSH.
    Join {
        /// Fleet credential file generated by network key create.
        #[arg(
            value_name = "FILE",
            required_unless_present = "invitation",
            conflicts_with = "invitation"
        )]
        file: Option<PathBuf>,
        /// Compatibility spelling for the credential file.
        #[arg(long, value_name = "FILE")]
        invitation: Option<PathBuf>,
        /// Name shown on the controller; defaults to this machine's hostname.
        #[arg(long)]
        name: Option<String>,
        /// Enroll without starting the daemon (for supervised deployments).
        #[arg(long)]
        no_start: bool,
    },
    /// Revoke an enrolled worker's access and certificate renewal.
    Revoke { runtime: String },
    /// Install/connect Tailscale and generate Horde certificates automatically.
    Setup {
        /// Prepare a Linux worker for pairing over Tailscale SSH.
        #[arg(long)]
        worker: bool,
        /// Start Horde at machine boot (requires service setup privileges).
        #[arg(long)]
        service: bool,
    },
    /// Discover, install, and pair a worker using Tailscale SSH.
    Add {
        /// Non-root account and discovered hostname, node ID, or tailnet IP.
        target: String,
        /// Install the worker boot service; requires remote passwordless sudo.
        #[arg(long)]
        service: bool,
    },
    #[command(hide = true)]
    Accept {
        #[arg(long)]
        service: bool,
    },
    /// Print a starter network configuration (disabled by default).
    Config,
    /// Discover candidates; discovery does not grant execution authority.
    Peers,
    /// Run the mTLS runtime listener in the foreground.
    Listen,
    /// Verify a discovered peer's certificate and authenticated health service.
    Probe { peer: String },
}
fn request(root: &std::path::Path, method: &str, args: Value) -> Result<Value> {
    let mut stream = std::os::unix::net::UnixStream::connect(root.join("daemon.sock"))
        .context("daemon unavailable; run `horde start`")?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(120)))?;
    let token = horde::branding::var("HORDE_WORKER_TOKEN").ok();
    writeln!(
        stream,
        "{}",
        json!({"method":method,"args":args,"token":token})
    )?;
    let mut line = String::new();
    std::io::BufReader::new(stream).read_line(&mut line)?;
    let response: Value = serde_json::from_str(&line).context("invalid daemon response")?;
    if let Some(error) = response.get("error") {
        bail!("{}", error.as_str().unwrap_or("daemon error"));
    }
    Ok(response["result"].clone())
}
fn mcp(root: &std::path::Path) -> Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.len() > 1024 * 1024 {
            bail!("MCP request too large");
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(v) => horde::protocol::mcp_response(&v, |name, args| request(root, name, args)),
            Err(e) => Some(
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":e.to_string()}}),
            ),
        };
        if let Some(mut response) = response {
            if horde::branding::var_os("HORDE_WORKER_TOKEN").is_some()
                && let Some(tools) = response["result"]["tools"].as_array_mut()
            {
                tools.retain(|t| horde::protocol::worker_allowed(t["name"].as_str().unwrap_or("")));
                let options = request(root, "knowledge_options", json!({}))?;
                let topics: Vec<String> = serde_json::from_value(options["topics"].clone())?;
                for tool in tools {
                    tool["inputSchema"] =
                        horde::protocol::schema(tool["name"].as_str().unwrap_or(""));
                    if matches!(tool["name"].as_str(), Some("add_knowledge" | "knowledge")) {
                        horde::knowledge::apply_topics(&mut tool["inputSchema"], &topics);
                    }
                }
            }
            writeln!(out, "{response}")?;
            out.flush()?;
        }
    }
    Ok(())
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let matches = Cli::command()
        .name(horde::branding::cli_name())
        .get_matches();
    let cli = Cli::from_arg_matches(&matches)?;
    let explicit_root = cli.data_dir.is_some();
    let root = cli.data_dir.unwrap_or_else(horde::branding::data_dir);
    std::fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    let output = match cli.command {
        Commands::Network { config, command } => {
            match &command {
                NetworkCommands::Key { command } => {
                    anyhow::ensure!(
                        config.is_none(),
                        "fleet keys use the controller's configured network identity; omit --config"
                    );
                    println!(
                        "{}",
                        horde::fleet_enrollment::cli::run(&root, config.as_deref(), command)
                            .await?
                    );
                    return Ok(());
                }
                NetworkCommands::Join {
                    file,
                    invitation,
                    name,
                    no_start,
                } => {
                    anyhow::ensure!(
                        config.is_none(),
                        "join obtains its network settings from the fleet invitation; omit --config"
                    );
                    let path = file
                        .as_ref()
                        .or(invitation.as_ref())
                        .context("credential file required")?;
                    let result = horde::worker_join::join(
                        &root,
                        explicit_root,
                        path,
                        name.as_deref(),
                        *no_start,
                    )
                    .await?;
                    println!("{}", serde_json::to_string_pretty(&result)?);
                    return Ok(());
                }
                NetworkCommands::Revoke { runtime } => {
                    horde::fleet_enrollment::admin()?;
                    horde::fleet_enrollment::authority::revoke_worker(
                        &horde::store::Store::open(&root)?,
                        runtime,
                    )?;
                    println!("{}", json!({"runtime":runtime,"revoked":true}));
                    return Ok(());
                }
                NetworkCommands::Setup { worker, service } => {
                    anyhow::ensure!(
                        config.is_none(),
                        "setup owns its per-runtime configuration; omit --config"
                    );
                    println!("{}", horde::pairing::setup(&root, *worker, *service).await?);
                    return Ok(());
                }
                NetworkCommands::Add { target, service } => {
                    anyhow::ensure!(
                        config.is_none(),
                        "pairing uses the setup configuration; omit --config"
                    );
                    println!("{}", horde::pairing::add(&root, target, *service).await?);
                    return Ok(());
                }
                NetworkCommands::Accept { service } => {
                    println!("{}", horde::pairing::accept(&root, *service).await?);
                    return Ok(());
                }
                _ => {}
            }
            if matches!(command, NetworkCommands::Config) {
                println!(
                    "{}",
                    toml::to_string_pretty(&horde::network::NetworkConfig::default())?
                );
                return Ok(());
            }
            let managed = root.join("managed-network.toml");
            let settings = horde::network::NetworkConfig::load(
                config
                    .as_deref()
                    .or_else(|| managed.exists().then_some(managed.as_path())),
            )?;
            match command {
                NetworkCommands::Config
                | NetworkCommands::Key { .. }
                | NetworkCommands::Join { .. }
                | NetworkCommands::Revoke { .. }
                | NetworkCommands::Setup { .. }
                | NetworkCommands::Add { .. }
                | NetworkCommands::Accept { .. } => unreachable!(),
                NetworkCommands::Peers => json!(horde::network::discover(&settings).await?),
                NetworkCommands::Probe { peer } => horde::network::probe(&settings, &peer).await?,
                NetworkCommands::Listen => {
                    let listener = horde::network::bind_listener(&settings).await?;
                    eprintln!(
                        "Horde runtime network listener: {} (mutual TLS and explicit execution enrollment required)",
                        listener.local_addr()?
                    );
                    let mut terminate =
                        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
                    horde::federation::configure(&root, &settings)?;
                    horde::network::serve_runtime(&settings, listener, async move {
                        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
                    },root.clone()).await?;
                    return Ok(());
                }
            }
        }
        Commands::Daemon => {
            return tokio::task::LocalSet::new()
                .run_until(horde::runtime::daemon(&root))
                .await;
        }
        Commands::Mcp => return mcp(&root),
        Commands::Stop => request(&root, "shutdown", json!({}))?,
        Commands::Start => horde::daemon_client::start(&root).await?,
        Commands::Submit {
            objective,
            repo,
            template,
            on,
        } => request(
            &root,
            "submit_task",
            json!({"objective":objective,"repo":repo.canonicalize()?,"template":template,"on":on}),
        )?,
        Commands::Inspect { task } => request(&root, "inspect", json!({"task":task}))?,
        Commands::Result { task } => request(&root, "remote_result", json!({"task":task}))?,
        Commands::Metrics { task } => request(&root, "metrics", json!({"task":task}))?,
        Commands::List => request(&root, "list_tasks", json!({}))?,
        Commands::Events {
            task,
            follow: false,
            options,
        } => request(&root, "events", json!({"task":task,"after":options.after}))?,
        Commands::Events {
            task,
            follow: true,
            options,
        }
        | Commands::Watch { task, options } => {
            std::process::exit(watch::run(&root, &task, &options).await?)
        }
        Commands::Summary { task } => request(&root, "summary", json!({"task":task}))?,
        Commands::Cancel { task } => request(&root, "cancel", json!({"task":task}))?,
        Commands::Resume { task } => request(&root, "resume", json!({"task":task}))?,
        Commands::Answer {
            task,
            question,
            answer,
        } => request(
            &root,
            "answer_question",
            json!({"task":task,"question":question,"answer":answer}),
        )?,
        Commands::Steer {
            task,
            body,
            id,
            presence,
            worker,
        } => {
            let mut args = json!({"task":task,"body":body,"id":id,"actionable":!presence});
            if let Some(worker) = worker {
                args["worker"] = json!(worker);
            }
            request(&root, "steer", args)?
        }
        Commands::Call { method, args } => request(&root, &method, serde_json::from_str(&args)?)?,
        Commands::Validate {
            template,
            repo,
            objective,
        } => {
            let mut plan = json!(horde::template::compile(
                &template,
                &horde::template::load_templates(&horde::branding::templates(&repo))?,
                std::collections::BTreeMap::from([("task".into(), objective)])
            )?);
            // Capture the skill catalog as submission would, so a step selecting a
            // skill the catalog does not hold fails here and not at the first
            // `submit`. Validation stays structural when no pack resolves: the
            // catalog error is reported and the selection check is skipped.
            let settings = horde::config::Settings::load(&repo)?;
            match horde::skills::baseline_for_root(&root, &repo.canonicalize()?, &settings.skills) {
                Ok(catalog) => {
                    for (index, step) in plan["steps"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .enumerate()
                    {
                        for name in step["skills"].as_array().cloned().unwrap_or_default() {
                            let name = name.as_str().unwrap_or("");
                            anyhow::ensure!(
                                catalog.contains_key(name),
                                "steps[{index}].skills: unknown skill {name:?}; available: {}",
                                catalog.keys().cloned().collect::<Vec<_>>().join(", ")
                            );
                        }
                    }
                    plan["skill_catalog"] = horde::skill_catalog::report(&catalog)?;
                }
                Err(error) => plan["skill_catalog"] = json!({"error": error.to_string()}),
            }
            plan
        }
        Commands::Doctor {
            repo,
            probe_tuara,
            probe,
            provider,
        } => {
            let skill_pack = horde::skill_catalog::check(&root)?;
            let settings = horde::config::Settings::load(&repo)?;
            let mut result = if probe || probe_tuara {
                let config = if probe_tuara {
                    settings
                        .executor("native")
                        .context("native executor missing")?
                } else {
                    settings
                        .provider(provider.as_deref().unwrap_or("default"))
                        .context("provider missing")?
                };
                anyhow::ensure!(
                    config.kind == "tuara",
                    "streaming probe requires a native tuara provider"
                );
                horde::executor::probe_tools(&config).await?
            } else {
                let mut resolved_models = serde_json::Map::new();
                if let Some(name) = &provider {
                    let config = settings.provider(name).context("provider missing")?;
                    anyhow::ensure!(
                        config.kind == "tuara",
                        "catalog resolution requires a native tuara provider"
                    );
                    resolved_models.insert(name.clone(), horde::executor::probe(&config).await?);
                } else {
                    for (name, provider) in &settings.providers {
                        if provider.kind == "tuara" && provider.model.as_deref() == Some("auto") {
                            resolved_models.insert(
                                name.clone(),
                                horde::executor::probe(
                                    &settings.provider(name).context("provider missing")?,
                                )
                                .await?,
                            );
                        }
                    }
                }
                json!({"settings":settings,"resolved_models":resolved_models,"data_dir":root,"daemon":root.join("daemon.sock").exists()})
            };
            result["skill_pack"] = skill_pack;
            result
        }
        Commands::Update {
            check,
            version,
            operation,
        } => {
            let result =
                horde::update::run(&root, version.as_deref(), check, operation.as_deref()).await;
            if let Some(id) = operation {
                let db = horde::store::Store::open(&root)?;
                let evidence = match &result {
                    Ok(v) => v.clone(),
                    Err(e) => json!({"error":e.to_string()}),
                };
                db.conn.execute(
                    "UPDATE runtime_operations SET state=?,result=? WHERE id=? AND runtime='local' AND state NOT IN ('succeeded','blocked')",
                    rusqlite::params![
                        if result.is_ok() {
                            "succeeded"
                        } else {
                            "failed"
                        },
                        evidence.to_string(),
                        id
                    ],
                )?;
            }
            result?
        }
        Commands::Service { command } => horde::service::action(
            &root,
            match command {
                ServiceCommands::Install => "install",
                ServiceCommands::Status => "status",
                ServiceCommands::Uninstall => "uninstall",
            },
        )?,
        Commands::Usage => request(&root, "account_status", json!({}))?,
        Commands::Skills { command } => match command {
            SkillCommands::List => request(&root, "skill_pack_list", json!({}))?,
            SkillCommands::Install { path } => request(
                &root,
                "skill_pack_install",
                json!({"path":std::fs::canonicalize(path)?}),
            )?,
        },
        Commands::Runtime { command } => {
            let human_list = matches!(&command, RuntimeCommands::List { json: false })
                && std::io::IsTerminal::is_terminal(&std::io::stdout());
            let (method, args) = match command {
                RuntimeCommands::Reconcile {
                    id,
                    resource,
                    request_id,
                } => (
                    "runtime_reconcile",
                    json!({"id":id,"resource":resource,"request_id":request_id}),
                ),
                RuntimeCommands::Status => ("runtime_status", json!({})),
                RuntimeCommands::Drain => ("runtime_drain", json!({})),
                RuntimeCommands::Resume => ("runtime_resume", json!({})),
                RuntimeCommands::List { .. } => ("runtime_list", json!({})),
                RuntimeCommands::Rename { id, name } => {
                    ("runtime_rename", json!({"id":id,"name":name}))
                }
                RuntimeCommands::Remove { id } => ("runtime_forget", json!({"id":id})),
                RuntimeCommands::Inspect { id } => ("runtime_inspect", json!({"id":id})),
                RuntimeCommands::Create {
                    id,
                    profile,
                    request_id,
                } => (
                    "runtime_create",
                    json!({"id":id,"profile":profile,"request_id":request_id}),
                ),
                RuntimeCommands::Destroy { id, request_id } => {
                    ("runtime_destroy", json!({"id":id,"request_id":request_id}))
                }
                RuntimeCommands::Restart { id, request_id } => {
                    ("runtime_restart", json!({"id":id,"request_id":request_id}))
                }
                RuntimeCommands::Update {
                    id,
                    version,
                    skills,
                    request_id,
                } => {
                    let request_id = request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                    if skills {
                        (
                            "runtime_skills_update",
                            json!({"id":id,"request_id":request_id}),
                        )
                    } else {
                        (
                            "runtime_update",
                            json!({"id":id,"version":version,"request_id":request_id}),
                        )
                    }
                }
                RuntimeCommands::Start { id, request_id } => {
                    ("runtime_start", json!({"id":id,"request_id":request_id}))
                }
                RuntimeCommands::Stop { id, request_id } => {
                    ("runtime_stop", json!({"id":id,"request_id":request_id}))
                }
            };
            let result = request(&root, method, args)?;
            if human_list {
                print_runtime_list(&result)?;
                return Ok(());
            }
            result
        }
        Commands::Config {
            command: Some(command),
        } => match command {
            ConfigCommands::Init { interactive } => {
                let directory = horde::branding::config_dir();
                let created = horde::config::initialize(&directory)?;
                if created.is_empty() {
                    println!("Configuration already present in {}.", directory.display());
                } else {
                    for file in &created {
                        println!("Wrote {}.", file.display());
                    }
                }
                // With no terminal — a CI install, or a tool call — say what to run
                // later rather than failing the install on an unanswerable prompt.
                let terminal = interactive
                    .then(horde::provisioning::Terminal::open)
                    .flatten();
                match terminal {
                    Some(mut terminal) => {
                        if terminal.confirm("\nSet up a provider and its API key now?", true)? {
                            horde::provisioning::interactive(&directory, &mut terminal)?;
                        }
                    }
                    None => println!(
                        "Add your first API key with: {} config provider add",
                        horde::branding::cli_name()
                    ),
                }
                return Ok(());
            }
            ConfigCommands::Provider { command } => {
                let directory = horde::branding::config_dir();
                match command {
                    ProviderCommands::List => json!(horde::provisioning::list(&directory)?),
                    ProviderCommands::Add {
                        name: None,
                        preset: None,
                        ..
                    } => {
                        let mut terminal = horde::provisioning::Terminal::open().context(
                            "no terminal to prompt on; name a provider, or pipe the key to --key-stdin",
                        )?;
                        horde::provisioning::interactive(&directory, &mut terminal)?;
                        return Ok(());
                    }
                    ProviderCommands::Add {
                        name,
                        preset,
                        kind,
                        auth_mode,
                        base_url,
                        api_key_env,
                        model,
                        use_for,
                        key_stdin,
                    } => {
                        let chosen = preset.or_else(|| name.clone());
                        let name = name
                            .or_else(|| chosen.clone())
                            .context("provider name required")?;
                        let mut spec = horde::provisioning::Spec {
                            name,
                            program: None,
                            kind,
                            auth_mode,
                            base_url,
                            api_key_env,
                            model,
                            roles: use_for,
                        };
                        if let Some(preset) =
                            chosen.as_deref().and_then(horde::provisioning::preset)
                        {
                            spec = spec.with_preset(preset);
                        }
                        // A login provider reads the CLI's own store, so it needs no
                        // key; neither does one whose key is already stored, so that
                        // changing a model does not mean typing the key again.
                        let needs_key = spec.auth_mode.as_deref() != Some("login")
                            && !spec
                                .api_key_env
                                .as_deref()
                                .is_some_and(|v| horde::config::credential(v).is_ok());
                        let key = if key_stdin {
                            Some(horde::provisioning::read_piped_key()?)
                        } else if needs_key {
                            let variable = spec.api_key_env.clone().unwrap_or_default();
                            let mut terminal = horde::provisioning::Terminal::open()
                                .context("no terminal to prompt on; pipe the key to --key-stdin")?;
                            Some(terminal.secret(&format!("{variable} (input hidden): "))?)
                        } else {
                            None
                        };
                        println!(
                            "{}",
                            horde::provisioning::apply(
                                &directory,
                                &spec,
                                key.filter(|k| !k.is_empty())
                            )?
                        );
                        return Ok(());
                    }
                }
            }
            ConfigCommands::Models { provider } => {
                json!(horde::provisioning::models(&horde::branding::config_dir(), &provider).await?)
            }
            ConfigCommands::Get { key } => {
                anyhow::ensure!(key == "concurrency", "supported key: concurrency");
                request(&root, "runtime_config_get", json!({}))?
            }
            ConfigCommands::Set { key, value } => {
                anyhow::ensure!(key == "concurrency", "supported key: concurrency");
                request(&root, "runtime_config_set", json!({"concurrency":value}))?
            }
        },
        Commands::Config { command: None } => {
            println!(
                "{}",
                toml::to_string_pretty(&horde::config::Settings::default())?
            );
            return Ok(());
        }
    };
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn print_runtime_list(value: &Value) -> Result<()> {
    let rows = value.as_array().context("runtime listing")?;
    if rows.is_empty() {
        println!("No workers connected. Create a key with horde network key create workers.");
        return Ok(());
    }
    println!("{:<24} {:<12} {:<16} ID", "NAME", "STATUS", "PROFILE");
    for row in rows {
        println!(
            "{:<24} {:<12} {:<16} {}",
            row["name"].as_str().unwrap_or("-"),
            row["state"].as_str().unwrap_or("unknown"),
            row["profile"].as_str().unwrap_or("-"),
            row["id"].as_str().unwrap_or("-")
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod worker_cli_tests;
