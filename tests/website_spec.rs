//! The published agent-facing catalog is generated from the live tool surface.
//!
//! `website/public/.well-known/tools.json` is served to agents as function-calling
//! definitions, so it must not drift from `protocol::OPERATIONS`. Regenerate it
//! with `UPDATE_WEBSITE_SPEC=1 cargo test --test website_spec`.
use horde::protocol::{OPERATIONS, schema, worker_allowed};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

fn catalog() -> Value {
    let tools: Vec<Value> = OPERATIONS
        .iter()
        .map(|(name, description)| {
            json!({
                "name": name,
                "description": description,
                "scope": if worker_allowed(name) { "worker" } else { "admin" },
                "parameters": schema(name),
            })
        })
        .collect();
    json!({
        "$schema": "https://horde.sh/api/v1/tools.schema.json",
        "specVersion": "1.0",
        "product": "Horde",
        "transport": {
            "protocol": "mcp",
            "version": "2024-11-05",
            "kind": "stdio",
            "command": "horde",
            "args": ["mcp"],
        },
        "scopes": {
            "admin": "Available to the operator's own MCP bridge (`horde mcp`).",
            "worker": "Also available to task-scoped worker credentials.",
        },
        "count": tools.len(),
        "tools": tools,
    })
}

fn published() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("website/public/.well-known/tools.json")
}

#[test]
fn published_catalog_matches_tool_surface() {
    let generated = format!("{:#}\n", catalog());
    let path = published();
    if std::env::var_os("UPDATE_WEBSITE_SPEC").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &generated).unwrap();
        return;
    }
    let current = fs::read_to_string(&path).expect("published tool catalog is missing");
    assert_eq!(
        current, generated,
        "published tool catalog is stale; regenerate with \
         UPDATE_WEBSITE_SPEC=1 cargo test --test website_spec"
    );
}

#[test]
fn every_published_tool_is_function_calling_compatible() {
    let catalog = catalog();
    let mut names: Vec<&str> = Vec::new();
    for tool in catalog["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        assert!(
            !tool["description"].as_str().unwrap().is_empty(),
            "{name} has no description"
        );
        assert_eq!(
            tool["parameters"]["type"], "object",
            "{name} is not an object schema"
        );
        assert!(
            tool["parameters"]["additionalProperties"] == json!(false),
            "{name} does not close its schema"
        );
        for required in tool["parameters"]["required"].as_array().unwrap() {
            let key = required.as_str().unwrap();
            assert!(
                tool["parameters"]["properties"].get(key).is_some(),
                "{name} requires undeclared property {key}"
            );
        }
        names.push(name);
    }
    let unique = names
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    assert_eq!(unique, names.len(), "operation names must be unique");
}
