//! Conservative, opt-in pruning of completed native read/search exchanges.
//! The original conversation stays live until every omitted byte is verified in CAS.
use crate::{
    config::{Decision, DecisionMode, NativeContextMode, Settings},
    decision::{Answer, ChoiceQuestion, DecisionHttpClient, DecisionRequest, Question, store},
    native_protocol::Conversation,
    store::Store,
};
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    time::Instant,
};

const KEEP_RECENT: usize = 3;
const MAX_GROUPS: usize = 8;
const MAX_ARCHIVE_BYTES: usize = 4 * 1024 * 1024;
const PAGE_BYTES: usize = 8 * 1024;
const MAX_EVALUATIONS_PER_GROUP: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum NextAction {
    Continue,
    Prune,
    Hold,
}

fn next_action(response: &crate::decision::DecisionResponse, hard_blocker: bool) -> NextAction {
    let selected = response.answers.iter().find_map(|answer| match answer {
        Answer::Choice {
            id,
            answer,
            confidence,
            probabilities,
        } if id == "next_action"
            && *confidence >= 0.95
            && probabilities
                .get(answer)
                .is_some_and(|value| *value >= 0.95) =>
        {
            Some(answer.as_str())
        }
        _ => None,
    });
    match selected {
        Some("prune") => NextAction::Prune,
        Some("hold") if hard_blocker => NextAction::Hold,
        _ => NextAction::Continue,
    }
}

fn proposed_action(
    action: NextAction,
    selected: usize,
    savings: usize,
    total: usize,
    decision: &Decision,
) -> &'static str {
    if action == NextAction::Prune
        && selected > 0
        && savings >= decision.native_context_min_savings_bytes
        && savings * 100 >= total * decision.native_context_min_savings_ratio_percent
    {
        "prune"
    } else if action == NextAction::Hold {
        "hold"
    } else {
        "continue"
    }
}

pub fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS native_context_epochs(
        decision TEXT PRIMARY KEY REFERENCES decisions(id),
        task TEXT NOT NULL REFERENCES tasks(id),
        attempt TEXT NOT NULL REFERENCES attempts(id),
        old_hash TEXT NOT NULL,
        new_hash TEXT NOT NULL,
        archive_hashes TEXT NOT NULL,
        state TEXT NOT NULL CHECK(state IN ('prepared','used')),
        created INTEGER NOT NULL
    );",
    )?;
    Ok(())
}

#[derive(Clone, Debug)]
struct Group {
    start: usize,
    end: usize,
    hash: String,
    bytes: Vec<u8>,
    tool: String,
    argument: String,
    result: String,
    active_safe: bool,
}

