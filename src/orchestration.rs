//! Resolve a parent's stated machine/model pools without choosing its work strategy.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn label(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_whitespace() && !['-', '_'].contains(c))
        .flat_map(char::to_lowercase)
        .collect()
}

fn allowed(roles: &serde_json::Map<String, Value>) -> Value {
    let mut runtimes = std::collections::BTreeMap::<String, BTreeSet<String>>::new();
    for role in roles.values() {
        for group in role["allowed"].as_array().into_iter().flatten() {
            if let Some(runtime) = group["runtime"].as_str() {
                runtimes.entry(runtime.into()).or_default().extend(
                    group["capabilities"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned),
                );
            }
        }
    }
    json!(
        runtimes
            .into_iter()
            .map(|(runtime, capabilities)| json!({"runtime":runtime,"capabilities":capabilities}))
            .collect::<Vec<_>>()
    )
}

fn runtime<'a>(inventory: &'a Value, requested: &str) -> Result<&'a Value> {
    let entries = inventory["runtimes"]
        .as_array()
        .context("runtime inventory missing")?;
    let exact: Vec<_> = entries
        .iter()
        .filter(|r| r["runtime"] == requested || (requested == "local" && r["local"] == true))
        .collect();
    let matches = if exact.is_empty() {
        entries
            .iter()
            .filter(|r| {
                r["name"]
                    .as_str()
                    .is_some_and(|n| label(n) == label(requested))
            })
            .collect()
    } else {
        exact
    };
    ensure!(
        !matches.is_empty(),
        "runtime {requested} is not discovered; inspect agent_setup for enrollment/access requirements"
    );
    ensure!(
        matches.len() == 1,
        "runtime name {requested} is ambiguous; resolve its identity before dispatch"
    );
    Ok(matches[0])
}

fn capability<'a>(runtime: &'a Value, requested: &str) -> Result<&'a Value> {
    let caps = runtime["capabilities"]
        .as_array()
        .context("worker has not advertised capabilities; upgrade or reconnect it")?;
    // An explicit executor ID wins. Otherwise only equivalent advertised bindings
    // may be collapsed; choosing between different models belongs to the parent.
    if let Some(cap) = caps.iter().find(|cap| cap["id"] == requested) {
        return Ok(cap);
    }
    let matches: Vec<_> = caps
        .iter()
        .filter(|cap| {
            ["model", "provider", "kind"].iter().any(|field| {
                cap[*field]
                    .as_str()
                    .is_some_and(|v| label(v) == label(requested))
            })
        })
        .collect();
    ensure!(
        !matches.is_empty(),
        "model or executor {requested} is not advertised by {}; inspect setup requirements instead of substituting another model",
        runtime["name"]
    );
    let bindings: BTreeSet<_> = matches
        .iter()
        .map(|c| {
            (
                c["provider"].to_string(),
                c["model"].to_string(),
                c["kind"].to_string(),
                c["configuration_hash"].to_string(),
            )
        })
        .collect();
    ensure!(
        bindings.len() == 1,
        "model or executor {requested} is ambiguous on {}; choose an advertised capability ID",
        runtime["name"]
    );
    matches
        .into_iter()
        .min_by_key(|c| c["id"].as_str().unwrap_or(""))
        .context("capability missing")
}

pub fn resolve(inventory: &Value, args: &Value) -> Result<Value> {
    let roles = args["roles"]
        .as_object()
        .context("roles must map work roles to runtime and models")?;
    ensure!(
        !roles.is_empty() && roles.len() <= 32,
        "supply between 1 and 32 work roles"
    );
    let mut result = serde_json::Map::new();
    let mut blockers = Vec::new();
    for (role, spec) in roles {
        ensure!(
            !role.trim().is_empty() && role.len() <= 128,
            "invalid work role"
        );
        let requested = spec["runtime"]
            .as_str()
            .context("each role requires runtime")?;
        let models = spec["models"]
            .as_array()
            .context("each role requires an allowed models array")?;
        ensure!(
            !models.is_empty() && models.len() <= 64,
            "supply between 1 and 64 allowed models"
        );
        let resolved = (|| -> Result<Value> {
            let runtime = runtime(inventory, requested)?;
            ensure!(
                runtime["fresh"] == true && runtime["ready"] == true,
                "runtime {requested} is offline or its capability report is stale; reconnect before dispatch"
            );
            ensure!(
                runtime["capacity"]["draining"] != true,
                "runtime {requested} is draining; wait or replan within the allowed pool"
            );
            ensure!(
                runtime["capacity"]["available"].as_u64().unwrap_or(0) > 0,
                "runtime {requested} has no available capacity; wait or replan within the allowed pool"
            );
            let mut selected = Vec::new();
            for model in models {
                let requested_model = model.as_str().context("model names must be strings")?;
                let cap = capability(runtime, requested_model)?;
                ensure!(
                    cap["available"] != false,
                    "{requested_model} is unavailable on {requested}; inspect agent_setup for missing executable or credentials"
                );
                if !selected.iter().any(|c: &Value| c["id"] == cap["id"]) {
                    selected.push(cap.clone());
                }
            }
            let ids: Vec<_> = selected.iter().map(|c| c["id"].clone()).collect();
            let allowed = json!([{"runtime":runtime["runtime"],"capabilities":ids}]);
            crate::execution_selection::prepare_inventory(
                inventory,
                &json!({"allowed":allowed,"selected":{"runtime":runtime["runtime"],"capability":selected[0]["id"]}}),
                None,
            )?;
            Ok(
                json!({"runtime":runtime["runtime"],"name":runtime["name"],"allowed":allowed,"choices":selected}),
            )
        })();
        match resolved {
            Ok(value) => {
                result.insert(role.clone(), value);
            }
            Err(error) => {
                blockers.push(json!({"role":role,"runtime":requested,"reason":error.to_string()}))
            }
        }
    }
    Ok(
        json!({"ready":blockers.is_empty(),"allowed":allowed(&result),"roles":result,"blockers":blockers,"dispatch_started":false,"guidance":"Choose one advertised capability for each task within its role's allowed pool. A parent that delegates across roles needs the combined allowed pool; narrow each child's pool to its work role. Explain the split briefly, then submit or delegate with execution.allowed and execution.selected. Authentication evidence may be unverified; do not describe credential presence as verified access. Reviewable results are the default; merge and deployment require their own authorization."}),
    )
}

pub fn plan(db: &crate::store::Store, args: &Value) -> Result<Value> {
    let inventory = crate::capabilities::inventory(db)?;
    let mut result = resolve(&inventory, args)?;
    if let Some(task) = args["task"].as_str()
        && let Some(parent) = crate::execution_selection::policy(db, task)?
    {
        let roles = result["roles"]
            .as_object()
            .context("resolved roles")?
            .clone();
        for (role, resolved) in roles {
            let input = json!({"allowed":resolved["allowed"],"selected":{"runtime":resolved["runtime"],"capability":resolved["choices"][0]["id"]}});
            if let Err(error) =
                crate::execution_selection::prepare_inventory(&inventory, &input, Some(&parent))
            {
                result["roles"]
                    .as_object_mut()
                    .context("resolved roles")?
                    .remove(&role);
                result["blockers"]
                    .as_array_mut()
                    .context("planning blockers")?
                    .push(json!({"role":role,"reason":error.to_string()}));
                result["ready"] = json!(false);
            }
        }
    }
    result["allowed"] = allowed(result["roles"].as_object().context("resolved roles")?);
    Ok(result)
}
