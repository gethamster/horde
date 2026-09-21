//! Stock AX resource specifications contain public identity, never enrollment secrets.
use super::{Profile, ax::wire::*};
use anyhow::{Context, Result, ensure};
use serde_json::Value;

pub(super) fn atespace(p: &Profile) -> String {
    format!("horde-{}", p.project)
}
pub(super) fn metadata(p: &Profile, name: &str) -> ObjectMeta {
    ObjectMeta {
        name: name.into(),
        atespace: atespace(p),
    }
}
pub(super) fn task(p: &Profile, id: &str, name: &str, owner: &str) -> Task {
    let resources = ResourceList {
        cpu: p.cpus.to_string(),
        memory: format!("{}Mi", p.memory_mb),
    };
    Task {
        api_version: "ax.io/v1alpha1".into(),
        kind: "Task".into(),
        metadata: Some(metadata(p, name)),
        status: None,
        spec: Some(TaskSpec {
            suspend: false,
            image: p.image.clone(),
            command: vec![],
            env: [
                ("HORDE_AX_RUNTIME_ID", id),
                ("HORDE_AX_PROJECT_ID", p.project.as_str()),
                ("HORDE_AX_OWNER", owner),
            ]
            .into_iter()
            .map(|(name, value)| EnvVar {
                name: name.into(),
                value: value.into(),
            })
            .collect(),
            resources: Some(ResourceReqs {
                requests: Some(resources.clone()),
                limits: Some(resources),
            }),
            workspaces: vec![WorkspaceRef {
                name: name.into(),
                path: "/workspace".into(),
                goal: String::new(),
            }],
            gateway: Some(GatewayRef { name: name.into() }),
            debug: false,
        }),
    }
}
pub(super) fn workspace(p: &Profile, name: &str) -> Workspace {
    Workspace {
        api_version: "ax.io/v1alpha1".into(),
        kind: "Workspace".into(),
        metadata: Some(metadata(p, name)),
        spec: Some(WorkspaceSpec::default()),
    }
}
pub(super) fn rule(value: &str) -> Result<HostRule> {
    let (host, port) = value
        .rsplit_once(':')
        .context("AX egress requires host:port")?;
    let port = port.parse::<u16>().context("invalid AX egress port")?;
    ensure!(
        !host.is_empty() && port > 0 && !host.chars().any(char::is_whitespace),
        "invalid AX egress host"
    );
    Ok(HostRule {
        host: host.trim_start_matches('[').trim_end_matches(']').into(),
        port: i32::from(port),
    })
}
pub(super) fn gateway(p: &Profile, name: &str, bootstrap: &Value) -> Result<Gateway> {
    let mut hosts = p
        .ax_egress
        .iter()
        .map(|s| rule(s))
        .collect::<Result<Vec<_>>>()?;
    for peer in bootstrap["network"]["peers"]
        .as_object()
        .into_iter()
        .flat_map(|v| v.values())
    {
        let address = peer["address"]
            .as_str()
            .context("bootstrap controller address")?;
        let address = address.parse::<std::net::SocketAddr>()?;
        let rule = HostRule {
            host: format!(
                "{}/{}",
                address.ip(),
                if address.is_ipv4() { 32 } else { 128 }
            ),
            port: i32::from(address.port()),
        };
        hosts.push(rule);
    }
    // The pinned AX converter ignores ports and emits one CIDR per host.
    // Keep the first rule for each host so Substrate never receives duplicates.
    let mut seen = std::collections::BTreeSet::new();
    let hosts = hosts
        .into_iter()
        .filter(|rule| seen.insert(rule.host.clone()))
        .collect();
    Ok(Gateway {
        api_version: "ax.io/v1alpha1".into(),
        kind: "Gateway".into(),
        metadata: Some(metadata(p, name)),
        spec: Some(GatewaySpec {
            listeners: vec![Listener {
                name: "horde-bootstrap".into(),
                port: 80,
                protocol: "HTTP".into(),
            }],
            egress: Some(EgressConfig {
                allowlist: Some(EgressAllowlist { hosts }),
            }),
        }),
    })
}
pub(super) fn verify_task(actual: &Task, expected: &Task) -> Result<()> {
    let mut actual = actual.clone();
    actual.status = None;
    if let Some(spec) = actual.spec.as_mut() {
        spec.suspend = false;
    }
    ensure!(
        actual == *expected,
        "AX task ownership or pinned specification mismatch"
    );
    Ok(())
}
