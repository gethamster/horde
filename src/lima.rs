//! Project-owned Lima guests. Host UID filtering, rather than guest firewall rules,
//! enforces egress. Native execution has no dependency on Lima or this helper.
use crate::{fleet::Profile, store::Store};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::io::AsyncWriteExt;

mod ownership;
use ownership::{claim_resource, sync_directories};
pub use ownership::{owned_resource, resource_name, validate_socket_path};

/// Print this bundled helper for an administrator to install in a protected path.
pub fn guard_script() -> &'static str {
    include_str!("../scripts/horde-lima-guard.py")
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

pub fn validate(p: &Profile) -> Result<()> {
    ensure!(identifier(&p.project), "invalid Lima project ID");
    ensure!(
        identifier(&p.lima_user) && p.lima_user != "root",
        "dedicated non-root Lima host user required"
    );
    ensure!(
        p.lima_home.is_absolute()
            && !p
                .lima_home
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
        "absolute Lima home required"
    );
    ensure!(
        p.lima_guard.is_absolute() && p.lima_horde_binary.is_absolute(),
        "absolute Lima guard and Linux Horde binary paths required"
    );
    let digest = p
        .lima_image_digest
        .strip_prefix("sha256:")
        .context("Lima image SHA-256 digest required")?;
    ensure!(
        digest.len() == 64 && digest.bytes().all(|c| c.is_ascii_hexdigit()),
        "invalid Lima image digest"
    );
    ensure!(
        !p.lima_egress.is_empty(),
        "explicit Lima egress allowlist required"
    );
    for entry in &p.lima_egress {
        cidr(entry)?;
    }
    Ok(())
}

fn cidr(value: &str) -> Result<(std::net::IpAddr, u8)> {
    let (address, prefix) = value
        .split_once('/')
        .context("egress must be an IP CIDR, not a hostname")?;
    let address: std::net::IpAddr = address.parse()?;
    let prefix: u8 = prefix.parse()?;
    ensure!(
        prefix <= if address.is_ipv4() { 32 } else { 128 },
        "invalid egress CIDR prefix"
    );
    Ok((address, prefix))
}

fn includes(cidr_value: &str, ip: std::net::IpAddr) -> Result<bool> {
    let (network, bits) = cidr(cidr_value)?;
    Ok(match (network, ip) {
        (std::net::IpAddr::V4(n), std::net::IpAddr::V4(i)) => {
            bits == 0 || (u32::from(n) >> (32 - bits)) == (u32::from(i) >> (32 - bits))
        }
        (std::net::IpAddr::V6(n), std::net::IpAddr::V6(i)) => {
            bits == 0 || (u128::from(n) >> (128 - bits)) == (u128::from(i) >> (128 - bits))
        }
        _ => false,
    })
}

pub fn configuration(p: &Profile, id: &str, os: &str, arch: &str) -> Result<Value> {
    validate(p)?;
    ensure!(identifier(id), "invalid Lima runtime ID");
    let vm_type = match os {
        "macos" => "vz",
        "linux" => "qemu",
        _ => bail!("Lima requires macOS or Linux (native WSL does not require Lima)"),
    };
    ensure!(
        ["aarch64", "x86_64"].contains(&arch),
        "unsupported Lima host architecture"
    );
    let provision_script = r#"#!/bin/sh
set -eu
command -v apt-get >/dev/null
export DEBIAN_FRONTEND=noninteractive
retry_apt() {
    attempt=1
    until apt-get -o Acquire::Retries=3 -o Acquire::http::Timeout=20 -o Acquire::https::Timeout=20 -o APT::Update::Error-Mode=any "$@"; do
        if [ "$attempt" -ge 3 ]; then return 1; fi
        attempt=$((attempt + 1))
        sleep 5
    done
}
retry_apt update
retry_apt install -y docker.io docker-compose-v2 ca-certificates git
usermod -aG docker horde
systemctl enable --now docker
install -d -o horde -g horde -m 0700 /var/lib/horde
"#;
    Ok(json!({
        "vmType":vm_type, "arch":arch, "cpus":p.cpus,
        "memory":format!("{}MiB",p.memory_mb), "disk":format!("{}GiB",p.disk_gb),
        "images":[{"location":p.image,"arch":arch,"digest":p.lima_image_digest}],
        "mounts":[], "ssh":{"forwardAgent":false,"forwardX11":false,"forwardX11Trusted":false,"loadDotSSHPubKeys":false},
        "portForwards":[{"guestIP":"0.0.0.0", "guestIPMustBeZero":false,"guestPortRange":[1,65535],"ignore":true},{"guestIP":"::","guestIPMustBeZero":false,"guestPortRange":[1,65535],"ignore":true}],
        "containerd":{"system":false,"user":false}, "hostResolver":{"enabled":false},
        "networks":[{"lima":"user-v2"}],
        "user":{"name":"horde","uid":10001,"home":"/home/horde","shell":"/bin/bash"},
        "provision":[{"mode":"system","script":provision_script}],
        "probes":[{"script":"#!/bin/sh\nset -eu\ncommand -v docker >/dev/null\nsudo docker compose version >/dev/null\nsudo systemctl is-active --quiet docker\nsudo test -d /var/lib/horde\nsudo docker info >/dev/null\n","hint":"Docker, Compose, and the Horde state directory must be ready. Inspect /var/log/cloud-init-output.log in the guest for package installation failures."}]
    }))
}

