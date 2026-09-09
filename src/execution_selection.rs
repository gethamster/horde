//! Immutable task-scoped runtime/capability permissions and local configuration selection.
use crate::{config::Settings, store::Store};
use anyhow::{Context, Result, ensure};
use rusqlite::params;
use serde_json::{Value, json};

pub fn migrate(connection: &rusqlite::Connection) -> Result<()> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS task_execution_policy(task TEXT PRIMARY KEY REFERENCES tasks(id),policy TEXT NOT NULL)")?;
    Ok(())
}

pub fn policy(db: &Store, task: &str) -> Result<Option<Value>> {
    db.rows(
        "SELECT policy FROM task_execution_policy WHERE task=?",
        &[&task],
    )?
    .first()
    .map(|row| {
        serde_json::from_str(row["policy"].as_str().context("execution policy")?)
            .map_err(Into::into)
    })
    .transpose()
}

pub fn pin(db: &Store, task: &str, value: &Value) -> Result<()> {
    validate_policy(value)?;
    db.atomic(|| {
        if let Some(existing) = policy(db, task)? {
            ensure!(
                existing == *value,
                "task execution selection is immutable; create a new child to change it"
            );
        } else {
            db.conn.execute(
                "INSERT INTO task_execution_policy VALUES(?,?)",
                params![task, value.to_string()],
            )?;
        }
        Ok(())
    })
}

pub fn prepare(db: &Store, input: &Value, parent: Option<&Value>) -> Result<Value> {
    prepare_inventory(&crate::capabilities::inventory(db)?, input, parent)
}

fn runtime<'a>(inventory: &'a Value, selector: &str) -> Result<&'a Value> {
    let runtimes = inventory["runtimes"]
        .as_array()
        .context("capability inventory unavailable")?;
    let exact = runtimes
        .iter()
        .find(|runtime| runtime["runtime"] == selector);
    if let Some(exact) = exact {
        return Ok(exact);
    }
    let matches: Vec<_> = runtimes
        .iter()
        .filter(|runtime| {
            (selector == "local" && runtime["local"] == true) || runtime["name"] == selector
        })
        .collect();
    ensure!(
        matches.len() == 1,
        "runtime {selector} is missing or ambiguous; refresh runtime capabilities"
    );
    Ok(matches[0])
}

fn binding(inventory: &Value, selector: &str, capability: &str, selected: bool) -> Result<Value> {
    let runtime = runtime(inventory, selector)?;
    if selected {
        ensure!(
            runtime["fresh"] == true
                && runtime["ready"] != false
                && runtime["capacity"]["draining"] != true,
            "runtime {selector} is offline, stale, or draining; refresh capabilities or choose an allowed runtime"
        );
        ensure!(
            runtime["capacity"]["available"]
                .as_u64()
                .is_some_and(|slots| slots > 0),
            "runtime {selector} has no available execution capacity; wait or choose another allowed runtime"
        );
        ensure!(
            runtime["local"] == true
                || runtime["protocol"]["features"]
                    .as_array()
                    .is_some_and(|features| features
                        .iter()
                        .any(|feature| feature == "execution_selection")),
            "selected runtime does not advertise execution selection; update it before submitting scoped work"
        );
    }
    let candidate = runtime["capabilities"]
        .as_array()
        .and_then(|caps| caps.iter().find(|cap| cap["id"] == capability))
        .with_context(|| {
            format!("capability {capability} is unavailable on {selector}; refresh capabilities")
        })?;
    if candidate["kind"] != "simulated" {
        ensure!(
            explicit_model(&candidate["model"]),
            "capability {capability} has no explicit model; configure and advertise an exact model before selecting it"
        );
    }
    ensure!(
        candidate["configuration_hash"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())),
        "capability {capability} has no configuration fingerprint; refresh capabilities or update that runtime"
    );
    if selected {
        ensure!(
            candidate["available"] != false,
            "capability {capability} on {selector} is missing its executable or authentication"
        );
    }
    Ok(
        json!({"runtime":runtime["runtime"],"capability":capability,"provider":candidate["provider"],"model":candidate["model"],"kind":candidate["kind"],"configuration_hash":candidate["configuration_hash"]}),
    )
}

fn same_pair(a: &Value, b: &Value) -> bool {
    a["runtime"] == b["runtime"] && a["capability"] == b["capability"]
}

fn explicit_model(model: &Value) -> bool {
    model.as_str().is_some_and(|model| {
        !model.is_empty()
            && model == model.trim()
            && !model.eq_ignore_ascii_case("auto")
            && !model.eq_ignore_ascii_case("default")
    })
}

