use crate::store::Store;
use anyhow::Result;
use serde_json::{Value, json};
#[derive(Default)]
struct Usage {
    input: u64,
    output: u64,
    cached: u64,
    cost: f64,
    reported: bool,
    cost_reported: bool,
}
fn usage(v: &Value) -> Usage {
    let mut result = Usage::default();
    if let Some(requests) = v["requests"].as_array() {
        for v in requests {
            let u = usage(v);
            result.input += u.input;
            result.output += u.output;
            result.cached += u.cached;
            result.cost += u.cost;
            result.reported |= u.reported;
            result.cost_reported |= u.cost_reported;
        }
    }
    if v["provider"].is_object() {
        result = usage(&v["provider"]);
    }
    if let Some(n) = v["input_tokens"]
        .as_u64()
        .or_else(|| v["prompt_tokens"].as_u64())
    {
        result.input += n;
        result.reported = true;
    }
    if let Some(n) = v["output_tokens"]
        .as_u64()
        .or_else(|| v["completion_tokens"].as_u64())
    {
        result.output += n;
        result.reported = true;
    }
    result.cached += v["cached_input_tokens"]
        .as_u64()
        .or_else(|| v["cache_read_input_tokens"].as_u64())
        .or_else(|| v["prompt_tokens_details"]["cached_tokens"].as_u64())
        .unwrap_or(0);
    if let Some(n) = v["api_cost_usd"]
        .as_f64()
        .or_else(|| v["total_cost_usd"].as_f64())
    {
        result.cost = n;
        result.cost_reported = true;
    }
    result
}
pub fn report(db: &Store, oid: &str) -> Result<Value> {
    let task = db.task(oid)?;
    let attempts = crate::budget::annotate(db, db.rows(
        "SELECT a.*,t.spec FROM attempts a JOIN steps t ON a.step=t.id WHERE t.task=? ORDER BY a.started,a.rowid",
        &[&oid],
    )?)?;
    let (
        mut input,
        mut output,
        mut cached,
        mut cost,
        mut unreported,
        mut unknown_cost,
        mut frontier,
    ) = (0, 0, 0, 0.0, 0, 0, 0);
    let mut turns = vec![];
    let mut finished = task["created"].as_i64().unwrap_or(0);
    for a in &attempts {
        let v: Value = a["usage"]
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        if let Some(requests) = v["requests"].as_array() {
            for (index, request) in requests.iter().enumerate() {
                turns.push(
                    json!({"attempt":a["id"],"step":a["step"],"turn":index + 1,"usage":request}),
                );
            }
        }
        let u = usage(&v);
        input += u.input;
        output += u.output;
        cached += u.cached;
        cost += u.cost;
        if !u.reported {
            unreported += 1;
        }
        if !u.cost_reported {
            unknown_cost += 1;
        }
        let spec: Value = serde_json::from_str(a["spec"].as_str().unwrap_or("{}"))?;
        if ["planner", "reviewer"].contains(
            &v["executor_role"]
                .as_str()
                .or_else(|| spec["role"].as_str())
                .unwrap_or(""),
        ) {
            frontier += u.input + u.output;
        }
        finished = finished.max(a["finished"].as_i64().unwrap_or_else(crate::store::now));
    }
    let count = |sql: &str| -> Result<i64> { Ok(db.conn.query_row(sql, [oid], |r| r.get(0))?) };
    let steps = count("SELECT COUNT(*) FROM steps WHERE task=?")?;
    let mut result = json!({"task":oid,"status":task["status"],"accepted_tasks":i64::from(task["status"]=="succeeded"),"attempts":attempts.len(),"retries":(attempts.len() as i64-steps).max(0),"reported_input_tokens":input,"reported_output_tokens":output,"reported_cached_tokens":cached,"planner_reviewer_reported_tokens":frontier,"reported_api_cost_usd":if unknown_cost==0{json!(cost)}else{Value::Null},"known_api_cost_subtotal_usd":cost,"attempts_without_token_usage":unreported,"attempts_without_cost":unknown_cost,"subscription_capacity":null,"elapsed_seconds":finished-task["created"].as_i64().unwrap_or(0),"coordination_messages":count("SELECT COUNT(*) FROM messages WHERE task=?")?,"coordination_tool_calls":count("SELECT COUNT(*) FROM events WHERE task=? AND kind='coordination.call'")?,"human_answers":count("SELECT COUNT(*) FROM questions WHERE task=? AND answer IS NOT NULL")? });
    result["turns"] = json!(turns);
    let mut step_metrics = vec![];
    for step in db.steps(oid)? {
        let id = step["id"].as_str().unwrap_or("");
        let runs: Vec<_> = attempts.iter().filter(|a| a["step"] == id).collect();
        let started = runs.iter().filter_map(|a| a["started"].as_i64()).min();
        let finished = runs
            .iter()
            .map(|a| a["finished"].as_i64().unwrap_or_else(crate::store::now))
            .max();
        let mut input = 0;
        let mut output = 0;
        let mut cached = 0;
        for a in &runs {
            let value = a["usage"]
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .unwrap_or(Value::Null);
            let u = usage(&value);
            input += u.input;
            output += u.output;
            cached += u.cached;
        }
        let tool_calls: i64 = db.conn.query_row("SELECT COUNT(*) FROM events WHERE task=? AND kind='tool.completed' AND json_extract(data,'$.step')=?",rusqlite::params![oid,id],|r|r.get(0))?;
        let coordination_calls: i64 = db.conn.query_row("SELECT COUNT(*) FROM events WHERE task=? AND kind='coordination.call' AND json_extract(data,'$.step')=?",rusqlite::params![oid,id],|r|r.get(0))?;
        step_metrics.push(json!({"step":id,"name":step["name"],"state":step["state"],
            "elapsed_seconds":started.zip(finished).map(|(s,f)|(f-s).max(0)),
            "attempt_elapsed_seconds":runs.iter().filter_map(|a|a["timing"]["elapsed_s"].as_f64()).sum::<f64>(),
            "reported_input_tokens":input,"reported_output_tokens":output,"reported_cached_tokens":cached,"tool_calls":tool_calls,"recorded_coordination_calls":coordination_calls,
            "attempts":runs.iter().map(|a|json!({"attempt":a["id"],"state":a["state"],"timing":a["timing"]})).collect::<Vec<_>>() }));
    }
    result["steps"] = json!(step_metrics);
    if let Some(remote) = db
        .rows(
            "SELECT data FROM external_ops WHERE task=? AND name='federation.metrics'",
            &[&oid],
        )?
        .first()
    {
        result = serde_json::from_str(remote["data"].as_str().unwrap_or("{}"))?;
        result["task"] = json!(oid);
        result["status"] = task["status"].clone();
        result["execution_location"] = json!("remote");
    }
    let children = db.rows("SELECT task FROM task_tree WHERE parent=?", &[&oid])?;
    result["children"] = json!(
        children
            .iter()
            .map(|c| report(db, c["task"].as_str().unwrap_or("")))
            .collect::<Result<Vec<_>>>()?
    );
    Ok(result)
}

pub fn totals(value: &Value) -> (Option<u64>, Option<f64>) {
    let u = usage(value);
    (
        u.reported.then_some(u.input + u.output),
        u.cost_reported.then_some(u.cost),
    )
}