/// Immutable controller-owned intent, separate from Lima's host-user-owned disk.
pub fn prepare(root: &Path, p: &Profile, id: &str, os: &str, arch: &str) -> Result<PathBuf> {
    let config = configuration(p, id, os, arch)?;
    let path = root.join("projects").join(&p.project).join("lima").join(id);
    std::fs::create_dir_all(&path)?;
    let mut intent = json!({"project":p.project,"runtime":id,"profile":p,"config":config});
    let manifest = path.join("ownership.json");
    if manifest.exists() {
        let metadata = std::fs::symlink_metadata(&manifest)?;
        ensure!(metadata.is_file(), "Lima ownership must be a regular file");
        let mut prior: Value = serde_json::from_slice(&std::fs::read(&manifest)?)?;
        let resource = owned_resource(root, p, id)?;
        // Program upgrades must not rewrite or invalidate a guest's recorded intent.
        // Retried provisioning uses this same immutable configuration below.
        ensure!(
            prior["config"].is_object(),
            "Lima stored configuration must be an object"
        );
        intent["config"] = prior["config"].clone();
        prior
            .as_object_mut()
            .context("Lima ownership object required")?
            .remove("resource");
        ensure!(
            prior == intent,
            "Lima project, resource and profile ownership are immutable"
        );
        claim_resource(root, p, id, &resource)?;
    } else {
        let resource = resource_name(p, id)?;
        claim_resource(root, p, id, &resource)?;
        intent["resource"] = json!(resource);
        crate::secrets::write_private(&manifest, intent.to_string().as_bytes())?;
    }
    sync_directories(root, &path)?;
    Ok(path)
}

pub fn stored_configuration(owned: &Path) -> Result<Value> {
    let manifest = owned.join("ownership.json");
    ensure!(
        std::fs::symlink_metadata(&manifest)?.is_file(),
        "Lima ownership must be a regular file"
    );
    let intent: Value = serde_json::from_slice(&std::fs::read(manifest)?)?;
    ensure!(
        intent["config"].is_object(),
        "Lima stored configuration must be an object"
    );
    Ok(intent["config"].clone())
}

pub fn firewall_rules(p: &Profile, uid: u32, os: &str) -> Result<String> {
    validate(p)?;
    ensure!(uid > 0, "Lima UID must not be root");
    let name = format!(
        "horde_lima_{}",
        crate::store::hash(p.project.as_bytes())
            .chars()
            .take(16)
            .collect::<String>()
    );
    match os {
        "linux" => {
            let allow = p
                .lima_egress
                .iter()
                .map(|value| {
                    let family = if value.contains(':') { "ip6" } else { "ip" };
                    format!("meta skuid {uid} {family} daddr {value} accept\n")
                })
                .collect::<String>();
            Ok(format!(
                "table inet {name} {{\nchain output {{ type filter hook output priority -100; policy accept;\n{allow}meta skuid {uid} drop\n}}\n}}\n"
            ))
        }
        "macos" => {
            let allow = p.lima_egress.iter().map(|value| format!("pass out quick proto {{ tcp udp }} from any to {value} user {uid} no state\n")).collect::<String>();
            Ok(format!(
                "{allow}block drop out quick proto {{ tcp udp }} from any to any user {uid}\n"
            ))
        }
        _ => bail!("unsupported Lima firewall host"),
    }
}

