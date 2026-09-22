//! Fold controller-side fleet-credential creation and Taildrop delivery into one step.
use super::{ServerConfig, admin, authority, defaults};
use crate::{
    network::{self, NetworkConfig, Peer, Provider},
    store::{Store, id},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};

fn matches(peer: &Peer, query: &str) -> bool {
    peer.tls_name == query || peer.tls_name.split('.').next() == Some(query) || peer.id == query
}

fn resolve_peer<'a>(peers: &'a [Peer], query: &str) -> Result<&'a Peer> {
    let found: Vec<&Peer> = peers.iter().filter(|p| matches(p, query)).collect();
    match found.as_slice() {
        [] => {
            bail!("no peer matching '{query}' found; run `horde network peers` to see candidates")
        }
        [one] => Ok(one),
        many => bail!(
            "peer query '{query}' matches {} peers ({}); use a more specific hostname or node id",
            many.len(),
            many.iter()
                .map(|p| p.tls_name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub async fn invite(
    root: &Path,
    peer_query: &str,
    worker_name: Option<&str>,
    expires_in: i64,
    max_workers: usize,
) -> Result<Value> {
    admin()?;
    let db = Store::open(root)?;
    let managed = root.join("managed-network.toml");
    let network = NetworkConfig::load(managed.exists().then_some(managed.as_path()))?;
    ensure!(
        network.provider != Provider::Disabled && network.controller_peer.is_none(),
        "configure a network controller before inviting a peer"
    );
    ensure!(
        network.provider == Provider::Tailscale,
        "horde network invite requires the tailscale provider; direct-peer networks have no Taildrop transport — use `horde network key create` and copy the credential file yourself"
    );
    let discovery = network::discover(&network).await?;
    let peer = resolve_peer(&discovery.peers, peer_query)?.clone();
    if peer.online == Some(false) {
        eprintln!(
            "warning: {} currently shows offline in Tailscale status; Taildrop may fail until it reconnects",
            peer.tls_name
        );
    }
    let short = peer
        .tls_name
        .split('.')
        .next()
        .unwrap_or(&peer.tls_name)
        .to_owned();

    let server_path = root.join("enrollment-server.toml");
    let existing = if server_path.exists() {
        Some(toml::from_str::<ServerConfig>(&std::fs::read_to_string(
            &server_path,
        )?)?)
    } else {
        None
    };
    let server = defaults::resolve(
        &network,
        existing.as_ref(),
        defaults::Overrides {
            listen: None,
            controller_address: None,
            tls_name: None,
            issuer_key: None,
        },
    )
    .await?;
    ensure!(
        server.listen.port() != network.port,
        "enrollment must use a separate port from the runtime listener"
    );
    let server = ServerConfig {
        issuer_key: server
            .issuer_key
            .canonicalize()
            .context("read controller CA signing key")?,
        ..server
    };
    if let Some(existing) = existing {
        ensure!(
            existing == server,
            "fleet enrollment already uses different listener or trust settings"
        );
    }

    let key_name = format!("invite-{short}-{}", &id()[..8]);
    let invitation = db.atomic(|| {
        let invitation = authority::create_key(
            &db,
            &network,
            &server,
            &key_name,
            expires_in,
            max_workers,
            4,
        )?;
        invitation.validate()?;
        if !server_path.exists() {
            crate::secrets::write_private(&server_path, toml::to_string(&server)?.as_bytes())?;
        }
        Ok(invitation)
    })?;

    let invites_dir = root.join("invites");
    std::fs::create_dir_all(&invites_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&invites_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let file_name = format!("horde-invite-{short}.json");
    let file_path = invites_dir.join(&file_name);
    super::replace_private(&file_path, &serde_json::to_vec(&invitation)?)?;

    let path_str = file_path
        .to_str()
        .context("credential path must be valid UTF-8")?;
    let destination = format!("{}:", peer.tls_name);
    let send = tokio::process::Command::new(&network.tailscale_program)
        .args(["file", "cp", path_str, &destination])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .status();
    let sent = tokio::time::timeout(Duration::from_secs(30), send)
        .await
        .context("tailscale file cp timed out")?
        .context("tailscale file cp failed to start")?
        .success();
    ensure!(
        sent,
        "tailscale file cp failed; retry manually: tailscale file cp {} {}",
        file_path.display(),
        destination
    );
    std::fs::remove_file(&file_path).ok();

    let name_flag = worker_name
        .map(|n| format!(" --name {n}"))
        .unwrap_or_default();
    Ok(json!({
        "key": invitation.key_id,
        "peer": peer.tls_name,
        "credential_file": file_name,
        "expires_in": expires_in,
        "max_workers": max_workers,
        "next": format!(
            "On {}: accept the Taildrop transfer if prompted (Tailscale menu bar), then run:",
            peer.tls_name
        ),
        "join_command": format!("horde network join ~/Downloads/{file_name}{name_flag}"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(id: &str, tls_name: &str, online: Option<bool>) -> Peer {
        Peer {
            id: id.into(),
            tls_name: tls_name.into(),
            addresses: vec![],
            online,
        }
    }

    #[test]
    fn matches_by_exact_name_short_hostname_or_id() {
        let peers = vec![
            peer("n1", "tuara.example.ts.net", Some(true)),
            peer("n2", "personal.example.ts.net", Some(true)),
        ];
        assert_eq!(
            resolve_peer(&peers, "tuara.example.ts.net").unwrap().id,
            "n1"
        );
        assert_eq!(resolve_peer(&peers, "tuara").unwrap().id, "n1");
        assert_eq!(resolve_peer(&peers, "n2").unwrap().id, "n2");
    }

    #[test]
    fn zero_matches_lists_the_lookup_hint() {
        let peers = vec![peer("n1", "tuara.example.ts.net", Some(true))];
        let error = resolve_peer(&peers, "missing").unwrap_err().to_string();
        assert!(error.contains("no peer matching 'missing'"));
        assert!(error.contains("horde network peers"));
    }

    #[test]
    fn ambiguous_short_names_list_every_candidate() {
        let peers = vec![
            peer("n1", "worker.hosta.ts.net", Some(true)),
            peer("n2", "worker.hostb.ts.net", Some(true)),
        ];
        let error = resolve_peer(&peers, "worker").unwrap_err().to_string();
        assert!(error.contains("matches 2 peers"));
        assert!(error.contains("worker.hosta.ts.net"));
        assert!(error.contains("worker.hostb.ts.net"));
    }

    #[test]
    fn offline_peer_still_resolves_for_the_caller_to_warn_on() {
        let peers = vec![peer("n1", "sleepy.example.ts.net", Some(false))];
        assert_eq!(resolve_peer(&peers, "sleepy").unwrap().online, Some(false));
    }
}