/// Normalize labels against a discovered inventory; preserve allowed runtime/capability pairs.
pub fn prepare_inventory(
    inventory: &Value,
    input: &Value,
    parent: Option<&Value>,
) -> Result<Value> {
    ensure!(input.is_object(), "execution selection must be an object");
    if let Some(parent) = parent {
        validate_policy(parent)?;
    }
    let selected_input = input
        .get("selected")
        .or_else(|| parent.and_then(|parent| parent.get("selected")));
    let default_allowed = selected_input.map(
        |selected| json!([{"runtime":selected["runtime"],"capabilities":[selected["capability"]]}]),
    );
    let groups = input
        .get("allowed")
        .or_else(|| parent.and_then(|parent| parent.get("allowed")))
        .or(default_allowed.as_ref())
        .and_then(Value::as_array)
        .context("execution.allowed or an explicit selection is required")?;
    ensure!(
        !groups.is_empty() && groups.len() <= 64,
        "allowed runtime pool must contain 1..64 entries"
    );
    let mut allowed = vec![];
    let mut bindings = vec![];
    for group in groups {
        let selector = group["runtime"]
            .as_str()
            .context("allowed runtime required")?;
        let caps = group["capabilities"]
            .as_array()
            .context("allowed capabilities required")?;
        ensure!(
            !caps.is_empty() && caps.len() <= 64,
            "allowed capability pool must contain 1..64 entries"
        );
        let resolved = runtime(inventory, selector)?["runtime"].clone();
        for cap in caps {
            let next = binding(
                inventory,
                selector,
                cap.as_str().context("capability ID required")?,
                false,
            )?;
            if let Some(parent) = parent {
                let original = parent["bindings"]
                    .as_array()
                    .and_then(|pairs| pairs.iter().find(|pair| same_pair(pair, &next)))
                    .context("child execution selection expands its parent's allowed pool")?;
                ensure!(
                    *original == next,
                    "allowed capability configuration changed; inspect the model/provider and create a new task"
                );
            }
            ensure!(
                !bindings.iter().any(|pair| same_pair(pair, &next)),
                "duplicate allowed runtime/capability pair"
            );
            bindings.push(next);
        }
        allowed.push(json!({"runtime":resolved,"capabilities":caps}));
    }
    let selected = selected_input
        .filter(|selected| !selected.is_null())
        .map(|selected| {
            let next = binding(
                inventory,
                selected["runtime"]
                    .as_str()
                    .context("selected runtime required")?,
                selected["capability"]
                    .as_str()
                    .context("selected capability required")?,
                true,
            )?;
            ensure!(
                bindings.contains(&next),
                "selected capability is outside the allowed runtime/model pool"
            );
            Ok::<Value, anyhow::Error>(next)
        })
        .transpose()?;
    let result = json!({"version":1,"allowed":allowed,"bindings":bindings,"selected":selected});
    validate_policy(&result)?;
    Ok(result)
}

fn validate_policy(value: &Value) -> Result<()> {
    ensure!(
        value["version"] == 1,
        "unsupported execution policy version"
    );
    let allowed = value["allowed"]
        .as_array()
        .context("allowed runtime pool missing")?;
    let bindings = value["bindings"]
        .as_array()
        .context("capability bindings missing")?;
    ensure!(
        !allowed.is_empty()
            && allowed.len() <= 64
            && !bindings.is_empty()
            && bindings.len() <= 4096,
        "invalid execution pool size"
    );
    let mut pairs = std::collections::BTreeSet::new();
    for pair in bindings {
        for field in ["runtime", "capability", "provider", "kind"] {
            ensure!(
                pair[field]
                    .as_str()
                    .is_some_and(|value| !value.is_empty() && value.len() <= 256),
                "invalid execution binding {field}"
            );
        }
        ensure!(
            pairs.insert((
                pair["runtime"].as_str().unwrap_or_default(),
                pair["capability"].as_str().unwrap_or_default()
            )),
            "duplicate pinned runtime/capability pair"
        );
        ensure!(
            pair["model"].is_null()
                || pair["model"]
                    .as_str()
                    .is_some_and(|model| model.len() <= 512),
            "invalid execution model"
        );
        ensure!(
            pair["kind"] == "simulated" || explicit_model(&pair["model"]),
            "explicit execution model required"
        );
        ensure!(pair["configuration_hash"].as_str().is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())), "execution configuration fingerprint required");
        ensure!(
            allowed
                .iter()
                .any(|group| group["runtime"] == pair["runtime"]
                    && group["capabilities"]
                        .as_array()
                        .is_some_and(|caps| caps.contains(&pair["capability"]))),
            "capability binding outside allowed pool"
        );
    }
    for group in allowed {
        for cap in group["capabilities"]
            .as_array()
            .context("allowed capabilities missing")?
        {
            ensure!(
                bindings
                    .iter()
                    .any(|pair| pair["runtime"] == group["runtime"] && pair["capability"] == *cap),
                "allowed capability has no pinned model binding"
            );
        }
    }
    if !value["selected"].is_null() {
        ensure!(
            bindings.contains(&value["selected"]),
            "selected capability outside pinned pool"
        );
    }
    Ok(())
}