async fn command(program: &Path, args: &[String], input: Option<&[u8]>) -> Result<Value> {
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .env_clear()
        .env(
            "PATH",
            "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin",
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("unable to start Lima prerequisite or management command")?;
    let mut stdin = child.stdin.take().context("command stdin")?;
    let bytes = input.unwrap_or_default();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1200), async {
        stdin.write_all(bytes).await?;
        drop(stdin);
        child.wait_with_output().await
    })
    .await
    .context("Lima command timed out; inspect before reconciling")??;
    ensure!(
        result.status.success(),
        "Lima command failed ({}); inspect host prerequisites and owned guest",
        result.status
    );
    ensure!(
        result.stdout.len() <= 4 * 1024 * 1024,
        "Lima output too large"
    );
    Ok(serde_json::from_slice(&result.stdout)
        .unwrap_or_else(|_| json!({"output":String::from_utf8_lossy(&result.stdout).trim()})))
}

async fn guard(p: &Profile, action: &str) -> Result<Value> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(&p.lima_guard)
        .context("install the bundled root-owned Lima network guard before provisioning")?;
    let canonical = p.lima_guard.canonicalize()?;
    for directory in canonical
        .parent()
        .context("Lima guard directory")?
        .ancestors()
    {
        let metadata = std::fs::metadata(directory)?;
        ensure!(
            metadata.uid() == 0 && metadata.mode() & 0o022 == 0,
            "Lima network guard parents must be root-owned and not group/world writable"
        );
    }
    ensure!(
        metadata.is_file() && metadata.uid() == 0 && metadata.mode() & 0o022 == 0,
        "Lima network guard must be root-owned and not group/world writable"
    );
    let report = command(
        Path::new("/usr/bin/sudo"),
        &[
            "-n".into(),
            p.lima_guard.to_string_lossy().into_owned(),
            action.into(),
            p.project.clone(),
        ],
        None,
    )
    .await?;
    ensure!(
        report["project"] == p.project
            && report["user"] == p.lima_user
            && report["home"] == p.lima_home.to_string_lossy().as_ref()
            && report["egress"] == json!(p.lima_egress)
            && report["enforced"] == true,
        "host Lima policy does not match approved project profile"
    );
    Ok(report)
}

async fn lima(p: &Profile, args: Vec<String>, input: Option<&[u8]>) -> Result<Value> {
    let mut argv = vec![
        "-n".into(),
        "-H".into(),
        "-u".into(),
        p.lima_user.clone(),
        "--".into(),
        "/usr/bin/env".into(),
        format!("LIMA_HOME={}", p.lima_home.display()),
        "PATH=/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin".into(),
        "limactl".into(),
    ];
    argv.extend(args);
    command(Path::new("/usr/bin/sudo"), &argv, input).await
}

async fn lookup_guest(p: &Profile, resource: &str) -> Result<Value> {
    // A named lookup exits nonzero for absence. Only a successful complete list
    // proves absence; command errors must retain the resource for reconciliation.
    let inventory = lima(p, vec!["list".into(), "--quiet".into()], None).await?;
    let names = inventory["output"]
        .as_str()
        .context("invalid Lima instance inventory")?;
    ensure!(
        names.lines().all(identifier),
        "invalid Lima instance inventory"
    );
    if !names.lines().any(|name| name == resource) {
        return Ok(json!([]));
    }
    lima(
        p,
        vec!["list".into(), "--json".into(), resource.into()],
        None,
    )
    .await
}

