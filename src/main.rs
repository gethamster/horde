use anyhow::{Context, Result, bail};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    path::PathBuf,
};
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
    },
    Inspect {
        task: String,
    },
    List,
    /// Summarize reported usage, cost, retries and coordination overhead.
    Metrics {
        task: String,
    },
    Events {
        task: String,
        #[arg(long, default_value_t = 0)]
        after: i64,
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
    Runtime {
        #[command(subcommand)]
        command: RuntimeCommands,
    },
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
    List,
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
        #[arg(long)]
        version: String,
        #[arg(long)]
        request_id: String,
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
                for tool in tools {
                    tool["inputSchema"] =
                        horde::protocol::schema(tool["name"].as_str().unwrap_or(""));
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
    let root = cli.data_dir.unwrap_or_else(horde::branding::data_dir);
    std::fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    let output = match cli.command {
        Commands::Network { config, command } => {
            match &command {
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
        Commands::Start => {
            if std::os::unix::net::UnixStream::connect(root.join("daemon.sock")).is_ok() {
                json!({"running":true})
            } else {
                let log = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(root.join("daemon.log"))?;
                let mut cmd = std::process::Command::new(std::env::current_exe()?);
                cmd.arg("--data-dir")
                    .arg(&root)
                    .arg("daemon")
                    .stdin(std::process::Stdio::null())
                    .stdout(log.try_clone()?)
                    .stderr(log);
                use std::os::unix::process::CommandExt;
                unsafe {
                    cmd.pre_exec(|| {
                        if libc::setsid() < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
                let child = cmd.spawn()?;
                let mut ready = false;
                for _ in 0..50 {
                    if std::os::unix::net::UnixStream::connect(root.join("daemon.sock")).is_ok() {
                        ready = true;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                if !ready {
                    bail!(
                        "daemon failed to start; inspect {}",
                        root.join("daemon.log").display()
                    );
                }
                json!({"pid":child.id(),"running":true})
            }
        }
        Commands::Submit {
            objective,
            repo,
            template,
        } => request(
            &root,
            "submit_task",
            json!({"objective":objective,"repo":repo.canonicalize()?,"template":template}),
        )?,
        Commands::Inspect { task } => request(&root, "inspect", json!({"task":task}))?,
        Commands::Metrics { task } => request(&root, "metrics", json!({"task":task}))?,
        Commands::List => request(&root, "list_tasks", json!({}))?,
        Commands::Events { task, after } => {
            request(&root, "events", json!({"task":task,"after":after}))?
        }
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
        Commands::Call { method, args } => request(&root, &method, serde_json::from_str(&args)?)?,
        Commands::Validate {
            template,
            repo,
            objective,
        } => json!(horde::template::compile(
            &template,
            &horde::template::load_templates(&horde::branding::templates(&repo))?,
            std::collections::BTreeMap::from([("task".into(), objective)])
        )?),
        Commands::Doctor {
            repo,
            probe_tuara,
            probe,
            provider,
        } => {
            let settings = horde::config::Settings::load(&repo)?;
            if probe || probe_tuara {
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
            }
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
        Commands::Runtime { command } => {
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
                RuntimeCommands::List => ("runtime_list", json!({})),
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
                    request_id,
                } => (
                    "runtime_update",
                    json!({"id":id,"version":version,"request_id":request_id}),
                ),
                RuntimeCommands::Start { id, request_id } => {
                    ("runtime_start", json!({"id":id,"request_id":request_id}))
                }
                RuntimeCommands::Stop { id, request_id } => {
                    ("runtime_stop", json!({"id":id,"request_id":request_id}))
                }
            };
            request(&root, method, args)?
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
