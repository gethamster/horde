use anyhow::{Context, Result, bail};
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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    #[serde(default = "worker")]
    pub role: String,
    #[serde(default = "agent")]
    pub kind: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub acceptance: Vec<String>,
    #[serde(default)]
    pub needs: Vec<String>,
    #[serde(default)]
    pub scope: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub output_types: BTreeMap<String, String>,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub environment: Option<crate::environment::Environment>,
    #[serde(default)]
    pub template: Option<String>,
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
    #[serde(default)]
    pub when: Option<Condition>,
    #[serde(default = "one")]
    pub attempts: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    pub step: String,
    pub status: String,
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
    if ids.len() != steps.len() {
        bail!("duplicate step id");
    }
    for s in steps {
        if s.id.is_empty() || s.attempts == 0 || s.attempts > 20 {
            bail!("invalid step or attempts");
        }
        if !["agent", "command", "delivery", "simulated", "environment"].contains(&s.kind.as_str())
        {
            bail!("unknown kind {}", s.kind);
        }
        if let Some(e) = &s.environment {
            e.validate()?;
        }
        if s.kind == "environment" && s.environment.is_none() {
            bail!("environment step requires environment configuration");
        }
        for text in std::iter::once(&s.instructions).chain(s.command.iter()) {
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
        }
        for ty in s.output_types.values() {
            if ![
                "string", "number", "integer", "boolean", "array", "object", "null",
            ]
            .contains(&ty.as_str())
            {
                bail!("unknown output type {ty}");
            }
        }
        for p in &s.scope {
            crate::store::scope(p)?;
        }
        for n in &s.needs {
            if !ids.contains(n.as_str()) {
                bail!("unknown dependency {n}");
            }
        }
        if let Some(c) = &s.when
            && (!s.needs.contains(&c.step)
                || !["succeeded", "failed", "skipped"].contains(&c.status.as_str()))
        {
            bail!("condition must reference a dependency and terminal status");
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