async fn prerequisites(p: &Profile, install_guard: bool) -> Result<()> {
    validate(p)?;
    validate_socket_path(&p.lima_home, "h00000000000", std::env::consts::OS)?;
    let identity = command(
        Path::new("/usr/bin/id"),
        &["-u".into(), p.lima_user.clone()],
        None,
    )
    .await?;
    let uid = identity
        .as_u64()
        .or_else(|| {
            identity["output"]
                .as_str()
                .and_then(|value| value.parse::<u64>().ok())
        })
        .context("dedicated Lima host UID missing")?;
    ensure!(
        uid != 0 && uid != u64::from(unsafe { libc::geteuid() }),
        "Lima host user must be a separate non-root identity"
    );
    if cfg!(target_os = "linux") {
        ensure!(
            Path::new("/dev/kvm").exists(),
            "Linux Lima requires KVM; native execution remains available"
        );
        for access in ["-r", "-w"] {
            command(
                Path::new("/usr/bin/sudo"),
                &[
                    "-n".into(),
                    "-u".into(),
                    p.lima_user.clone(),
                    "--".into(),
                    "/usr/bin/test".into(),
                    access.into(),
                    "/dev/kvm".into(),
                ],
                None,
            )
            .await
            .context("dedicated Lima host user needs read/write access to KVM")?;
        }
    } else if cfg!(target_os = "macos") {
        let support = command(
            Path::new("/usr/sbin/sysctl"),
            &["-n".into(), "kern.hv_support".into()],
            None,
        )
        .await?;
        ensure!(
            support == 1 || support["output"] == "1",
            "Apple Virtualization.framework requires host hypervisor support"
        );
        let version = command(
            Path::new("/usr/bin/sw_vers"),
            &["-productVersion".into()],
            None,
        )
        .await?;
        let text = version["output"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| version.to_string());
        ensure!(
            text.split('.')
                .next()
                .context("macOS version")?
                .parse::<u32>()?
                >= 13,
            "Lima VZ backend requires macOS 13 or newer"
        );
    } else {
        bail!("Lima requires macOS or Linux; native WSL execution remains available");
    }
    guard(p, if install_guard { "apply" } else { "verify" }).await?;
    let version = lima(p, vec!["--version".into()], None).await?;
    let version = version["output"]
        .as_str()
        .context("limactl version output missing")?;
    let version = version
        .split_whitespace()
        .last()
        .context("limactl version missing")?
        .trim_start_matches('v');
    let major = version
        .split('.')
        .next()
        .context("limactl version")?
        .parse::<u32>()?;
    ensure!(
        major >= 1,
        "Lima 1.x or newer required for isolated user-v2 networking"
    );
    Ok(())
}

/// Read-only prerequisite diagnosis; never creates a VM or installs firewall rules.
pub async fn doctor(p: &Profile) -> Result<Value> {
    ensure!(
        p.provider == "lima",
        "runtime doctor currently checks Lima profiles"
    );
    p.validate()?;
    ensure!(
        p.host.is_none(),
        "run Lima doctor on the selected provisioning host using its local profile"
    );
    binary(p)?;
    prerequisites(p, false).await?;
    Ok(
        json!({"ready":true,"project":p.project,"provider":"lima","host_os":std::env::consts::OS,"architecture":std::env::consts::ARCH,"vm_type":if cfg!(target_os="macos"){"vz"}else{"qemu"},"host_egress_enforced":true,"native_execution_unaffected":true}),
    )
}

fn binary(p: &Profile) -> Result<Vec<u8>> {
    let bytes = std::fs::read(&p.lima_horde_binary)
        .context("Linux Horde binary required for guest installation")?;
    ensure!(
        bytes.len() >= 20
            && bytes.len() <= 128 * 1024 * 1024
            && &bytes[..4] == b"\x7fELF"
            && bytes[5] == 1,
        "guest Horde binary must be a bounded little-endian Linux ELF executable"
    );
    let expected = if cfg!(target_arch = "aarch64") {
        183
    } else {
        62
    };
    ensure!(
        u16::from_le_bytes([bytes[18], bytes[19]]) == expected,
        "guest Horde binary architecture does not match host"
    );
    Ok(bytes)
}

