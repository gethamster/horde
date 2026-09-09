use super::worker_allowed;
use crate::template;
use serde_json::{Value, json};

/// Published worker tools omit fields supplied or controlled by the runtime.
pub fn schema(name: &str) -> Value {
    let mut schema = admin_schema(name);
    if worker_allowed(name) {
        let owned = |key: &str| {
            ["task", "worker", "step", "verified"].contains(&key) || key.starts_with('_')
        };
        schema["properties"]
            .as_object_mut()
            .expect("object schema")
            .retain(|key, _| !owned(key));
        schema["required"]
            .as_array_mut()
            .expect("required fields")
            .retain(|key| !owned(key.as_str().expect("field name")));
    }
    schema
}
/// Operator tools keep explicit task selection and verification controls.
pub fn admin_schema(name: &str) -> Value {
    let fields: &[(&str, &str)] = match name {
        "skill_pack_list" => &[],
        "skill_pack_install" => &[("path", "string")],
        "runtime_skills_update" => &[("id", "string"), ("request_id", "string")],
        "runtime_capabilities" => &[("task", "string")],
        "plan_execution" => &[("task", "string"), ("roles", "object")],
        "agent_setup" => &[
            ("action", "string"),
            ("provider", "string"),
            ("roles", "array"),
            ("model", "string"),
            ("kind", "string"),
            ("auth_mode", "string"),
            ("base_url", "string"),
            ("api_key_env", "string"),
            ("program", "string"),
            ("credential_env", "string"),
            ("credential_file", "string"),
            ("name", "string"),
            ("invitation_file", "string"),
            ("output_file", "string"),
            ("max_workers", "integer"),
            ("expires_in", "integer"),
            ("concurrency", "integer"),
            ("no_start", "boolean"),
            ("explicit_root", "boolean"),
        ],
        "skill_inspect" => &[("repo", "string"), ("name", "string")],
        "skill_propose" => &[
            ("repo", "string"),
            ("name", "string"),
            ("content", "string"),
            ("reason", "string"),
            ("expected_hash", "string"),
        ],
        "skill_apply" => &[
            ("repo", "string"),
            ("proposal_id", "string"),
            ("accepted", "boolean"),
        ],
        "skill_history" => &[
            ("repo", "string"),
            ("name", "string"),
            ("limit", "integer"),
            ("before_revision", "integer"),
        ],
        "skill_rollback" => &[
            ("repo", "string"),
            ("name", "string"),
            ("revision", "integer"),
            ("reason", "string"),
        ],
        "runtime_updates_resume" => &[],
        "runtime_reconcile" => &[
            ("id", "string"),
            ("request_id", "string"),
            ("resource", "string"),
        ],
        "runtime_list" => &[],
        "runtime_rename" => &[("id", "string"), ("name", "string")],
        "runtime_forget" => &[("id", "string")],
        "runtime_inspect" => &[("id", "string")],
        "runtime_create" | "runtime_destroy" | "runtime_restart" | "runtime_update"
        | "runtime_stop" | "runtime_start" => &[
            ("id", "string"),
            ("request_id", "string"),
            ("profile", "string"),
            ("version", "string"),
        ],
        "runtime_config_get" | "runtime_drain" | "runtime_resume" | "runtime_status"
        | "account_status" => &[],
        "runtime_config_set" => &[("concurrency", "integer")],
        "account_observe" => &[
            ("account", "string"),
            ("provider", "string"),
            ("window", "string"),
            ("used_percent", "number"),
            ("reset_at", "integer"),
            ("observed_at", "integer"),
            ("source", "string"),
        ],
        "management_events" => &[("after", "integer")],
        "management_ack" => &[("consumer", "string"), ("seq", "integer")],
        "remote_result" => &[("task", "string")],
        "submit_task" => &[
            ("request_id", "string"),
            ("execution", "object"),
            ("on", "string"),
            ("context", "array"),
            ("objective", "string"),
            ("repo", "string"),
            ("template", "string"),
        ],
        "send_message" => &[
            ("task", "string"),
            ("worker", "string"),
            ("id", "string"),
            ("destination", "string"),
            ("body", "string"),
            ("refs", "object"),
            ("actionable", "boolean"),
        ],
        "read_messages" => &[
            ("task", "string"),
            ("worker", "string"),
            ("after", "integer"),
            ("limit", "integer"),
        ],
        "acknowledge_messages" => &[("task", "string"), ("worker", "string"), ("ids", "array")],
        "register_workspace" => &[
            ("task", "string"),
            ("worker", "string"),
            ("path", "string"),
            ("branch", "string"),
            ("base", "string"),
        ],
        "claim_paths" => &[("task", "string"), ("worker", "string"), ("paths", "array")],
        "transfer_claim" => &[
            ("task", "string"),
            ("worker", "string"),
            ("to", "string"),
            ("path", "string"),
        ],
        "set_worker_status" => &[
            ("task", "string"),
            ("worker", "string"),
            ("status", "string"),
        ],
        "join_channel" => &[
            ("task", "string"),
            ("worker", "string"),
            ("channel", "string"),
        ],
        "put_artifact" => &[
            ("task", "string"),
            ("worker", "string"),
            ("step", "string"),
            ("name", "string"),
            ("content", "string"),
            ("inputs", "object"),
            ("verified", "boolean"),
        ],
        "get_artifact" => &[("task", "string"), ("hash", "string")],
        "reuse_artifact" => &[("task", "string"), ("name", "string"), ("inputs", "object")],
        "add_knowledge" => &[
            ("task", "string"),
            ("step", "string"),
            ("kind", "string"),
            ("content", "string"),
            ("provenance", "object"),
            ("inputs", "object"),
            ("verified", "boolean"),
        ],
        "link_knowledge" => &[
            ("task", "string"),
            ("source", "string"),
            ("target", "string"),
            ("relation", "string"),
        ],
        "integrate_child" => &[
            ("task", "string"),
            ("worker", "string"),
            ("child", "string"),
            ("validation", "array"),
        ],
        "delegate_task" => &[
            ("execution", "object"),
            ("task", "string"),
            ("worker", "string"),
            ("id", "string"),
            ("objective", "string"),
            ("template", "string"),
            ("peer", "string"),
            ("bundles", "array"),
            ("skills", "array"),
        ],
        "read_skill" => &[
            ("task", "string"),
            ("worker", "string"),
            ("name", "string"),
            ("path", "string"),
            ("offset", "integer"),
            ("limit", "integer"),
        ],
        "read_context" => &[
            ("task", "string"),
            ("after", "integer"),
            ("limit", "integer"),
        ],
        "update_context" => &[
            ("task", "string"),
            ("id", "string"),
            ("kind", "string"),
            ("content", "string"),
            ("provenance", "string"),
            ("mandatory", "boolean"),
            ("supersedes", "array"),
        ],
        "escalate_question" => &[
            ("task", "string"),
            ("worker", "string"),
            ("question", "string"),
            ("commentary", "string"),
        ],
        "ack_events" => &[
            ("task", "string"),
            ("consumer", "string"),
            ("seq", "integer"),
        ],
        "request_question" => &[
            ("id", "string"),
            ("evidence", "string"),
            ("recommendation", "string"),
            ("human_only", "boolean"),
            ("task", "string"),
            ("worker", "string"),
            ("question", "string"),
        ],
        "answer_question" => &[
            ("worker", "string"),
            ("human", "boolean"),
            ("task", "string"),
            ("question", "string"),
            ("answer", "string"),
        ],
        "propose_steps" => &[("task", "string"), ("worker", "string"), ("steps", "array")],
        "add_steps" => &[("task", "string"), ("steps", "array")],
        "integrate" => &[
            ("task", "string"),
            ("worker", "string"),
            ("validation", "array"),
        ],
        "register_worker" => &[("task", "string"), ("step", "string")],
        "release_claims" | "reconcile_worker" => &[("task", "string"), ("worker", "string")],
        "events" => &[
            ("task", "string"),
            ("after", "integer"),
            ("consumer", "string"),
        ],
        "list_tasks" => &[],
        _ => &[("task", "string")],
    };
    let properties: serde_json::Map<String, Value> = fields
        .iter()
        .map(|(k, t)| {
            let mut s = json!({"type":t});
            if *k == "execution" {
                s = json!({"type":"object","additionalProperties":false,"required":["allowed","selected"],"properties":{
                    "allowed":{"type":"array","minItems":1,"items":{"type":"object","additionalProperties":false,"required":["runtime","capabilities"],"properties":{"runtime":{"type":"string"},"capabilities":{"type":"array","minItems":1,"items":{"type":"string"}}}}},
                    "selected":{"type":"object","additionalProperties":false,"required":["runtime","capability"],"properties":{"runtime":{"type":"string"},"capability":{"type":"string"}}}
                },"description":"Parent-selected capability and allowed runtime/capability pairs from discovery. Children may narrow this pool; no credentials or global configuration changes."});
            }
            if name == "plan_execution" && *k == "roles" {
                s = json!({"type":"object","minProperties":1,"maxProperties":32,"additionalProperties":{"type":"object","additionalProperties":false,"required":["runtime","models"],"properties":{"runtime":{"type":"string"},"models":{"type":"array","minItems":1,"maxItems":64,"items":{"type":"string"}}}},"description":"Named work roles, each with a machine and an allowed model/executor pool; the parent chooses which member to use."});
            }
            if name == "agent_setup" && *k == "action" {
                s["enum"] = json!(["inspect","verify","configure_provider","configure_controller","create_fleet_key","join_worker","restart_local"]);
            }
            if name == "skill_apply" && *k == "accepted" {
                s["const"] = json!(true);
                s["description"] = json!("Set only after the user agrees to this proposed persistent skill change.");
            }
            if *t == "array" {
                s["items"] = if *k == "steps" {
                    template::step_schema()
                } else if *k == "context" {
                    json!({"type":"object"})
                } else {
                    json!({"type":"string"})
                };
            }
            if *k == "steps" {
                s["description"] = json!("Workflow Step objects. Call this tool with a steps array; step IDs are data, never tool names. Use expanded steps, not nested template invocations.");
                if name == "propose_steps" {
                    s["minItems"] = json!(1);
                    s["maxItems"] = json!(32);
                    s["items"]["properties"]["kind"]["enum"] = json!(["agent", "command", "simulated", "environment"]);
                    s["items"]["properties"]["template"] = json!({"type":"null","description":"Planner proposals must be expanded; omit template."});
                }
            }
            (k.to_string(), s)
        })
        .collect();
    let required: &[&str] = match name {
        "plan_execution" => &["roles"],
        "agent_setup" => &["action"],
        "skill_inspect" => &["repo"],
        "skill_propose" => &["repo", "name", "content"],
        "skill_apply" => &["repo", "proposal_id", "accepted"],
        "skill_history" => &["repo", "name"],
        "skill_rollback" => &["repo", "name", "revision"],
        "runtime_config_set" => &["concurrency"],
        "runtime_create" => &["id", "profile", "request_id"],
        "runtime_inspect" | "runtime_forget" => &["id"],
        "runtime_rename" => &["id", "name"],
        "skill_pack_install" => &["path"],
        "runtime_skills_update" => &["id", "request_id"],
        "runtime_update" => &["id", "request_id", "version"],
        "runtime_destroy" | "runtime_restart" | "runtime_start" | "runtime_stop" => {
            &["id", "request_id"]
        }
        "runtime_reconcile" => &["id", "request_id", "resource"],
        "account_observe" => &["account", "provider", "window", "observed_at", "source"],
        "management_ack" => &["consumer", "seq"],
        "submit_task" => &["objective", "repo"],
        "remote_result" => &["task"],
        "delegate_task" => &["id", "objective"],
        "read_skill" => &["name"],
        "integrate_child" => &["child", "validation"],
        "update_context" => &["content", "provenance"],
        "ack_events" => &["consumer", "seq"],
        "escalate_question" => &["question"],
        "send_message" => &["id", "destination", "body"],
        "request_question" => &["question"],
        "propose_steps" | "add_steps" => &["steps"],
        "register_workspace" => &["path", "branch", "base"],
        "claim_paths" => &["paths"],
        "transfer_claim" => &["to", "path"],
        "acknowledge_messages" => &["ids"],
        "set_worker_status" => &["status"],
        "join_channel" => &["channel"],
        "put_artifact" => &["name", "content"],
        "get_artifact" => &["hash"],
        "reuse_artifact" => &["name", "inputs"],
        "add_knowledge" => &["kind", "content", "provenance"],
        "link_knowledge" => &["source", "target", "relation"],
        "answer_question" => &["question", "answer"],
        _ => &[],
    };
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
