use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// Provider slug used by every executor role that does not name one.
pub const DEFAULT_PROVIDER: &str = "default";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub concurrency: usize,
    pub autonomy: bool,
    pub default_template: String,
    pub timeout_seconds: u64,
    pub step_budget_seconds: u64,
    pub max_tool_rounds: usize,
    pub max_identical_tool_calls: usize,
    pub tool_event_bytes: usize,
    pub allow_commands: bool,
    pub secret_bundles: Vec<String>,
    pub skills: BTreeMap<String, PathBuf>,
    pub providers: BTreeMap<String, Provider>,
    pub executors: BTreeMap<String, Executor>,
    pub fallbacks: BTreeMap<String, String>,
    pub delivery: Delivery,
    pub limits: crate::delegation::Limits,
}
/// A named endpoint and credential, declared once and shared by executor roles.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Provider {
    pub kind: String,
    pub auth_mode: String,
    pub base_url: String,
    pub api_key_env: String,
    pub account: Option<String>,
    pub program: Option<String>,
    pub model: Option<String>,
    pub max_price: Option<String>,
    pub max_tokens: u64,
    pub max_api_cost_usd: Option<f64>,
    pub extra_body: BTreeMap<String, serde_json::Value>,
    pub stream: bool,
}
/// Deliberately neutral: a provider declared in a config file inherits nothing
/// endpoint- or credential-shaped, so an omitted `base_url` or `api_key_env` is
/// reported rather than silently filled in with another provider's.
impl Default for Provider {
    fn default() -> Self {
        Self {
            kind: String::new(),
            auth_mode: "login".into(),
            base_url: String::new(),
            api_key_env: String::new(),
            account: None,
            program: None,
            model: None,
            max_price: None,
            max_tokens: 8192,
            max_api_cost_usd: None,
            extra_body: BTreeMap::new(),
            stream: false,
        }
    }
}
impl Provider {
    /// The provider's connection, with the role's own model and limits laid over it.
    fn resolve(&self, e: &Executor) -> ExecutorConfig {
        ExecutorConfig {
            kind: self.kind.clone(),
            auth_mode: self.auth_mode.clone(),
            base_url: self.base_url.clone(),
            api_key_env: self.api_key_env.clone(),
            account: e.account.clone().or_else(|| self.account.clone()),
            program: e.program.clone().or_else(|| self.program.clone()),
            model: e.model.clone().or_else(|| self.model.clone()),
            max_price: e.max_price.clone().or_else(|| self.max_price.clone()),
            max_tokens: e.max_tokens.unwrap_or(self.max_tokens),
            max_api_cost_usd: e.max_api_cost_usd.or(self.max_api_cost_usd),
            extra_body: self
                .extra_body
                .iter()
                .chain(e.extra_body.iter())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            stream: self.stream,
        }
    }
}
/// An executor role: which provider it uses, plus the per-role knobs. A role cannot
/// restate the connection — kind, auth_mode, base_url and api_key_env travel together
/// on the provider, so a role can never pair one provider's kind with another's key.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Executor {
    pub step_budget_seconds: Option<u64>,
    pub provider: Option<String>,
    pub account: Option<String>,
    pub program: Option<String>,
    pub model: Option<String>,
    pub max_price: Option<String>,
    pub max_tokens: Option<u64>,
    pub max_api_cost_usd: Option<f64>,
    pub extra_body: BTreeMap<String, serde_json::Value>,
}
impl Executor {
    pub fn provider(&self) -> &str {
        self.provider.as_deref().unwrap_or(DEFAULT_PROVIDER)
    }
    fn using(provider: &str) -> Self {
        Self {
            provider: Some(provider.into()),
            ..Default::default()
        }
    }
}
/// An executor role resolved against its provider; what every caller executes with.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExecutorConfig {
    pub kind: String,
    pub account: Option<String>,
    pub auth_mode: String,
    pub program: Option<String>,
    pub model: Option<String>,
    pub base_url: String,
    pub api_key_env: String,
    pub max_price: Option<String>,
    pub max_tokens: u64,
    pub max_api_cost_usd: Option<f64>,
    pub extra_body: BTreeMap<String, serde_json::Value>,
    pub stream: bool,
}
impl Default for ExecutorConfig {
    fn default() -> Self {
        Provider::default().resolve(&Executor::default())
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Delivery {
    pub program: Option<String>,
    pub enabled: bool,
    pub repository: String,
    pub base: String,
    pub merge: bool,
    pub deploy_workflow: Option<String>,
    pub health_url: Option<String>,
}
impl Default for Settings {
    fn default() -> Self {
        let providers = BTreeMap::from([
            (
                DEFAULT_PROVIDER.into(),
                Provider {
                    kind: "tuara".into(),
                    auth_mode: "api".into(),
                    base_url: "https://tuara.com/router/v1".into(),
                    api_key_env: "TUARA_API_KEY".into(),
                    model: Some("qwen/qwen3.8-27b".into()),
                    ..Default::default()
                },
            ),
            (
                "codex".into(),
                Provider {
                    kind: "codex".into(),
                    base_url: "https://api.openai.com/v1".into(),
                    api_key_env: "OPENAI_API_KEY".into(),
                    ..Default::default()
                },
            ),
            (
                "claude".into(),
                Provider {
                    kind: "claude".into(),
                    base_url: "https://api.anthropic.com/v1".into(),
                    api_key_env: "ANTHROPIC_API_KEY".into(),
                    ..Default::default()
                },
            ),
            (
                "simulated".into(),
                Provider {
                    kind: "simulated".into(),
                    ..Default::default()
                },
            ),
        ]);
        let executors = BTreeMap::from([
            (
                "planner".into(),
                Executor {
                    step_budget_seconds: Some(600),
                    ..Default::default()
                },
            ),
            ("worker".into(), Executor::default()),
            (
                "reviewer".into(),
                Executor {
                    step_budget_seconds: Some(600),
                    ..Default::default()
                },
            ),
            ("native".into(), Executor::default()),
            ("codex".into(), Executor::using("codex")),
            ("claude".into(), Executor::using("claude")),
            ("simulated".into(), Executor::using("simulated")),
        ]);
        Self {
            concurrency: 4,
            autonomy: true,
            default_template: "local-implementation".into(),
            timeout_seconds: 1800,
            step_budget_seconds: 1800,
            max_tool_rounds: 64,
            max_identical_tool_calls: 3,
            tool_event_bytes: 512,
            allow_commands: true,
            secret_bundles: vec![],
            skills: BTreeMap::new(),
            providers,
            executors,
            fallbacks: BTreeMap::new(),
            delivery: Delivery::default(),
            limits: Default::default(),
        }
    }
}
/// Written by `horde config init`; must parse back to the built-in defaults.
pub const STARTER: &str = r#"# Horde settings. Every value here is a built-in default, shown so it can be
# changed in place. Delete anything you do not need; omitted keys keep their
# default. A repository may override any of it in its own .horde.toml.

concurrency = 4
autonomy = true
default_template = "local-implementation"
timeout_seconds = 1800
step_budget_seconds = 1800 # Time without durable progress, per attempt
max_tool_rounds = 64
max_identical_tool_calls = 3 # 0 disables repeated-call detection
tool_event_bytes = 512 # Per arguments/result field; 0 omits payloads
allow_commands = true

# Optional skill directories, captured when a task is submitted. Select their
# names in step.skills. Use absolute paths for skills outside the repository.
[skills]
# report = ".agents/skills/report"

# Providers are declared once and referenced by name. Put the API key itself in
# ~/.config/horde/credentials.env (mode 0600) or the daemon environment, never
# here: only the name of the variable belongs in this file.
[providers.default]
kind = "tuara"
auth_mode = "api"
base_url = "https://tuara.com/router/v1"
api_key_env = "TUARA_API_KEY"
model = "qwen/qwen3.8-27b"
max_tokens = 8192
# max_price = "1.00"  # Ceiling in dollars per million tokens, not a total budget.

# Subscription login through the installed Codex CLI. Set auth_mode = "api" to
# use OPENAI_API_KEY through the loopback broker instead.
[providers.codex]
kind = "codex"
auth_mode = "login"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"

# Subscription login through the installed Claude Code CLI. Set auth_mode = "api"
# to use ANTHROPIC_API_KEY through the loopback broker instead.
[providers.claude]
kind = "claude"
auth_mode = "login"
base_url = "https://api.anthropic.com/v1"
api_key_env = "ANTHROPIC_API_KEY"

[providers.simulated]
kind = "simulated"

# Executor roles. Each role picks a provider and a model; a role that names no
# provider uses providers.default, and a role that names no model uses that
# provider's. Connection settings are never restated here — change the provider,
# or declare another one, to send a role somewhere else.
#
#   [executors.reviewer]
#   provider = "default"
#   model = "a-stronger-model"
#
[executors.planner]
step_budget_seconds = 600
[executors.worker]
[executors.reviewer]
step_budget_seconds = 600
[executors.native]

[executors.codex]
provider = "codex"

[executors.claude]
provider = "claude"

[executors.simulated]
provider = "simulated"

# [fallbacks]
# worker = "codex"

# [delivery]
# enabled = true
# repository = "owner/name"
# base = "main"
"#;
/// Connection settings used to be restated on every executor role. Say where they went,
/// rather than letting `deny_unknown_fields` report a bare unknown field.
fn moved_to_provider(value: &toml::Value) -> Result<()> {
    let Some(executors) = value.get("executors").and_then(toml::Value::as_table) else {
        return Ok(());
    };
    for (role, table) in executors {
        for key in ["kind", "auth_mode", "base_url", "api_key_env"] {
            if table.get(key).is_some() {
                bail!(
                    "executor role {role} sets {key}, which now belongs to a provider. \
                     Declare it once under [providers.<name>] and point the role at it \
                     with provider = \"<name>\"."
                );
            }
        }
    }
    Ok(())
}
fn merge(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(a), toml::Value::Table(b)) => {
            for (k, v) in b {
                if let Some(old) = a.get_mut(&k) {
                    merge(old, v);
                } else {
                    a.insert(k, v);
                }
            }
        }
        (a, b) => *a = b,
    }
}
impl Settings {
    pub fn user_path() -> PathBuf {
        crate::branding::config_dir().join("config.toml")
    }
    pub fn credentials_path() -> PathBuf {
        crate::branding::config_dir().join("credentials.env")
    }
    pub fn load_user() -> Result<Self> {
        Self::load_dir(&crate::branding::config_dir())
    }
    /// Load as `load_user` does, but from a stated configuration directory.
    pub fn load_dir(directory: &Path) -> Result<Self> {
        Self::load_files(&[directory.join("config.toml")])
    }
    pub fn load(project: &Path) -> Result<Self> {
        Self::load_files(&[Self::user_path(), crate::branding::project_config(project)])
    }
    /// The role resolved against its provider, or `None` when the role is not configured.
    pub fn executor(&self, role: &str) -> Option<ExecutorConfig> {
        let executor = self.executors.get(role)?;
        Some(self.providers.get(executor.provider())?.resolve(executor))
    }
    pub fn provider(&self, name: &str) -> Option<ExecutorConfig> {
        Some(self.providers.get(name)?.resolve(&Executor::default()))
    }
    /// Every configured role, resolved. Roles naming a missing provider are omitted;
    /// `load` rejects those, so they only arise from settings injected by a peer.
    pub fn resolved(&self) -> BTreeMap<String, ExecutorConfig> {
        self.executors
            .keys()
            .filter_map(|role| Some((role.clone(), self.executor(role)?)))
            .collect()
    }
    fn load_files(files: &[PathBuf]) -> Result<Self> {
        let mut value = toml::Value::try_from(Self::default())?;
        for file in files {
            if file.exists() {
                merge(
                    &mut value,
                    toml::from_str::<toml::Value>(&std::fs::read_to_string(file)?)
                        .with_context(|| format!("invalid {}", file.display()))?,
                );
            }
        }
        moved_to_provider(&value)?;
        let mut settings: Self = value.try_into()?;
        // An auto catalog plus an endpoint and key variable fully identifies
        // the native connection; no additional kind tag is needed.
        for provider in settings.providers.values_mut() {
            if provider.kind.is_empty()
                && provider.model.as_deref() == Some("auto")
                && !provider.base_url.is_empty()
                && !provider.api_key_env.is_empty()
            {
                provider.kind = "tuara".into();
                provider.auth_mode = "api".into();
            }
        }
        settings.validate()?;
        Ok(settings)
    }
    fn validate(&self) -> Result<()> {
        if self.concurrency == 0
            || self.concurrency > 64
            || self.timeout_seconds == 0
            || self.max_tool_rounds == 0
        {
            bail!("invalid concurrency, timeout, or tool round limit");
        }
        if self.step_budget_seconds == 0 {
            bail!("step_budget_seconds must be positive");
        }
        if self.tool_event_bytes > 65536 {
            bail!("tool_event_bytes must be between 0 and 65536");
        }
        for (slug, provider) in &self.providers {
            crate::native_protocol::validate_extra_body(&provider.extra_body)
                .map_err(|e| anyhow::anyhow!("provider {slug}: {e}"))?;
            if provider.kind != "tuara" && !provider.extra_body.is_empty() {
                bail!("provider {slug}: extra_body is supported only by the native tuara executor");
            }
            if provider.kind.is_empty() {
                bail!("provider {slug} needs a kind");
            }
            if !["login", "api"].contains(&provider.auth_mode.as_str()) {
                bail!("provider {slug} auth_mode must be login or api");
            }
            // Only these reach a provider endpoint directly; the CLI harnesses under
            // subscription login use their own installed credential store.
            if provider.auth_mode == "api" || provider.kind == "tuara" {
                if provider.base_url.is_empty() {
                    bail!("provider {slug} needs a base_url");
                }
                if provider.api_key_env.is_empty() {
                    bail!("provider {slug} needs an api_key_env naming the variable to read");
                }
            }
        }
        for (role, executor) in &self.executors {
            if executor.step_budget_seconds == Some(0) {
                bail!("executor role {role}: step_budget_seconds must be positive");
            }
            crate::native_protocol::validate_extra_body(&executor.extra_body)
                .map_err(|e| anyhow::anyhow!("executor role {role}: {e}"))?;
            if !executor.extra_body.is_empty()
                && self
                    .providers
                    .get(executor.provider())
                    .is_some_and(|p| p.kind != "tuara")
            {
                bail!(
                    "executor role {role}: extra_body is supported only by the native tuara executor"
                );
            }
            if !self.providers.contains_key(executor.provider()) {
                bail!(
                    "executor role {role} names provider {}, which is not configured",
                    executor.provider()
                );
            }
        }
        for (role, target) in &self.fallbacks {
            if !self.executors.contains_key(role) || !self.executors.contains_key(target) {
                bail!("fallback refers to an unconfigured executor role");
            }
            let mut seen = std::collections::BTreeSet::new();
            let mut current = role;
            while let Some(next) = self.fallbacks.get(current) {
                if !seen.insert(current) {
                    bail!("executor fallback cycle");
                }
                current = next;
            }
        }
        Ok(())
    }
}

