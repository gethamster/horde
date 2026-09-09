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

#[test]
fn workflow_tools_publish_the_rust_step_fields_and_inline_nested_types() {
    let step: horde::template::Step = serde_json::from_value(json!({"id":"sample"})).unwrap();
    let serialized = serde_json::to_value(step).unwrap();
    for name in ["propose_steps", "add_steps"] {
        let parameters = schema(name);
        let steps = &parameters["properties"]["steps"];
        let item = &steps["items"];
        assert_eq!(item["type"], "object");
        assert_eq!(item["additionalProperties"], false);
        assert_eq!(item["required"], json!(["id"]));
        let fields = item["properties"].as_object().unwrap();
        assert_eq!(
            fields.keys().collect::<Vec<_>>(),
            serialized.as_object().unwrap().keys().collect::<Vec<_>>()
        );
        assert_eq!(fields["needs"]["items"]["type"], "string");
        assert_eq!(fields["command"]["items"]["type"], "string");
        assert_eq!(fields["attempts"]["minimum"], 1);
        assert_eq!(fields["attempts"]["maximum"], 20);
        assert!(
            !item.to_string().contains("\"$ref\""),
            "nested schemas must be self-contained"
        );
        for (field, nested) in [("when", "status"), ("environment", "runner")] {
            let object = &fields[field];
            assert_eq!(object["type"], json!(["object", "null"]));
            assert_eq!(object["additionalProperties"], false);
            assert!(object["properties"][nested]["enum"].is_array());
        }
        assert_eq!(fields["environment"]["required"], json!(["test"]));
        assert!(
            fields["environment"]["properties"]["test"]
                .get("default")
                .is_none()
        );
        if name == "propose_steps" {
            assert_eq!(steps["minItems"], 1);
            assert_eq!(steps["maxItems"], 32);
            assert!(
                !fields["kind"]["enum"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("delivery"))
            );
            assert_eq!(fields["template"]["type"], "null");
        }
    }
}

#[test]
fn published_worker_schemas_omit_runtime_owned_arguments() {
    let published: Value = if std::env::var_os("UPDATE_WEBSITE_SPEC").is_some() {
        catalog()
    } else {
        serde_json::from_str(&fs::read_to_string(published()).unwrap()).unwrap()
    };
    for tool in published["tools"].as_array().unwrap() {
        if tool["scope"] != "worker" {
            continue;
        }
        let fields = tool["parameters"]["properties"].as_object().unwrap();
        assert!(
            fields.keys().all(
                |k| !["task", "worker", "step", "verified"].contains(&k.as_str())
                    && !k.starts_with('_')
            ),
            "{}",
            tool["name"]
        );
    }
    for name in ["propose_steps", "put_artifact", "add_knowledge"] {
        let tool = published["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap();
        assert_eq!(tool["parameters"], schema(name));
        assert!(
            horde::protocol::admin_schema(name)["properties"]
                .get("task")
                .is_some()
        );
    }
}