pub async fn provision(
    db: &Store,
    p: &Profile,
    id: &str,
    bootstrap: Option<&Value>,
) -> Result<String> {
    let packet = bootstrap.context("Lima requires authenticated controller enrollment")?;
    let network: crate::network::NetworkConfig = serde_json::from_value(packet["network"].clone())?;
    let controller = network
        .controller_peer
        .as_deref()
        .context("Lima controller peer required")?;
    let address = network
        .peers
        .get(controller)
        .context("Lima controller address required")?
        .address
        .ip();
    ensure!(
        p.lima_egress
            .iter()
            .any(|entry| includes(entry, address).unwrap_or(false)),
        "Lima egress must include controller address"
    );
    let executable = binary(p)?;
    let owned = prepare(
        &db.root,
        p,
        id,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )?;
    prerequisites(p, true).await?;
    let name = owned_resource(&db.root, p, id)?;
    validate_socket_path(&p.lima_home, &name, std::env::consts::OS)?;
    let authorization = owned.join("creation-authorized");
    let identity = json!({"project":p.project,"runtime":id,"resource":name});
    if authorization.exists() {
        ensure!(
            serde_json::from_slice::<Value>(&std::fs::read(&authorization)?)? == identity,
            "Lima creation authorization ownership mismatch"
        );
    } else {
        let observed = lookup_guest(p, &name).await?;
        ensure!(
            observed == json!([]),
            "Lima resource name collision: existing guest has no durable creation authorization"
        );
        crate::secrets::write_private(&authorization, identity.to_string().as_bytes())?;
        sync_directories(&db.root, &owned)?;
    }
    let config = stored_configuration(&owned)?;
    lima(
        p,
        vec![
            "start".into(),
            "--tty=false".into(),
            format!("--name={name}"),
            "-".into(),
        ],
        Some(config.to_string().as_bytes()),
    )
    .await?;
    verify_running(p, &name).await?;
    lima(
        p,
        vec![
            "shell".into(),
            name.clone(),
            "sudo".into(),
            "/bin/sh".into(),
            "-c".into(),
            "set -eu; umask 077; cat > /usr/local/bin/horde.incoming; chmod 0755 /usr/local/bin/horde.incoming; mv /usr/local/bin/horde.incoming /usr/local/bin/horde".into(),
        ],
        Some(&executable),
    )
    .await?;
    let boot = format!(
        "HORDE_BOOTSTRAP_JSON='{}'\n",
        packet.to_string().replace('\'', "'\\''")
    );
    lima(
        p,
        vec![
            "shell".into(),
            name.clone(),
            "sudo".into(),
            "/bin/sh".into(),
            "-c".into(),
            "set -eu; umask 077; cat > /var/lib/horde/bootstrap.env.incoming; chmod 0600 /var/lib/horde/bootstrap.env.incoming; mv /var/lib/horde/bootstrap.env.incoming /var/lib/horde/bootstrap.env".into(),
        ],
        Some(boot.as_bytes()),
    )
    .await?;
    let unit = "[Unit]\nDescription=Horde project runtime\nAfter=network-online.target docker.service\nWants=network-online.target\n[Service]\nUser=horde\nGroup=horde\nSupplementaryGroups=docker\nEnvironment=HOME=/home/horde\nEnvironment=HORDE_ISOLATION=lima\nEnvironmentFile=/var/lib/horde/bootstrap.env\nExecStart=/usr/local/bin/horde --data-dir /var/lib/horde/state daemon\nRestart=on-failure\n[Install]\nWantedBy=multi-user.target\n";
    lima(
        p,
        vec![
            "shell".into(),
            name.clone(),
            "sudo".into(),
            "/bin/sh".into(),
            "-c".into(),
            "set -eu; umask 077; cat > /etc/systemd/system/horde.service.incoming; chmod 0644 /etc/systemd/system/horde.service.incoming; mv /etc/systemd/system/horde.service.incoming /etc/systemd/system/horde.service".into(),
        ],
        Some(unit.as_bytes()),
    )
    .await?;
    lima(
        p,
        vec![
            "shell".into(),
            name.clone(),
            "sudo".into(),
            "systemctl".into(),
            "enable".into(),
            "--now".into(),
            "horde.service".into(),
        ],
        None,
    )
    .await?;
    crate::secrets::write_private(&owned.join("provisioned"), b"enrollment-pending\n")?;
    Ok(name)
}