fn mentions_protected_context(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    [
        "agents.md",
        "claude.md",
        "skill.md",
        ".horde",
        "contract",
        "acceptance",
        "instructions",
        "policy",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
}

fn short(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}
fn redactions(
    bundle: std::collections::BTreeMap<String, String>,
    decision_key: &str,
    provider_key: &str,
) -> Vec<String> {
    let mut values = bundle
        .into_values()
        .chain([decision_key.to_owned(), provider_key.to_owned()])
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let escaped = values
        .iter()
        .filter_map(|secret| {
            let encoded = serde_json::to_string(secret).ok()?;
            let inner = encoded.get(1..encoded.len().saturating_sub(1))?;
            (inner != secret).then(|| inner.to_owned())
        })
        .collect::<Vec<_>>();
    values.extend(escaped);
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values.dedup();
    values
}
fn redacted_short(value: &str, redactions: &[String], limit: usize) -> String {
    let mut redacted = value.to_owned();
    for secret in redactions {
        redacted = redacted.replace(secret, "[REDACTED]");
    }
    short(&redacted, limit)
}

fn choose_candidates(
    mut groups: Vec<Group>,
    mode: NativeContextMode,
    seen: &HashMap<String, usize>,
) -> Vec<Group> {
    groups.retain(|group| {
        (mode == NativeContextMode::Shadow || group.active_safe)
            && seen.get(&group.hash).copied().unwrap_or(0) < MAX_EVALUATIONS_PER_GROUP
    });
    groups.sort_by(|left, right| {
        right
            .bytes
            .len()
            .cmp(&left.bytes.len())
            .then(left.start.cmp(&right.start))
    });
    groups.truncate(MAX_GROUPS);
    groups.sort_by_key(|group| group.start);
    groups
}

/// Only an entire, contiguous, successful assistant/tool exchange is eligible.
/// Any uncertainty preserves the original messages.
fn groups(messages: &[Value]) -> Result<Vec<Group>> {
    let mut complete = Vec::new();
    let mut index = 2; // Mandatory system and initial user messages.
    while index < messages.len() {
        let assistant = &messages[index];
        let Some(calls) = assistant["tool_calls"].as_array() else {
            index += 1;
            continue;
        };
        let content_empty = assistant["content"].as_str().is_some_and(str::is_empty);
        let narrative = assistant.as_object().is_some_and(|object| {
            object.iter().any(|(key, value)| {
                !["role", "content", "tool_calls"].contains(&key.as_str())
                    && !value.is_null()
                    && value != ""
            })
        });
        if assistant["role"] != "assistant"
            || !content_empty
            || narrative
            || calls.is_empty()
            || calls.len() > 8
        {
            index += 1;
            continue;
        }
        let mut ids = HashSet::new();
        let mut all_read_only = true;
        let mut argument = String::new();
        let mut tool = String::new();
        for call in calls {
            let Some(id) = call["id"].as_str() else {
                all_read_only = false;
                break;
            };
            let Some(name) = call["function"]["name"].as_str() else {
                all_read_only = false;
                break;
            };
            let Some(raw) = call["function"]["arguments"].as_str() else {
                all_read_only = false;
                break;
            };
            let Ok(args) = serde_json::from_str::<Value>(raw) else {
                all_read_only = false;
                break;
            };
            if !ids.insert(id)
                || !["read_file", "search"].contains(&name)
                || (name == "read_file" && args["path"].as_str().is_none())
                || (name == "search" && args["pattern"].as_str().is_none())
            {
                all_read_only = false;
                break;
            }
            tool.push_str(name);
            tool.push(' ');
            argument.push_str(raw);
            argument.push(' ');
        }
        if !all_read_only || index + calls.len() >= messages.len() {
            index += 1;
            continue;
        }
        let outcomes = &messages[index + 1..index + 1 + calls.len()];
        let mut result = String::new();
        let valid = outcomes.iter().zip(calls).all(|(outcome, call)| {
            let Some(raw) = outcome["content"].as_str() else {
                return false;
            };
            let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
                return false;
            };
            if outcome["role"] != "tool"
                || outcome["tool_call_id"] != call["id"]
                || !parsed.is_object()
                || parsed.get("error").is_some()
                || (call["function"]["name"] == "read_file" && parsed["content"].as_str().is_none())
                || (call["function"]["name"] == "search" && parsed["matches"].as_str().is_none())
            {
                return false;
            }
            result.push_str(raw);
            true
        });
        if valid {
            let end = index + calls.len() + 1;
            let bytes = serde_json::to_vec(&messages[index..end])?;
            if bytes.len() <= MAX_ARCHIVE_BYTES {
                let active_safe = calls
                    .iter()
                    .all(|call| call["function"]["name"] == "search")
                    && !mentions_protected_context(&argument)
                    && !mentions_protected_context(&result);
                complete.push(Group {
                    start: index,
                    end,
                    hash: crate::store::hash(&bytes),
                    bytes,
                    tool,
                    argument,
                    result,
                    active_safe,
                });
            }
            index = end;
        } else {
            index += 1;
        }
    }
    complete.truncate(complete.len().saturating_sub(KEEP_RECENT));
    Ok(complete)
}

