use super::*;
use rusqlite::OptionalExtension;
const BINDING: &str = "upstream-maintenance.binding";
#[derive(Default, Serialize, Deserialize)]
struct Usage {
    #[serde(default)]
    fenced: bool,
    model_calls: u64,
    input_bytes: u64,
    output_bytes: u64,
}
pub fn binding(db: &Store, task: &str) -> Result<Option<(String, Binding)>> {
    // Historical diagnostic fixtures and records may predate task-tree metadata.
    // Absence of a maintenance binding must leave ordinary reads unchanged.
    let root: String = db.conn.query_row(
        "SELECT COALESCE((SELECT root FROM task_tree WHERE task=?), ?)",
        rusqlite::params![task, task],
        |row| row.get(0),
    )?;
    let raw: Option<String> = db
        .conn
        .query_row(
            "SELECT data FROM external_ops WHERE task=? AND name=?",
            rusqlite::params![root, BINDING],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|raw| Ok((root, serde_json::from_str(&raw)?)))
        .transpose()
}
pub fn pin(db: &Store, task: &str, binding: &Binding) -> Result<()> {
    let project = crate::projects::task_project(db, task)?;
    let taskrow = db.task(task)?;
    let repo = Path::new(taskrow["repo"].as_str().context("task repo")?);
    validate_binding(db, binding, &project, repo)?;
    db.conn.execute(
        "INSERT INTO external_ops(task,name,state,data) VALUES(?,?,'pinned',?)",
        rusqlite::params![task, BINDING, serde_json::to_string(binding)?],
    )?;
    db.event(task,"maintenance.bound",json!({"policy_id":binding.policy_id,"policy_revision":binding.policy_revision,"routine_id":binding.routine_id,"occurrence_id":binding.occurrence_id,"invocation_id":binding.invocation_id,"budget_reference":binding.budget.reference}))?;
    Ok(())
}
pub fn check_local(db: &Store, task: &str) -> Result<Option<Policy>> {
    let Some((root, binding)) = binding(db, task)? else {
        return Ok(None);
    };
    let project = crate::projects::task_project(db, &root)?;
    ensure!(
        crate::projects::task_project(db, task)? == project,
        "maintenance descendant project mismatch"
    );
    let row = db.task(&root)?;
    let policy = validate_binding(
        db,
        &binding,
        &project,
        Path::new(row["repo"].as_str().context("root repository")?),
    )?;
    ensure!(
        db.task(task)?["status"] != "cancelled" && row["status"] != "cancelled",
        "maintenance task cancelled"
    );
    ensure!(
        !usage(db, &binding)?.fenced,
        "maintenance worker execution budget exhausted"
    );
    Ok(Some(policy))
}
fn usage_key(binding: &Binding) -> Result<String> {
    Ok(format!(
        "upstream-maintenance.usage:{}",
        crate::store::hash(&serde_json::to_vec(&(
            binding.policy_id.clone(),
            binding.policy_revision,
            binding.invocation_id.clone(),
            binding.budget.reference.clone()
        ))?)
    ))
}
fn usage(db: &Store, binding: &Binding) -> Result<Usage> {
    management::value(db, &usage_key(binding)?)?
        .map(|v| serde_json::from_str(&v).context("maintenance budget usage"))
        .unwrap_or_else(|| Ok(Usage::default()))
}
fn reserve(db: &Store, task: &str, calls: u64, input: u64, output: u64) -> Result<()> {
    let allowed = db.atomic(|| {
        check_local(db, task)?;
        let Some((_, binding)) = binding(db, task)? else {
            return Ok(true);
        };
        let current = usage(db, &binding)?;
        let model_calls = current.model_calls.checked_add(calls);
        let input_bytes = current.input_bytes.checked_add(input);
        let output_bytes = current.output_bytes.checked_add(output);
        let allowed = model_calls.is_some_and(|v| v <= binding.budget.max_model_calls)
            && input_bytes.is_some_and(|v| v <= binding.budget.max_input_bytes)
            && output_bytes.is_some_and(|v| v <= binding.budget.max_output_bytes);
        let next = if allowed {
            Usage {
                fenced: false,
                model_calls: model_calls.unwrap(),
                input_bytes: input_bytes.unwrap(),
                output_bytes: output_bytes.unwrap(),
            }
        } else {
            Usage {
                fenced: true,
                ..current
            }
        };
        management::set(db, &usage_key(&binding)?, &serde_json::to_string(&next)?)?;
        // Commit the fence before returning an error: transaction rollback must not
        // let a command or delivery retry spend authority after attempted overrun.
        Ok(allowed)
    })?;
    ensure!(allowed, "maintenance worker execution budget exhausted");
    Ok(())
}

pub fn reserve_model(db: &Store, task: &str, input_bytes: usize) -> Result<()> {
    if binding(db, task)?.is_none() {
        return Ok(());
    }
    reserve(db, task, 1, u64::try_from(input_bytes)?, 0)
}
pub fn consume_output(db: &Store, task: &str, output_bytes: usize) -> Result<()> {
    if binding(db, task)?.is_none() {
        return Ok(());
    }
    reserve(db, task, 0, 0, u64::try_from(output_bytes)?)
}
pub fn inspection(db: &Store, task: &str) -> Result<Value> {
    let Some((root, binding)) = binding(db, task)? else {
        return Ok(Value::Null);
    };
    let active = check_local(db, task).is_ok();
    Ok(
        json!({"root_task":root,"binding":binding,"usage":usage(db,&binding)?,"active":active,"package_release_supported":true,"package_release_verified":db.rows("SELECT name FROM external_ops WHERE task=? AND name='package-release.verified' AND state='succeeded'", &[&root])?.len()==1}),
    )
}
