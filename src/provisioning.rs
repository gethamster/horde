//! Adding a provider and its key from the CLI. Edits are surgical: `config.toml`
//! keeps its comments and hand-written stanzas, and the key only ever reaches
//! `credentials.env`, never the settings file and never a command-line argument.
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::{BufRead, Write},
    path::Path,
};

/// A ready-made provider, so the common cases need no endpoint or variable name.
pub struct Preset {
    pub name: &'static str,
    pub kind: &'static str,
    pub auth_mode: &'static str,
    pub base_url: &'static str,
    pub api_key_env: &'static str,
    pub model: Option<&'static str>,
    pub summary: &'static str,
}
pub const PRESETS: &[Preset] = &[
    Preset {
        name: "tuara",
        kind: "tuara",
        auth_mode: "api",
        base_url: "https://tuara.com/router/v1",
        api_key_env: "TUARA_API_KEY",
        model: Some("qwen/qwen3.8-27b"),
        summary: "Tuara router, Horde's own tool loop over an API key",
    },
    Preset {
        name: "codex",
        kind: "codex",
        auth_mode: "login",
        base_url: "https://api.openai.com/v1",
        api_key_env: "OPENAI_API_KEY",
        model: None,
        summary: "Installed Codex CLI on its own subscription login",
    },
    Preset {
        name: "claude",
        kind: "claude",
        auth_mode: "login",
        base_url: "https://api.anthropic.com/v1",
        api_key_env: "ANTHROPIC_API_KEY",
        model: None,
        summary: "Installed Claude Code CLI on its own subscription login",
    },
    Preset {
        name: "openai",
        kind: "codex",
        auth_mode: "api",
        base_url: "https://api.openai.com/v1",
        api_key_env: "OPENAI_API_KEY",
        model: None,
        summary: "Codex CLI against the OpenAI API with a key",
    },
    Preset {
        name: "anthropic",
        kind: "claude",
        auth_mode: "api",
        base_url: "https://api.anthropic.com/v1",
        api_key_env: "ANTHROPIC_API_KEY",
        model: None,
        summary: "Claude Code CLI against the Anthropic API with a key",
    },
];
pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name)
}
/// What `provider add` is being asked to write. Every field is optional so an
/// existing provider can be adjusted one setting at a time.
#[derive(Clone, Default)]
pub struct Spec {
    pub name: String,
    pub kind: Option<String>,
    pub auth_mode: Option<String>,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub model: Option<String>,
    pub roles: Vec<String>,
}
impl Spec {
    /// Fill anything unstated from the preset of the same name, when there is one.
    pub fn with_preset(mut self, preset: &Preset) -> Self {
        self.kind.get_or_insert_with(|| preset.kind.into());
        self.auth_mode
            .get_or_insert_with(|| preset.auth_mode.into());
        self.base_url.get_or_insert_with(|| preset.base_url.into());
        self.api_key_env
            .get_or_insert_with(|| preset.api_key_env.into());
        if let Some(model) = preset.model {
            self.model.get_or_insert_with(|| model.into());
        }
        self
    }
}
fn identifier(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 48
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !name.starts_with('-'),
        "provider name must be 1-48 lowercase letters, digits or hyphens"
    );
    Ok(())
}
/// Replace a file's contents while keeping it private and never leaving a partial
/// file behind: write a sibling temporary, then rename over the original.
fn replace_private(path: &Path, contents: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let temporary = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));
    let _ = std::fs::remove_file(&temporary);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    drop(file);
    if let Ok(metadata) = std::fs::metadata(path) {
        std::fs::set_permissions(&temporary, metadata.permissions())?;
    }
    std::fs::rename(&temporary, path)?;
    Ok(())
}
/// Write the provider stanza, and point any named roles at it. Keys already in the
/// file that the spec does not mention are left exactly as they are.
fn write_provider(config: &Path, spec: &Spec) -> Result<()> {
    let mut document = std::fs::read_to_string(config)?
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("invalid {}", config.display()))?;
    let providers = document["providers"].or_insert(toml_edit::table());
    ensure!(
        providers.is_table_like(),
        "providers must be a table in {}",
        config.display()
    );
    let entry = providers[spec.name.as_str()].or_insert(toml_edit::table());
    for (key, value) in [
        ("kind", &spec.kind),
        ("auth_mode", &spec.auth_mode),
        ("base_url", &spec.base_url),
        ("api_key_env", &spec.api_key_env),
        ("model", &spec.model),
    ] {
        if let Some(value) = value {
            entry[key] = toml_edit::value(value.clone());
        }
    }
    for role in &spec.roles {
        let executors = document["executors"].or_insert(toml_edit::table());
        ensure!(
            executors.is_table_like(),
            "executors must be a table in {}",
            config.display()
        );
        let role_entry = executors[role.as_str()].or_insert(toml_edit::table());
        role_entry["provider"] = toml_edit::value(spec.name.clone());
    }
    replace_private(config, &document.to_string())
}
/// Set one variable in `credentials.env`, keeping the comments and other entries.
fn write_credential(path: &Path, name: &str, key: &str) -> Result<()> {
    ensure!(
        !key.is_empty() && !key.contains(['\n', '\r', '\0']),
        "an API key cannot be empty or contain a line break"
    );
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = vec![];
    let mut replaced = false;
    for line in existing.lines() {
        let trimmed = line.trim().trim_start_matches("export ");
        let matches = trimmed
            .split_once('=')
            .is_some_and(|(candidate, _)| candidate.trim() == name);
        if matches && !trimmed.starts_with('#') {
            lines.push(format!("{name}='{key}'"));
            replaced = true;
        } else {
            lines.push(line.to_owned());
        }
    }
    if !replaced {
        lines.push(format!("{name}='{key}'"));
    }
    let mut contents = lines.join("\n");
    contents.push('\n');
    // Reject anything the daemon's own parser would refuse, before it is on disk.
    crate::secrets::parse(&contents).context("refusing to write an unreadable credentials.env")?;
    replace_private(path, &contents)
}
/// Read a key that was piped in, for `--key-stdin`. Never a command-line argument:
/// those reach `ps` and the shell history.
pub fn read_piped_key() -> Result<String> {
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_owned())
}
/// The controlling terminal, opened directly rather than through stdin, so prompts
/// still reach the person running `curl ... | sh` — where stdin is the script.
pub struct Terminal {
    reader: std::io::BufReader<std::fs::File>,
    writer: std::fs::File,
}
impl Terminal {
    pub fn open() -> Option<Self> {
        Self::on(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")
                .ok()?,
        )
    }
    /// Prompt on an already-open terminal. Tests hand this the slave side of a pty.
    pub fn on(terminal: std::fs::File) -> Option<Self> {
        Some(Self {
            reader: std::io::BufReader::new(terminal.try_clone().ok()?),
            writer: terminal,
        })
    }
    fn say(&mut self, text: &str) -> Result<()> {
        write!(self.writer, "{text}")?;
        self.writer.flush()?;
        Ok(())
    }
    fn line(&mut self) -> Result<String> {
        let mut line = String::new();
        ensure!(self.reader.read_line(&mut line)? > 0, "input ended");
        Ok(line.trim().to_owned())
    }
    fn ask(&mut self, prompt: &str, default: &str) -> Result<String> {
        if default.is_empty() {
            self.say(&format!("{prompt}: "))?;
        } else {
            self.say(&format!("{prompt} [{default}]: "))?;
        }
        let line = self.line()?;
        Ok(if line.is_empty() {
            default.to_owned()
        } else {
            line
        })
    }
    pub fn confirm(&mut self, prompt: &str, default_yes: bool) -> Result<bool> {
        let answer = self.ask(prompt, if default_yes { "Y/n" } else { "y/N" })?;
        Ok(match answer.to_ascii_lowercase().as_str() {
            "y" | "yes" => true,
            "n" | "no" => false,
            _ => default_yes,
        })
    }
    /// Read a secret with the terminal's echo turned off. Echo goes off *before* the
    /// prompt is written: a key typed or pasted ahead of the prompt must not appear
    /// either, and turning it off afterwards races the first keystroke.
    pub fn secret(&mut self, prompt: &str) -> Result<String> {
        let guard = EchoOff::new(&self.writer)?;
        self.say(prompt)?;
        let key = self.line();
        drop(guard);
        self.say("\n")?;
        key
    }
}
/// Turns terminal echo off for as long as it is alive, restoring it on every exit
/// path so an error or a Ctrl-C mid-prompt cannot leave the terminal mute.
struct EchoOff(libc::termios, std::os::fd::RawFd);
impl EchoOff {
    fn new(terminal: &std::fs::File) -> Result<Self> {
        use std::os::fd::AsRawFd;
        let fd = terminal.as_raw_fd();
        // SAFETY: both calls take a pointer to a termios this function owns, and a
        // descriptor borrowed from a file that outlives the returned guard.
        unsafe {
            let mut settings: libc::termios = std::mem::zeroed();
            ensure!(
                libc::tcgetattr(fd, &mut settings) == 0,
                "cannot read terminal settings"
            );
            let restore = settings;
            settings.c_lflag &= !libc::ECHO;
            ensure!(
                libc::tcsetattr(fd, libc::TCSANOW, &settings) == 0,
                "cannot silence the terminal for a secret"
            );
            Ok(Self(restore, fd))
        }
    }
}
impl Drop for EchoOff {
    fn drop(&mut self) {
        // SAFETY: restores the settings read in `new` on the same descriptor.
        unsafe {
            libc::tcsetattr(self.1, libc::TCSANOW, &self.0);
        }
    }
}
/// Walk through adding providers until the person says they are done. Each one is
/// written as it is confirmed, so an interruption keeps whatever already worked.
pub fn interactive(directory: &Path, terminal: &mut Terminal) -> Result<Vec<String>> {
    let mut added = vec![];
    loop {
        terminal.say("\nProviders you can set up:\n")?;
        for p in PRESETS {
            terminal.say(&format!("  {:<10} {}\n", p.name, p.summary))?;
        }
        terminal.say(&format!(
            "  {:<10} anything else, described by hand\n\n",
            "custom"
        ))?;
        match one(directory, terminal) {
            Ok(summary) => {
                terminal.say(&format!("{summary}\n"))?;
                added.push(summary);
            }
            // A mistyped answer should cost one provider, not the whole walkthrough.
            Err(error) => terminal.say(&format!("Not written: {error}\n"))?,
        }
        if !terminal.confirm("\nAdd another provider?", false)? {
            return Ok(added);
        }
    }
}
fn one(directory: &Path, terminal: &mut Terminal) -> Result<String> {
    let chosen = terminal.ask("Provider", "tuara")?;
    let mut spec = Spec {
        name: chosen.clone(),
        ..Default::default()
    };
    if let Some(preset) = preset(&chosen) {
        spec = spec.with_preset(preset);
    } else {
        spec.name = terminal.ask("Name for this provider", "")?;
        spec.kind = Some(terminal.ask("Kind (tuara, codex, claude, simulated)", "tuara")?);
        spec.auth_mode = Some(terminal.ask("Auth mode (api, login)", "api")?);
        spec.base_url = Some(terminal.ask("Base URL", "")?);
        spec.api_key_env = Some(terminal.ask("Environment variable holding the key", "")?);
    }
    identifier(&spec.name)?;
    let model = terminal.ask(
        "Model (blank to leave unset)",
        spec.model.as_deref().unwrap_or(""),
    )?;
    spec.model = (!model.is_empty()).then_some(model);
    let roles = terminal.ask(
        "Roles to use it, comma separated (blank for none)",
        "planner,worker,reviewer",
    )?;
    spec.roles = roles
        .split(',')
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(str::to_owned)
        .collect();
    let key = if spec.auth_mode.as_deref() == Some("login") {
        terminal
            .say("Subscription login: this uses the CLI's own store, so no key is needed.\n")?;
        None
    } else {
        let variable = spec.api_key_env.clone().unwrap_or_default();
        // Coming back to change a model should not mean typing the key again.
        let stored = crate::config::credential(&variable).is_ok();
        if stored
            && !terminal.confirm(&format!("{variable} is already stored. Replace it?"), false)?
        {
            None
        } else {
            let key = terminal.secret(&format!("{variable} (input hidden): "))?;
            (!key.is_empty()).then_some(key)
        }
    };
    apply(directory, &spec, key)
}
/// Picking a preset means "set up my Tuara", not "give me a second one". When a
/// configured provider already points at the same endpoint with the same harness,
/// that is the one to write to — otherwise `add tuara` would leave a twin of the
/// shipped `default` behind, with the roles still on the old one.
fn existing_equivalent(settings: &crate::config::Settings, spec: &Spec) -> Option<String> {
    if settings.providers.contains_key(&spec.name) || preset(&spec.name).is_none() {
        return None;
    }
    settings
        .providers
        .iter()
        .find(|(_, p)| {
            Some(&p.kind) == spec.kind.as_ref() && Some(&p.base_url) == spec.base_url.as_ref()
        })
        .map(|(name, _)| name.clone())
}
/// Write the provider and, when one was supplied, its key. Returns a summary line.
pub fn apply(directory: &Path, spec: &Spec, key: Option<String>) -> Result<String> {
    identifier(&spec.name)?;
    let config = directory.join("config.toml");
    if !config.exists() {
        crate::config::initialize(directory)?;
    }
    let mut spec = Spec { ..spec.clone() };
    if let Some(name) = crate::config::Settings::load_dir(directory)
        .ok()
        .and_then(|settings| existing_equivalent(&settings, &spec))
    {
        spec.name = name;
    }
    let spec = &spec;
    write_provider(&config, spec)?;
    let mut summary = format!("Wrote [providers.{}] to {}.", spec.name, config.display());
    if let Some(key) = key {
        let variable = spec
            .api_key_env
            .clone()
            .context("a key needs an api_key_env to store it under")?;
        let credentials = directory.join("credentials.env");
        write_credential(&credentials, &variable, &key)?;
        summary.push_str(&format!(
            "\nStored {variable} in {}.",
            credentials.display()
        ));
    }
    if !spec.roles.is_empty() {
        summary.push_str(&format!("\nPointed {} at it.", spec.roles.join(", ")));
    }
    // Prove the daemon can still load what was just written.
    crate::config::Settings::load_dir(directory)
        .context("the provider was written but the settings no longer load")?;
    Ok(summary)
}
/// Every configured provider, and whether its key is actually readable.
pub fn list(directory: &Path) -> Result<Vec<serde_json::Value>> {
    let settings = crate::config::Settings::load_dir(directory)?;
    let roles = settings.executors.iter().fold(
        std::collections::BTreeMap::<String, Vec<String>>::new(),
        |mut acc, (role, executor)| {
            acc.entry(executor.provider().to_owned())
                .or_default()
                .push(role.clone());
            acc
        },
    );
    Ok(settings
        .providers
        .iter()
        .map(|(name, provider)| {
            let needs_key = provider.auth_mode == "api" || provider.kind == "tuara";
            serde_json::json!({
                "provider": name,
                "kind": provider.kind,
                "auth_mode": provider.auth_mode,
                "model": provider.model,
                "api_key_env": provider.api_key_env,
                "credential": if !needs_key {
                    "not required"
                } else if crate::config::credential(&provider.api_key_env).is_ok() {
                    "present"
                } else {
                    "missing"
                },
                "roles": roles.get(name).cloned().unwrap_or_default(),
            })
        })
        .collect())
}
/// Ask a provider's endpoint what models it will accept, using its configured key.
pub async fn models(directory: &Path, name: &str) -> Result<Vec<String>> {
    let settings = crate::config::Settings::load_dir(directory)?;
    let provider = settings
        .providers
        .get(name)
        .with_context(|| format!("provider {name} is not configured"))?;
    if provider.base_url.is_empty() {
        bail!("provider {name} has no base_url to ask");
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let mut request = client.get(format!(
        "{}/models",
        provider.base_url.trim_end_matches('/')
    ));
    // Public catalogues answer unauthenticated; send the key only when there is one.
    if let Ok(key) = crate::config::credential(&provider.api_key_env) {
        request = request.bearer_auth(key);
    }
    let response = request.send().await?;
    ensure!(
        response.status().is_success(),
        "{name} returned {} listing models",
        response.status()
    );
    let body: serde_json::Value = response.json().await?;
    Ok(body["data"]
        .as_array()
        .context("model listing has no data array")?
        .iter()
        .filter_map(|m| m["id"].as_str().map(str::to_owned))
        .collect())
}