fn proposed_messages(messages: &[Value], selected: &[Group], references: &[String]) -> Vec<Value> {
    let mut result = Vec::with_capacity(messages.len());
    let mut next = 0;
    for (group, hash) in selected.iter().zip(references) {
        result.extend_from_slice(&messages[next..group.start]);
        result.push(json!({"role":"user","content":format!(
            "An earlier read-only tool exchange was archived as SHA-256 {hash}. If its exact content is needed, call retrieve_native_context with this hash and a byte offset. Never repeat a side-effectful command to recover context."
        )}));
        next = group.end;
    }
    result.extend_from_slice(&messages[next..]);
    result
}

fn authorized(db: &Store, task: &str, decision: &Decision) -> Result<bool> {
    let snapshot: Settings = serde_json::from_str(
        db.task(task)?["settings"]
            .as_str()
            .context("task settings")?,
    )?;
    let project = crate::projects::task_project(db, task)?;
    let current = Settings::load_project_user(db, &project)?;
    Ok(snapshot.decision == *decision
        && current.decision == *decision
        && decision.mode == DecisionMode::Shadow
        && decision.native_context_mode != NativeContextMode::Disabled)
}

/// Called immediately before a native provider request. Decision failure leaves
/// the old conversation intact; the caller checks a configured hard byte ceiling.
pub struct PruningContext<'a> {
    pub db: &'a Store,
    pub task: &'a str,
    pub step: &'a str,
    pub attempt: &'a str,
    pub decision: &'a Decision,
    pub provider_key: &'a str,
}