/// Daemon-side credentials for boot services; this file is never sent to workers.
pub fn credential(name: &str) -> Result<String> {
    if let Ok(value) = std::env::var(name) {
        return Ok(value);
    }
    let path = Settings::credentials_path();
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(&path)
        .with_context(|| format!("missing {name} in daemon environment or credentials.env"))?;
    anyhow::ensure!(
        metadata.permissions().mode() & 0o077 == 0,
        "credentials.env must have mode 0600"
    );
    crate::secrets::parse(&std::fs::read_to_string(path)?)?
        .remove(name)
        .with_context(|| format!("credential {name} is not configured"))
}

/// Write the starter configuration and an empty credentials file into `directory`.
/// Never replaces a file that already exists; returns the paths actually created.
pub fn initialize(directory: &Path) -> Result<Vec<PathBuf>> {
    std::fs::create_dir_all(directory)?;
    let config = directory.join("config.toml");
    let mut created = vec![];
    if !config.exists() {
        crate::secrets::write_private(&config, STARTER.as_bytes())?;
        created.push(config);
    }
    let credentials = directory.join("credentials.env");
    if !credentials.exists() {
        crate::secrets::write_private(
            &credentials,
            concat!(
                "# Daemon-only provider credentials, one NAME='value' per line.\n",
                "# The name must match api_key_env for a provider in config.toml.\n",
                "# TUARA_API_KEY=''\n"
            )
            .as_bytes(),
        )?;
        created.push(credentials);
    }
    Ok(created)
}
