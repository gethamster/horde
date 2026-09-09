use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use toml_edit::{Array, DocumentMut, Item, Table, value};

use super::Agent;

pub(super) fn merge(agent: Agent, original: Option<&[u8]>, args: &[String]) -> Result<Vec<u8>> {
    match agent {
        Agent::Codex => codex(original, args),
        Agent::Claude => claude(original, args),
    }
}

fn codex(original: Option<&[u8]>, args: &[String]) -> Result<Vec<u8>> {
    let text =
        std::str::from_utf8(original.unwrap_or_default()).context("Codex config must be UTF-8")?;
    let mut document: DocumentMut = text.parse().context("invalid Codex configuration TOML")?;
    if document.get("mcp_servers").is_none() {
        document["mcp_servers"] = Item::Table(Table::new());
    }
    let servers = document["mcp_servers"]
        .as_table_like_mut()
        .context("Codex mcp_servers must be a table")?;
    if let Some(existing) = servers.get("horde") {
        let table = existing
            .as_table_like()
            .context("existing Horde MCP configuration must be a table")?;
        let matches = table.len() == 2
            && table.get("command").and_then(Item::as_str) == Some("horde")
            && table
                .get("args")
                .and_then(Item::as_array)
                .is_some_and(|array| {
                    array.len() == args.len()
                        && array
                            .iter()
                            .zip(args)
                            .all(|(actual, expected)| actual.as_str() == Some(expected))
                });
        ensure!(
            matches,
            "existing Horde MCP configuration conflicts with init; preserve or reconcile it before rerunning"
        );
        return Ok(text.as_bytes().to_vec());
    }
    let mut horde = Table::new();
    horde["command"] = value("horde");
    horde["args"] = value(args.iter().map(String::as_str).collect::<Array>());
    servers.insert("horde", Item::Table(horde));
    Ok(document.to_string().into_bytes())
}

fn claude(original: Option<&[u8]>, args: &[String]) -> Result<Vec<u8>> {
    let mut document: Value = match original {
        Some(bytes) => {
            serde_json::from_slice(bytes).context("invalid Claude MCP configuration JSON")?
        }
        None => json!({}),
    };
    let object = document
        .as_object_mut()
        .context("Claude MCP configuration must be an object")?;
    let servers = object
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("Claude mcpServers must be an object")?;
    let expected = json!({"command": "horde", "args": args});
    if let Some(existing) = servers.get("horde") {
        ensure!(
            *existing == expected,
            "existing Horde MCP configuration conflicts with init; preserve or reconcile it before rerunning"
        );
        return Ok(original.unwrap_or_default().to_vec());
    }
    servers.insert("horde".into(), expected);
    let mut output = serde_json::to_vec_pretty(&document)?;
    output.push(b'\n');
    Ok(output)
}