pub async fn maybe_prune(
    context: PruningContext<'_>,
    conversation: &mut Conversation,
    seen: &mut HashMap<String, usize>,
    current_request_bytes: usize,
) -> Result<()> {
    let PruningContext {
        db,
        task,
        step,
        attempt,
        decision,
        provider_key,
    } = context;
    if decision.native_context_mode == NativeContextMode::Disabled
        || !authorized(db, task, decision)?
    {
        return Ok(());
    }
    let messages = conversation.values()?;
    if current_request_bytes < decision.native_context_trigger_bytes
        && (decision.native_context_max_request_bytes == 0
            || current_request_bytes <= decision.native_context_max_request_bytes)
    {
        return Ok(());
    }
    let current_size = conversation.range_bytes(0, messages.len())?.len();
    let available = groups(&messages)?
        .into_iter()
        .map(|mut group| {
            group.bytes = conversation.range_bytes(group.start, group.end)?;
            group.hash = crate::store::hash(&group.bytes);
            Ok(group)
        })
        .collect::<Result<Vec<_>>>()?;
    let candidates = choose_candidates(available, decision.native_context_mode.clone(), seen);
    if candidates.is_empty() {
        return Ok(());
    }
    let all_hashes = candidates
        .iter()
        .map(|group| group.hash.clone())
        .collect::<Vec<_>>();
    let potential = current_size.saturating_sub(
        serde_json::to_vec(&proposed_messages(&messages, &candidates, &all_hashes))?.len(),
    );
    if potential < decision.native_context_min_savings_bytes
        || potential * 100 < current_size * decision.native_context_min_savings_ratio_percent
    {
        return Ok(());
    }
    if crate::budget::status(db, attempt)?["remaining_s"]
        .as_f64()
        .is_some_and(|remaining| remaining * 1000.0 <= decision.deadline_ms as f64 + 1000.0)
    {
        return Ok(());
    }
    let redactions = redactions(
        crate::secrets::values(db, task)?,
        &crate::config::credential(&decision.api_key_env)?,
        provider_key,
    );
    let context_version = crate::delegation::mandatory(db, task)?["version"]
        .as_i64()
        .context("native context version")?;
    let state = json!({
        "catalog":"native-context-v1",
        "context_version":context_version,
        "objective":redacted_short(messages[1]["content"].as_str().unwrap_or(""), &redactions, 1500),
        "groups":candidates.iter().enumerate().map(|(index, group)|json!({
            "id":format!("g{index}"),"hash":group.hash,"bytes":group.bytes.len(),"tool":group.tool,
            "arguments":redacted_short(&group.argument, &redactions, 700),
            "result":redacted_short(&group.result, &redactions, 1100),
        })).collect::<Vec<_>>(),
    });
    let hard_blocker = decision.native_context_max_request_bytes > 0
        && current_request_bytes > decision.native_context_max_request_bytes;
    let mut questions = candidates.iter().enumerate().map(|(index, _)| Question::Choice(ChoiceQuestion {
            id: format!("g{index}"),
            question: "Can this completed read-only result be omitted from the next native request without losing information needed for the assigned task? Choose keep if uncertain.".into(),
            options: vec!["keep".into(), "omit".into()],
        })).collect::<Vec<_>>();
    let mut actions = vec!["continue".into(), "prune".into()];
    if hard_blocker {
        actions.push("hold".into());
    }
    questions.push(Question::Choice(ChoiceQuestion {
        id: "next_action".into(),
        question: "Choose a bounded native action: continue unchanged, prune only high-confidence eligible read-only groups, or hold only if the configured byte ceiling blocks continuation. This choice cannot dispatch tools or delegates.".into(),
        options: actions,
    }));
    let request = DecisionRequest {
        model: decision.model.clone(),
        state: state.clone(),
        questions,
    };
    let id = crate::store::id();
    let admitted = store::enqueue_bounded(
        db,
        &store::QueuedDecision {
            id: id.clone(),
            task: task.into(),
            step: Some(step.into()),
            attempt: Some(attempt.into()),
            purpose: "native_context_prune".into(),
            policy: "native-context-v1".into(),
            backend: decision.backend.clone(),
            model: decision.model.clone(),
            baseline: Some("continue".into()),
        },
        decision.max_decisions_per_task,
    )?;
    if !admitted {
        for group in &candidates {
            seen.insert(group.hash.clone(), MAX_EVALUATIONS_PER_GROUP);
        }
        return Ok(());
    }
    let evidence = serde_json::to_vec(&state)?;
    let evidence_hash = crate::store::hash(&evidence);
    let artifact_hash = db.artifact(
        task,
        Some(step),
        &format!("native-context-evidence-{id}"),
        &evidence,
        &json!({"attempt":attempt,"purpose":"native_context_prune"}),
        false,
    )?;
    store::start(
        db,
        &id,
        &store::PreparedDecision {
            state_hash: evidence_hash.clone(),
            context_version,
            policy_hash: crate::store::hash(b"native-context-v1"),
            catalog_hash: crate::store::hash(b"keep,omit,continue,prune,hold"),
            candidate_hashes: json!(
                candidates
                    .iter()
                    .map(|group| &group.hash)
                    .collect::<Vec<_>>()
            ),
            backend_fingerprint: decision.fingerprint()?,
            evidence_hash,
            request_hash: crate::store::hash(&serde_json::to_vec(&crate::decision::wire_request(
                &request,
            ))?),
            cache_hash: String::new(),
            artifact_hash: Some(artifact_hash),
        },
    )?;
    if request.validate().is_err() {
        store::finish_state(db, &id, "skipped", "request_limit", 0)?;
        for group in &candidates {
            seen.insert(group.hash.clone(), MAX_EVALUATIONS_PER_GROUP);
        }
        return Ok(());
    }
    let started = Instant::now();
    let backend = match DecisionHttpClient::new_project(decision.clone(), &db.root, task) {
        Ok(backend) => backend,
        Err(error) => {
            store::finish_state(db, &id, "failed", "backend_unavailable", 0)?;
            for group in &candidates {
                *seen.entry(group.hash.clone()).or_default() += 1;
            }
            return Err(error);
        }
    };
    let response = backend
        .decide_counted_with(&request, |_| {
            let _ = store::attempt_started(db, &id);
        })
        .await;
    let (response, attempts) = match response {
        Ok(value) => value,
        Err((_, attempts)) => {
            store::finish_state(db, &id, "failed", "provider_unavailable", attempts)?;
            for group in &candidates {
                *seen.entry(group.hash.clone()).or_default() += 1;
            }
            return Ok(());
        }
    };
    let action = next_action(&response, hard_blocker);
    let selected = response
        .answers
        .iter()
        .filter_map(|answer| match answer {
            Answer::Choice {
                id,
                answer,
                confidence,
                probabilities,
            } if answer == "omit"
                && *confidence >= 0.95
                && probabilities
                    .get("omit")
                    .is_some_and(|value| *value >= 0.95) =>
            {
                id.strip_prefix('g')
                    .and_then(|index| index.parse::<usize>().ok())
                    .and_then(|index| candidates.get(index))
            }
            _ => None,
        })
        .cloned()
        .collect::<Vec<_>>();
    let hashes = selected
        .iter()
        .map(|group| group.hash.clone())
        .collect::<Vec<_>>();
    let proposal = proposed_messages(&messages, &selected, &hashes);
    let savings = current_size.saturating_sub(serde_json::to_vec(&proposal)?.len());
    let replacement = (|| -> Result<Option<Conversation>> {
        let apply = action == NextAction::Prune
            && decision.native_context_mode == NativeContextMode::Active
            && savings >= decision.native_context_min_savings_bytes
            && savings * 100 >= current_size * decision.native_context_min_savings_ratio_percent
            && !selected.is_empty()
            && authorized(db, task, decision)?
            && crate::delegation::mandatory(db, task)?["version"].as_i64() == Some(context_version);
        if !apply {
            return Ok(None);
        }
        for (index, group) in selected.iter().enumerate() {
            let hash = db.artifact(
                task,
                Some(step),
                &format!("native-context/{attempt}/{id}/{index}"),
                &group.bytes,
                &json!({"attempt":attempt,"decision":id,"group_hash":group.hash}),
                true,
            )?;
            ensure!(hash == group.hash, "native context archival hash mismatch");
            let retrieved = retrieve(db, task, attempt, &hash, 0, PAGE_BYTES)?;
            ensure!(
                retrieved["hash"] == hash,
                "native context archival readback failed"
            );
        }
        Ok(Some(Conversation::new(proposal)?))
    })();
    let replacement = match replacement {
        Ok(replacement) => replacement,
        Err(error) => {
            store::finish_state(db, &id, "failed", "archive_or_policy_failure", attempts)?;
            for group in &candidates {
                *seen.entry(group.hash.clone()).or_default() += 1;
            }
            return Err(error);
        }
    };
    let applied = replacement.is_some();
    let proposed = proposed_action(action, selected.len(), savings, current_size, decision);
    let result = json!({"catalog":"native-context-v1","next_action":action,"answers":response.answers,"selected_hashes":hashes,"savings_bytes":savings,"applied":false,"prepared":applied,"mode":decision.native_context_mode});
    let usage = serde_json::to_value(&response.usage)?;
    let committed = db.atomic(|| {
        if let Some(replacement) = replacement.as_ref() {
            ensure!(authorized(db,task,decision)?, "native context authorization changed before epoch commit");
            ensure!(crate::delegation::mandatory(db,task)?["version"].as_i64()==Some(context_version), "native context version changed before epoch commit");
            let old_hash = crate::store::hash(&conversation.range_bytes(0, messages.len())?);
            let new_hash = crate::store::hash(&replacement.range_bytes(0, replacement.values()?.len())?);
            db.conn.execute(
                "INSERT INTO native_context_epochs(decision,task,attempt,old_hash,new_hash,archive_hashes,state,created) VALUES(?,?,?,?,?,?,'prepared',?)",
                rusqlite::params![id, task, attempt, old_hash, new_hash, json!(hashes).to_string(), crate::store::now()],
            )?;
        }
        store::complete(
            db, &id,
            &store::CompletedDecision {
                result: &result,
                proposed: Some(proposed),
                abstention: proposed != "prune",
                provider_ms: started.elapsed().as_millis() as u64,
                attempts, usage: &usage,
            },
        )
    });
    if let Err(error) = committed {
        store::finish_state(
            db,
            &id,
            "failed",
            "freshness_or_epoch_commit_failure",
            attempts,
        )?;
        return Err(error);
    }
    for group in &candidates {
        seen.insert(group.hash.clone(), MAX_EVALUATIONS_PER_GROUP);
    }
    if let Some(replacement) = replacement {
        ensure!(
            authorized(db, task, decision)?,
            "native context authorization changed before epoch swap"
        );
        ensure!(
            crate::delegation::mandatory(db, task)?["version"].as_i64() == Some(context_version),
            "native context version changed before epoch swap"
        );
        *conversation = replacement;
    }
    let _ = db.event(task, "native_context.decision", json!({"decision":id,"attempt":attempt,"selected":selected.len(),"savings_bytes":savings,"prepared":applied}));
    Ok(())
}

