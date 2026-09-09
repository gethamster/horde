use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub outputs: BTreeMap<String, String>,
    pub steps: Vec<Step>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Step {
    /// Unknown input names retained only for diagnostics; their values are discarded.
    #[serde(flatten, skip_serializing)]
    #[schemars(skip)]
    pub ignored_fields: BTreeMap<String, serde::de::IgnoredAny>,
    /// Unique workflow-local name for this step, not a tool/function name.
    #[schemars(length(min = 1))]
    pub id: String,
    /// Seconds without durable progress before the daemon ends an attempt.
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub step_budget_seconds: Option<u64>,
    /// Disable the progress budget for this command step; command timeout still applies.
    #[serde(default)]
    pub step_budget_exempt: bool,
    /// Configured executor role (defaults to worker). Must exist for planner proposals.
    #[serde(default = "worker")]
    pub role: String,
    /// Execution kind. Planner proposals cannot request delivery.
    #[serde(default = "agent")]
    #[schemars(extend("enum" = ["agent", "command", "delivery", "simulated", "environment"]))]
    pub kind: String,
    /// Assignment for this step; use ${dependency.result} for a direct dependency's output.
    #[serde(default)]
    pub instructions: String,
    /// Criteria that must pass before the step is accepted.
    #[serde(default)]
    pub acceptance: Vec<String>,
    /// Workflow-local IDs of dependencies, not task UUIDs. Planner dependency is added automatically.
    #[serde(default)]
    pub needs: Vec<String>,
    /// Repository-relative files/directory prefixes to claim for writes; use . for the whole repo.
    #[serde(default)]
    pub scope: Vec<String>,
    /// Allowed native tools, e.g. read_file, search, write_file, apply_patch, command.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Pinned skill names to include in this step prompt. Names must be available in the task catalog.
    #[serde(default)]
    pub skills: Vec<String>,
    /// Repository-relative artifact paths to collect after execution.
    #[serde(default)]
    pub artifacts: Vec<String>,
    /// Extra result field names mapped to string, number, integer, boolean, array, object, or null.
    #[serde(default)]
    #[schemars(extend("additionalProperties" = {"type":"string","enum":["string","number","integer","boolean","array","object","null"]}))]
    pub output_types: BTreeMap<String, String>,
    /// Command working directory: isolated integrated worktree by default, or the live checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<CommandWorkspace>,
    /// Command argv for a command step, not a shell string.
    #[serde(default)]
    pub command: Vec<String>,
    /// Required when kind is environment; describes the disposable app and its test.
    #[serde(default)]
    pub environment: Option<crate::environment::Environment>,
    /// Nested template name, expanded only during template compilation. Submit expanded steps to revision tools.
    #[serde(default)]
    pub template: Option<String>,
    /// String substitutions for nested template compilation.
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    /// Optional condition on a direct dependency's terminal status.
    #[serde(default)]
    pub when: Option<Condition>,
    /// Maximum number of attempts, from 1 through 20.
    #[serde(default = "one")]
    #[schemars(range(min = 1, max = 20))]
    pub attempts: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommandWorkspace {
    Worktree,
    Checkout,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    /// Workflow-local ID that must also appear in needs.
    pub step: String,
    #[schemars(extend("enum" = ["succeeded", "failed", "skipped"]))]
    pub status: String,
}

/// Inline nested schemas so MCP and native providers receive a self-contained contract.
pub fn step_schema() -> Value {
    static SCHEMA: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    SCHEMA
        .get_or_init(|| {
            let settings =
                schemars::generate::SchemaSettings::draft07().with(|s| s.inline_subschemas = true);
            let mut schema =
                serde_json::to_value(settings.into_generator().into_root_schema_for::<Step>())
                    .expect("Step schema serializes");
            schema
                .as_object_mut()
                .expect("Step object schema")
                .remove("$schema");
            // Environment's Serde defaults support TOML merging, but runtime
            // validation requires a nonempty test argv whenever it is supplied.
            schema["properties"]["environment"]["required"] = serde_json::json!(["test"]);
            schema["properties"]["environment"]["properties"]["test"]
                .as_object_mut()
                .expect("test schema")
                .remove("default");
            schema
        })
        .clone()
}

/// Report the nested input path without changing the accepted Step representation.
pub fn parse_steps(value: &Value) -> Result<Vec<Step>> {
    let steps: Vec<Step> = serde_path_to_error::deserialize(value.clone()).map_err(|error| {
        let path = error.path().to_string();
        let suffix = if path == "." { String::new() } else { path };
        anyhow::anyhow!("steps{suffix}: {}", error.inner())
    })?;
    for warning in step_warnings(&steps) {
        eprintln!("warning: {warning}");
    }
    Ok(steps)
}

pub fn step_warnings(steps: &[Step]) -> Vec<String> {
    steps
        .iter()
        .filter(|s| !s.ignored_fields.is_empty())
        .map(|s| {
            format!(
                "step {}: ignored unknown fields: {}",
                s.id,
                s.ignored_fields
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect()
}

fn worker() -> String {
    "worker".into()
}
fn agent() -> String {
    "agent".into()
}
fn one() -> u32 {
    1
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    pub steps: Vec<Step>,
    pub pins: BTreeMap<String, String>,
    pub outputs: BTreeMap<String, String>,
}
pub fn load_templates(dir: &Path) -> Result<BTreeMap<String, Template>> {
    let mut result = BTreeMap::new();
    for source in [
        include_str!("../templates/local-implementation.toml"),
        include_str!("../templates/nextjs.toml"),
        include_str!("../templates/github-actions.toml"),
        include_str!("../templates/simulated.toml"),
    ] {
        let t: Template = toml::from_str(source)?;
        result.insert(t.name.clone(), t);
    }
    if dir.exists() {
        for entry in std::fs::read_dir(dir)? {
            let p = entry?.path();
            if p.extension().is_some_and(|x| x == "toml") {
                let t: Template = toml::from_str(&std::fs::read_to_string(&p)?)?;
                for warning in step_warnings(&t.steps) {
                    eprintln!("warning: {}: {warning}", p.display());
                }
                result.insert(t.name.clone(), t);
            }
        }
    }
    Ok(result)
}
fn render(s: &str, inputs: &BTreeMap<String, String>) -> String {
    let mut s = s.to_owned();
    for (k, v) in inputs {
        s = s.replace(&format!("{{{{{k}}}}}"), v);
    }
    s
}
pub fn compile(
    name: &str,
    templates: &BTreeMap<String, Template>,
    inputs: BTreeMap<String, String>,
) -> Result<Plan> {
    let mut plan = Plan {
        warnings: vec![],
        steps: vec![],
        pins: BTreeMap::new(),
        outputs: BTreeMap::new(),
    };
    expand(name, "", templates, &inputs, &mut vec![], &mut plan)?;
    for step in &mut plan.steps {
        for (alias, target) in &plan.outputs {
            step.instructions = step
                .instructions
                .replace(&format!("${{{alias}}}"), &format!("${{{target}}}"));
            for arg in &mut step.command {
                *arg = arg.replace(&format!("${{{alias}}}"), &format!("${{{target}}}"));
            }
        }
    }
    validate(&plan.steps)?;
    Ok(plan)
}
fn expand(
    name: &str,
    prefix: &str,
    all: &BTreeMap<String, Template>,
    inputs: &BTreeMap<String, String>,
    stack: &mut Vec<String>,
    plan: &mut Plan,
) -> Result<Vec<String>> {
    if stack.iter().any(|n| n == name) {
        bail!(
            "recursive template inclusion: {} -> {name}",
            stack.join(" -> ")
        );
    }
    let t = all
        .get(name)
        .with_context(|| format!("unknown template {name}"))?;
    for i in &t.inputs {
        if !inputs.contains_key(i) {
            bail!("missing input {i} for {name}");
        }
    }
    stack.push(name.into());
    plan.pins.insert(
        format!("{}@{}", name, t.version),
        hex::encode(Sha256::digest(serde_json::to_vec(t)?)),
    );
    let mut aliases: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // References may point forward; resolve nested aliases after all expansion.
    let start = plan.steps.len();
    for warning in step_warnings(&t.steps) {
        plan.warnings.push(format!("template {name}: {warning}"));
    }
    for original in &t.steps {
        let mut s = original.clone();
        s.id = format!("{prefix}{}", s.id);
        s.needs = s.needs.iter().map(|n| format!("{prefix}{n}")).collect();
        if let Some(c) = &mut s.when {
            c.step = format!("{prefix}{}", c.step);
        }
        s.instructions = render(&s.instructions, inputs).replace("${", &format!("${{{prefix}"));
        s.acceptance = s.acceptance.iter().map(|x| render(x, inputs)).collect();
        s.command = s
            .command
            .iter()
            .map(|x| render(x, inputs).replace("${", &format!("${{{prefix}")))
            .collect();
        if let Some(nested) = &s.template {
            if s.workspace.is_some() {
                bail!(
                    "step {}.workspace: set workspace on the command inside the nested template",
                    s.id
                );
            }
            if s.step_budget_exempt {
                bail!(
                    "step {:?}: set step_budget_exempt on command steps, not template inclusions",
                    s.id
                );
            }
            if s.when.is_some() {
                bail!("put conditional execution on child steps, not template inclusion");
            }
            let child_inputs = s
                .inputs
                .iter()
                .map(|(k, v)| (k.clone(), render(v, inputs)))
                .collect();
            let child_start = plan.steps.len();
            let ends = expand(
                nested,
                &format!("{}.", s.id),
                all,
                &child_inputs,
                stack,
                plan,
            )?;
            for child in &mut plan.steps[child_start..] {
                if !child.step_budget_exempt {
                    child.step_budget_seconds = child.step_budget_seconds.or(s.step_budget_seconds);
                }
                if child.kind == "agent" {
                    child.skills.extend(s.skills.clone());
                    child.skills.sort();
                    child.skills.dedup();
                }
                if child.needs.is_empty() {
                    child.needs.extend(s.needs.clone());
                }
            }
            aliases.insert(s.id, ends);
        } else {
            plan.steps.push(s);
        }
    }
    for s in &mut plan.steps[start..] {
        s.needs = s
            .needs
            .iter()
            .flat_map(|n| aliases.get(n).cloned().unwrap_or_else(|| vec![n.clone()]))
            .collect();
    }
    let referenced: BTreeSet<_> = plan.steps[start..]
        .iter()
        .flat_map(|s| s.needs.iter().cloned())
        .collect();
    let ends = plan.steps[start..]
        .iter()
        .filter(|s| !referenced.contains(&s.id))
        .map(|s| s.id.clone())
        .collect();
    for (k, v) in &t.outputs {
        let (step, field) = v
            .rsplit_once('.')
            .context("output must reference step.field")?;
        let id = format!("{prefix}{step}");
        if !plan.steps[start..].iter().any(|s| s.id == id) && !aliases.contains_key(&id) {
            bail!("unknown output step {id}");
        }
        let target = aliases.get(&id).and_then(|v| v.last()).unwrap_or(&id);
        plan.outputs
            .insert(format!("{prefix}{k}"), format!("{target}.{field}"));
    }
    stack.pop();
    Ok(ends)
}
pub fn validate(steps: &[Step]) -> Result<()> {
    if steps.is_empty() {
        bail!("workflow must contain steps");
    }
    let ids: BTreeSet<_> = steps.iter().map(|s| s.id.as_str()).collect();
    let mut seen = BTreeSet::new();
    for (index, s) in steps.iter().enumerate() {
        let path = format!("workflow.steps[{index}] (id={:?})", s.id);
        if !seen.insert(&s.id) {
            bail!(
                "{path}.id: duplicate step id {}; choose a unique workflow-local ID",
                s.id
            );
        }
        if s.id.is_empty() {
            bail!("{path}.id: must not be empty");
        }
        if s.attempts == 0 || s.attempts > 20 {
            bail!("{path}.attempts: must be between 1 and 20");
        }
        if !["agent", "command", "delivery", "simulated", "environment"].contains(&s.kind.as_str())
        {
            bail!(
                "{path}.kind: unknown kind {}; use agent, command, delivery, simulated, or environment",
                s.kind
            );
        }
        if s.workspace.is_some()
            && (s.kind != "command" || s.environment.is_some() || s.template.is_some())
        {
            bail!("{path}.workspace: only plain command steps can select a workspace");
        }
        if let Some(e) = &s.environment {
            e.validate()
                .map_err(|e| anyhow::anyhow!("{path}.environment: {e:#}"))?;
        }
        if s.step_budget_exempt && s.kind != "command" {
            bail!(
                "{path}.step_budget_exempt: only command steps can opt out of the progress budget"
            );
        }
        if s.step_budget_exempt && s.step_budget_seconds.is_some() {
            bail!("{path}: choose step_budget_exempt or step_budget_seconds, not both");
        }
        if s.step_budget_seconds == Some(0) {
            bail!("{path}.step_budget_seconds: must be positive");
        }
        if s.kind == "environment" && s.environment.is_none() {
            bail!("{path}.environment: environment step requires environment configuration");
        }
        for (field, text) in std::iter::once(("instructions".to_owned(), &s.instructions)).chain(
            s.command
                .iter()
                .enumerate()
                .map(|(i, text)| (format!("command[{i}]"), text)),
        ) {
            (|| -> Result<()> {
                let mut rest = text.as_str();
                while let Some(start) = rest.find("${") {
                    rest = &rest[start + 2..];
                    let end = rest.find('}').context("unterminated output reference")?;
                    let (step, field) = rest[..end]
                        .rsplit_once('.')
                        .context("output reference must be step.field")?;
                    if field.is_empty() || !s.needs.iter().any(|n| n == step) {
                        bail!(
                            "output reference must name a direct dependency: {}",
                            &rest[..end]
                        );
                    }
                    let producer = steps.iter().find(|s| s.id == step).context("output step")?;
                    if ![
                        "result",
                        "accepted",
                        "artifacts",
                        "usage",
                        "integration",
                        "events_artifact",
                        "latency_ms",
                        "process",
                    ]
                    .contains(&field)
                        && !producer.output_types.contains_key(field)
                    {
                        bail!("undeclared output {step}.{field}");
                    }
                    rest = &rest[end + 1..];
                }
                Ok(())
            })()
            .map_err(|e| anyhow::anyhow!("{path}.{field}: {e:#}; value={text:?}"))?;
        }
        for ty in s.output_types.values() {
            if ![
                "string", "number", "integer", "boolean", "array", "object", "null",
            ]
            .contains(&ty.as_str())
            {
                bail!(
                    "{path}.output_types: unknown output type {ty}; use string, number, integer, boolean, array, object, or null"
                );
            }
        }
        for p in &s.scope {
            crate::store::scope(p).map_err(|e| anyhow::anyhow!("{path}.scope: {e:#}"))?;
        }
        for n in &s.needs {
            if !ids.contains(n.as_str()) {
                bail!("{path}.needs: unknown dependency {n}; use an existing or proposed step ID");
            }
        }
        if let Some(c) = &s.when
            && (!s.needs.contains(&c.step)
                || !["succeeded", "failed", "skipped"].contains(&c.status.as_str()))
        {
            bail!(
                "{path}.when: condition must reference a dependency and terminal status (succeeded, failed, or skipped)"
            );
        }
    }
    let mut done = BTreeSet::new();
    loop {
        let before = done.len();
        for s in steps {
            if s.needs.iter().all(|n| done.contains(n)) {
                done.insert(s.id.clone());
            }
        }
        if done.len() == steps.len() {
            return Ok(());
        }
        if before == done.len() {
            bail!("workflow dependency cycle");
        }
    }
}
pub fn resolve_refs(text: &str, outputs: &BTreeMap<String, Value>) -> Result<String> {
    let mut result = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        result.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        let end = rest.find('}').context("unterminated output reference")?;
        let key = &rest[..end];
        let value = outputs
            .get(key)
            .with_context(|| format!("missing output {key}"))?;
        result.push_str(
            &value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string()),
        );
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    Ok(result)
}

pub fn validate_result(step: &Step, result: &Value) -> Result<()> {
    for (name, ty) in &step.output_types {
        let v = result
            .get(name)
            .with_context(|| format!("missing declared output {name}"))?;
        let valid = match ty.as_str() {
            "string" => v.is_string(),
            "number" => v.is_number(),
            "integer" => v.is_i64() || v.is_u64(),
            "boolean" => v.is_boolean(),
            "array" => v.is_array(),
            "object" => v.is_object(),
            "null" => v.is_null(),
            _ => false,
        };
        if !valid {
            bail!("output {name} must be {ty}");
        }
    }
    Ok(())
}
