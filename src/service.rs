//! Machine-boot services run Horde as the installing user, never as root.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn unit(s: &str) -> Result<String> {
    ensure!(!s.contains(['\n', '\r', '\0']), "invalid service argument");
    Ok(format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    ))
}
pub fn render(
    platform: &str,
    exe: &Path,
    root: &Path,
    user: &str,
    home: &Path,
    config_home: &Path,
) -> Result<String> {
    let label = "io.horde.daemon";
    let description = "Horde";
    ensure!(
        config_home.is_absolute(),
        "service configuration path must be absolute"
    );
    let config_home = config_home
        .to_str()
        .context("UTF-8 configuration path required")?;
    ensure!(
        !config_home.chars().any(char::is_control),
        "invalid service configuration path"
    );
    ensure!(
        !user.is_empty()
            && user
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c)),
        "invalid service user"
    );
    let exe = exe.to_str().context("UTF-8 executable path required")?;
    let data = root.to_str().context("UTF-8 data path required")?;
    let home = home.to_str().context("UTF-8 home required")?;
    if platform == "macos" {
        Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{label}</string><key>UserName</key><string>{}</string><key>ProgramArguments</key><array><string>{}</string><string>--data-dir</string><string>{}</string><string>daemon</string></array><key>EnvironmentVariables</key><dict><key>HOME</key><string>{}</string><key>XDG_CONFIG_HOME</key><string>{}</string><key>PATH</key><string>{}/.local/bin:{}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin</string></dict><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>StandardOutPath</key><string>{}/daemon.log</string><key>StandardErrorPath</key><string>{}/daemon.log</string></dict></plist>\n",
            xml(user),
            xml(exe),
            xml(data),
            xml(home),
            xml(config_home),
            xml(home),
            xml(home),
            xml(data),
            xml(data)
        ))
    } else if platform == "linux" {
        Ok(format!(
            "[Unit]\nDescription={description} durable runtime\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nUser={user}\nEnvironment={}\nEnvironment={}\nEnvironment={}\nExecStart={} --data-dir {} daemon\nRestart=on-failure\nRestartSec=5\nTimeoutStopSec=60\nUMask=0077\n\n[Install]\nWantedBy=multi-user.target\n",
            unit(&format!("HOME={home}"))?,
            unit(&format!("XDG_CONFIG_HOME={config_home}"))?,
            unit(&format!(
                "PATH={home}/.local/bin:{home}/.cargo/bin:/usr/local/bin:/usr/bin:/bin"
            ))?,
            unit(exe)?,
            unit(data)?
        ))
    } else {
        bail!("boot services support macOS and Linux")
    }
}
fn destination() -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from("/Library/LaunchDaemons/io.horde.daemon.plist")
    } else {
        PathBuf::from("/etc/systemd/system/horde.service")
    }
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    ensure!(
        std::process::Command::new(program)
            .args(args)
            .status()?
            .success(),
        "{program} service operation failed"
    );
    Ok(())
}
pub fn action(root: &Path, action: &str) -> Result<Value> {
    let dest = destination();
    if action == "status" {
        return Ok(json!({"installed":dest.exists(),"definition":dest}));
    }
    if action == "install" {
        ensure!(
            unsafe { libc::geteuid() } != 0,
            "install as the intended runtime user; privilege elevation is limited to service setup"
        );
        let out = std::process::Command::new("id").arg("-un").output()?;
        ensure!(out.status.success(), "cannot determine service user");
        let user = String::from_utf8(out.stdout)?;
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME required")?);
        crate::store::Store::open(root)?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("daemon.log"))?;
        let root = root.canonicalize()?;
        let exe = crate::branding::var_os("HORDE_LAUNCHER")
            .map(PathBuf::from)
            .unwrap_or(std::env::current_exe()?);
        ensure!(exe.is_absolute(), "service executable must be absolute");
        // Resolve relative overrides against the installer's working directory;
        // service managers start from a different directory at boot.
        let config_home = std::path::absolute(
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config")),
        )?;
        let body = render(
            std::env::consts::OS,
            &exe,
            &root,
            user.trim(),
            &home,
            &config_home,
        )?;
        if dest.exists() {
            let existing = std::fs::read_to_string(&dest)?;
            ensure!(
                existing == body,
                "a different Horde service is already installed; uninstall it before replacing its identity or data directory"
            );
            return Ok(json!({"installed":true,"definition":dest}));
        }
        let temp = root.join(format!("service-{}", crate::store::id()));
        crate::secrets::write_private(&temp, body.as_bytes())?;
        let result = run(
            "sudo",
            &[
                "install",
                "-o",
                "root",
                "-m",
                "644",
                temp.to_str().context("path")?,
                dest.to_str().context("path")?,
            ],
        );
        std::fs::remove_file(temp)?;
        result?;
        if cfg!(target_os = "macos") {
            run(
                "sudo",
                &[
                    "launchctl",
                    "bootstrap",
                    "system",
                    dest.to_str().context("path")?,
                ],
            )?;
        } else {
            run("sudo", &["systemctl", "daemon-reload"])?;
            run(
                "sudo",
                &[
                    "systemctl",
                    "enable",
                    "--now",
                    dest.file_name().unwrap().to_str().context("service name")?,
                ],
            )?;
        }
    } else if action == "uninstall" {
        if !dest.exists() {
            return Ok(json!({"installed":false}));
        }
        if cfg!(target_os = "macos") {
            run(
                "sudo",
                &[
                    "launchctl",
                    "bootout",
                    "system",
                    dest.to_str().context("path")?,
                ],
            )?;
        } else {
            run(
                "sudo",
                &[
                    "systemctl",
                    "disable",
                    "--now",
                    dest.file_name().unwrap().to_str().context("service name")?,
                ],
            )?;
        }
        run("sudo", &["rm", dest.to_str().context("path")?])?;
        if cfg!(target_os = "linux") {
            run("sudo", &["systemctl", "daemon-reload"])?;
        }
    } else {
        bail!("unsupported service action");
    }
    crate::management::set(
        &crate::store::Store::open(root)?,
        "service_installed",
        if dest.exists() { "true" } else { "false" },
    )?;
    Ok(json!({"installed":dest.exists(),"definition":dest}))
}