/// A prepared epoch becomes applied only after its request has been sent.
/// Interrupted attempts with a prepared-only epoch never claim use.
pub fn mark_used(db: &Store, task: &str, attempt: &str, conversation_hash: &str) -> Result<()> {
    db.atomic(|| {
        let rows=db.rows("SELECT e.decision,d.result FROM native_context_epochs e JOIN decisions d ON d.id=e.decision WHERE e.task=? AND e.attempt=? AND e.new_hash=? AND e.state='prepared'", &[&task,&attempt,&conversation_hash])?;
        for row in rows {
            let id=row["decision"].as_str().context("epoch decision")?;
            let mut result:Value=serde_json::from_str(row["result"].as_str().context("decision result")?)?;
            result["applied"]=json!(true);
            ensure!(db.conn.execute("UPDATE decisions SET result=? WHERE id=? AND state='succeeded'",rusqlite::params![result.to_string(),id])?==1,"native context decision changed before use");
            ensure!(db.conn.execute("UPDATE native_context_epochs SET state='used' WHERE decision=? AND state='prepared'",[id])?==1,"native context epoch changed before use");
        }
        Ok(())
    })
}

pub fn retrieve(
    db: &Store,
    task: &str,
    attempt: &str,
    hash: &str,
    offset: usize,
    limit: usize,
) -> Result<Value> {
    ensure!(
        hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid native context hash"
    );
    ensure!(
        (1..=PAGE_BYTES).contains(&limit),
        "native context page limit exceeds 8 KiB"
    );
    let prefix = format!("native-context/{attempt}/%");
    ensure!(!db.rows("SELECT hash FROM artifact_links WHERE task=? AND hash=? AND name LIKE ? AND verified=1", &[&task,&hash,&prefix])?.is_empty(), "native context archive is not linked to this task and attempt");
    let bytes = std::fs::read(db.root.join("artifacts").join(hash))?;
    ensure!(
        bytes.len() <= MAX_ARCHIVE_BYTES && crate::store::hash(&bytes) == hash,
        "native context archive integrity failure"
    );
    let text = String::from_utf8(bytes).context("native context archive is not UTF-8")?;
    ensure!(
        offset <= text.len() && text.is_char_boundary(offset),
        "invalid native context offset"
    );
    let mut end = offset.saturating_add(limit).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    ensure!(
        end > offset || offset == text.len(),
        "native context page limit cuts a UTF-8 character"
    );
    Ok(
        json!({"hash":hash,"offset":offset,"content":&text[offset..end],"next_offset":(end<text.len()).then_some(end),"total_bytes":text.len()}),
    )
}

pub fn retrieval_tool() -> Value {
    json!({"type":"function","function":{"name":"retrieve_native_context","description":"Retrieve a bounded page of a previously archived read-only native tool exchange by SHA-256, without rerunning tools.","parameters":{"type":"object","properties":{"hash":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":8192}},"required":["hash","offset","limit"]}}})
}

#[cfg(test)]
#[path = "native_context/tests.rs"]
mod tests;