pub async fn lifecycle(
    db: &Store,
    p: &Profile,
    id: &str,
    resource: &str,
    action: &str,
) -> Result<Value> {
    ensure!(
        resource == resource_name(p, id)? || resource == format!("horde-{id}"),
        "Lima resource ownership mismatch"
    );
    let owned = db
        .root
        .join("projects")
        .join(&p.project)
        .join("lima")
        .join(id)
        .join("ownership.json");
    ensure!(
        owned.is_file(),
        "Lima resource has no durable ownership intent"
    );
    ensure!(
        resource == owned_resource(&db.root, p, id)?,
        "Lima resource ownership mismatch"
    );
    prepare(
        &db.root,
        p,
        id,
        std::env::consts::OS,
        std::env::consts::ARCH,
    )?;
    if ["runtime_start", "runtime_reconcile"].contains(&action) {
        guard(p, "verify").await?;
    }
    if action == "runtime_start" {
        validate_socket_path(&p.lima_home, resource, std::env::consts::OS)?;
    }
    let object = lookup_guest(p, resource).await;
    let object = match existing_guest(object, resource, action)? {
        Some(object) => object,
        None => {
            return Ok(
                json!({"project":p.project,"resource":resource,"state":"removed","already_absent":true}),
            );
        }
    };
    if resource != format!("horde-{id}") {
        let authorization = owned
            .parent()
            .context("Lima ownership directory")?
            .join("creation-authorized");
        let identity = json!({"project":p.project,"runtime":id,"resource":resource});
        ensure!(
            authorization.is_file()
                && serde_json::from_slice::<Value>(&std::fs::read(authorization)?)? == identity,
            "Lima resource name collision: existing guest has no durable creation authorization"
        );
    }
    ensure!(
        object["name"] == resource
            || object
                .as_array()
                .is_some_and(|items| items.len() == 1 && items[0]["name"] == resource),
        "Lima instance identity mismatch"
    );
    match action {
        "runtime_reconcile" => {
            let guest = object
                .as_array()
                .and_then(|items| items.first())
                .unwrap_or(&object);
            let state = match guest["status"].as_str() {
                Some("Running") => "provisioned",
                Some("Stopped") => "stopped",
                _ => "uncertain",
            };
            Ok(
                json!({"guest":object,"project":p.project,"state":state,"isolation":"lima","host_egress_enforced":true}),
            )
        }
        "runtime_start" => {
            let result = lima(
                p,
                vec!["start".into(), "--tty=false".into(), resource.into()],
                None,
            )
            .await;
            verify_running(p, resource).await?;
            result
        }
        "runtime_stop" => lima(p, vec!["stop".into(), resource.into()], None).await,
        "runtime_destroy" => {
            lima(
                p,
                vec!["delete".into(), "--force".into(), resource.into()],
                None,
            )
            .await
        }
        _ => bail!("unsupported Lima lifecycle operation"),
    }
}

fn existing_guest(result: Result<Value>, resource: &str, action: &str) -> Result<Option<Value>> {
    let object = result?;
    let absent = object.as_array().is_some_and(Vec::is_empty) || object == json!({"output":""});
    if absent {
        ensure!(
            action == "runtime_destroy",
            "Lima owned guest is absent; only destroy may release its reservation"
        );
        return Ok(None);
    }
    ensure!(
        object["name"] == resource
            || object
                .as_array()
                .is_some_and(|items| items.len() == 1 && items[0]["name"] == resource),
        "Lima instance identity mismatch"
    );
    Ok(Some(object))
}

async fn verify_running(p: &Profile, name: &str) -> Result<()> {
    if let Err(error) = guard(p, "verify").await {
        let stopped = lima(p, vec!["stop".into(), "--force".into(), name.into()], None).await;
        ensure!(
            stopped.is_ok(),
            "host network verification failed and guest shutdown is uncertain; inspect owned guest immediately"
        );
        return Err(error.context("host network verification failed; guest stopped"));
    }
    Ok(())
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    #[test]
    fn only_confirmed_absence_can_complete_destroy() {
        assert!(
            existing_guest(Ok(json!([])), "h123", "runtime_destroy")
                .unwrap()
                .is_none()
        );
        assert!(
            existing_guest(Ok(json!({"output":""})), "h123", "runtime_destroy")
                .unwrap()
                .is_none()
        );
        assert!(
            existing_guest(
                Err(anyhow::anyhow!("list failed")),
                "h123",
                "runtime_destroy"
            )
            .is_err()
        );
        assert!(
            existing_guest(Ok(json!({"output":"warning"})), "h123", "runtime_destroy").is_err()
        );
        assert!(existing_guest(Ok(json!([])), "h123", "runtime_start").is_err());
        assert!(existing_guest(Ok(json!({"name":"other"})), "h123", "runtime_destroy").is_err());
    }
}