pub fn inherit(db: &Store, parent_task: &str, child: &str, args: &Value) -> Result<()> {
    let parent = policy(db, parent_task)?;
    if parent.is_none() && args.get("execution").is_none() {
        return Ok(());
    }
    let requested = args.get("execution").cloned().unwrap_or_else(|| json!({}));
    let selected = prepare(db, &requested, parent.as_ref())?;
    pin(db, child, &selected)?;
    validate_target(
        db,
        child,
        args["peer"].as_str().or_else(|| args["on"].as_str()),
    )
}

pub fn validate_target(db: &Store, task: &str, target: Option<&str>) -> Result<()> {
    let Some(policy) = policy(db, task)? else {
        return Ok(());
    };
    let inventory = crate::capabilities::inventory(db)?;
    let target = runtime(&inventory, target.unwrap_or("local"))?;
    ensure!(
        policy["selected"].is_null() || policy["selected"]["runtime"] == target["runtime"],
        "execution selection targets a different runtime; specify the selected runtime explicitly instead of falling back locally"
    );
    Ok(())
}

pub fn validate_received(db: &Store, value: &Value, local_runtime_id: &str) -> Result<()> {
    validate_policy(value)?;
    let selected = &value["selected"];
    ensure!(
        selected["runtime"] == local_runtime_id,
        "execution selection belongs to another runtime"
    );
    let local = crate::capabilities::local(db)?;
    let current = binding(
        &json!({"runtimes":[local]}),
        local_runtime_id,
        selected["capability"]
            .as_str()
            .context("selected capability required")?,
        true,
    )?;
    ensure!(
        current == *selected,
        "selected model/provider configuration changed on the worker; refresh capabilities and submit a new task"
    );
    Ok(())
}

/// Use local provider configuration and credential references only, in a new task settings value.
pub fn apply(db: &Store, task: &str, settings: &Settings) -> Result<Settings> {
    let Some(policy) = policy(db, task)? else {
        return Ok(settings.clone());
    };
    let configured = Settings::load_user()?;
    let path = db.root.join("network-runtime.toml");
    let local = if path.exists() {
        crate::network::NetworkConfig::load(Some(&path))?.runtime_id
    } else {
        "local".into()
    };
    let roles = db
        .steps(task)?
        .iter()
        .map(Store::step)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|step| step.kind == "agent")
        .map(|step| step.role)
        .collect::<Vec<_>>();
    apply_settings(&policy, &local, &configured, settings, &roles)
}

/// Validate and use the same immutable configuration snapshot; never reload after admission.
pub fn apply_settings(
    policy: &Value,
    local: &str,
    configured: &Settings,
    settings: &Settings,
    step_roles: &[String],
) -> Result<Settings> {
    validate_policy(policy)?;
    ensure!(
        policy["selected"]["runtime"] == local,
        "execution selection belongs to another runtime"
    );
    let selected = policy["selected"]["capability"]
        .as_str()
        .context("choose an execution capability before running this task")?;
    let executor = configured
        .executors
        .get(selected)
        .context("selected local executor disappeared")?;
    let provider = configured
        .providers
        .get(executor.provider())
        .context("selected local provider disappeared")?;
    let resolved = configured
        .executor(selected)
        .context("selected local executor disappeared")?;
    let actual = json!({"runtime":local,"capability":selected,"provider":executor.provider(),"kind":resolved.kind,"model":resolved.model,"configuration_hash":crate::capabilities::configuration_hash(&resolved)?});
    ensure!(
        policy["selected"] == actual,
        "selected model/provider configuration changed; refresh capabilities and submit a new task"
    );
    let mut roles: std::collections::BTreeSet<_> = settings.executors.keys().cloned().collect();
    roles.extend(step_roles.iter().cloned());
    Ok(Settings {
        executors: roles
            .into_iter()
            .map(|role| (role, executor.clone()))
            .collect(),
        providers: std::collections::BTreeMap::from([(
            executor.provider().into(),
            provider.clone(),
        )]),
        fallbacks: Default::default(),
        ..settings.clone()
    })
}
