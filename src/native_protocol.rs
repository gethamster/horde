//! Frozen request history for native OpenAI-compatible providers.
use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, value::RawValue};

/// Messages are encoded once on append. There is deliberately no update API.
#[derive(Serialize)]
#[serde(transparent)]
pub(crate) struct Conversation(Vec<Box<RawValue>>);
impl Conversation {
    pub fn new(messages: Vec<Value>) -> Result<Self> {
        let mut conversation = Self(Vec::new());
        for message in messages {
            conversation.push(message)?;
        }
        Ok(conversation)
    }
    pub fn push(&mut self, message: Value) -> Result<()> {
        self.0.push(serde_json::value::to_raw_value(&message)?);
        Ok(())
    }
}

pub(crate) fn request_bytes(
    model: &str,
    messages: &Conversation,
    tools: &RawValue,
    stream: bool,
    max_tokens: u64,
    max_price: Option<&str>,
    extra_body: &std::collections::BTreeMap<String, Value>,
) -> Result<Vec<u8>> {
    validate_extra_body(extra_body)?;
    let extra_body = extra_body_with_usage(extra_body, stream);
    #[derive(Serialize)]
    struct Request<'a> {
        model: &'a str,
        tools: &'a RawValue,
        messages: &'a Conversation,
        stream: bool,
        max_tokens: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_price: Option<&'a str>,
        #[serde(flatten)]
        extra_body: &'a std::collections::BTreeMap<String, Value>,
    }
    Ok(serde_json::to_vec(&Request {
        model,
        tools,
        messages,
        stream,
        max_tokens,
        max_price,
        extra_body: &extra_body,
    })?)
}

pub(crate) fn extra_body_with_usage(
    extra_body: &std::collections::BTreeMap<String, Value>,
    stream: bool,
) -> std::collections::BTreeMap<String, Value> {
    let mut extra_body = extra_body.clone();
    if stream {
        let options = extra_body
            .entry("stream_options".into())
            .or_insert_with(|| serde_json::json!({}));
        if let Some(options) = options.as_object_mut() {
            options.entry("include_usage").or_insert(Value::Bool(true));
        }
    }
    extra_body
}

pub(crate) fn validate_extra_body(
    fields: &std::collections::BTreeMap<String, Value>,
) -> Result<()> {
    for key in fields.keys() {
        if [
            "model",
            "messages",
            "tools",
            "stream",
            "max_tokens",
            "max_price",
            "n",
        ]
        .contains(&key.as_str())
        {
            anyhow::bail!(
                "extra_body.{key} is reserved; use the dedicated setting where available"
            );
        }
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct RepeatedToolCall {
    pub tool: String,
    pub repetitions: usize,
}
impl std::fmt::Display for RepeatedToolCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "tool {} repeated {} times with identical arguments and unchanged results; task held for inspection",
            self.tool, self.repetitions
        )
    }
}
impl std::error::Error for RepeatedToolCall {}

#[derive(Default)]
pub(crate) struct LoopGuard {
    signature: String,
    outcome: String,
    repetitions: usize,
}
impl LoopGuard {
    pub fn signature(&self, name: &str, parsed: Option<&Value>, raw: &Value) -> String {
        crate::store::hash(
            serde_json::to_string(&(name, parsed.unwrap_or(raw)))
                .expect("JSON serializes")
                .as_bytes(),
        )
    }
    pub fn count(&self) -> usize {
        self.repetitions
    }
    pub fn should_stop(&self, signature: &str, limit: usize) -> bool {
        limit > 0 && self.repetitions >= limit && self.signature == signature
    }
    pub fn record(&mut self, signature: String, outcome: &Value, limit: usize) -> bool {
        let outcome = crate::store::hash(outcome.to_string().as_bytes());
        if self.signature == signature && self.outcome == outcome {
            self.repetitions += 1;
        } else {
            self.repetitions = 1;
        }
        self.signature = signature;
        self.outcome = outcome;
        limit > 0 && self.repetitions == limit
    }
}

#[cfg(test)]
mod loop_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn repeated_calls_stop_only_after_unchanged_results_and_can_be_disabled() {
        let mut guard = LoopGuard::default();
        let signature = guard.signature(
            "put_artifact",
            Some(&json!({"name":"done","content":"ok"})),
            &Value::Null,
        );
        for _ in 0..3 {
            guard.record(signature.clone(), &json!({"hash":"abc"}), 3);
        }
        assert!(guard.should_stop(&signature, 3));
        assert!(!guard.should_stop(&signature, 0));
        guard.record(signature.clone(), &json!({"hash":"changed"}), 3);
        assert!(!guard.should_stop(&signature, 3));
        assert_eq!(guard.count(), 1);
        assert!(!guard.should_stop("other", 3));
    }

    #[test]
    fn a_loop_failure_blocks_the_task_without_releasing_claims_or_accepting_work() {
        use crate::{config::Settings, store::Store, template};
        let dir = tempfile::tempdir().unwrap();
        let db = Store::open(dir.path()).unwrap();
        let plan = template::compile(
            "simulated",
            &template::load_templates(std::path::Path::new("absent")).unwrap(),
            std::collections::BTreeMap::from([("task".into(), "loop".into())]),
        )
        .unwrap();
        let task = db
            .submit("loop", dir.path(), &Settings::default(), &plan)
            .unwrap();
        let step = db.steps(&task).unwrap()[0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let worker = db.register(&task, Some(&step)).unwrap();
        let worker = worker["id"].as_str().unwrap();
        db.conn
            .execute(
                "UPDATE workers SET workspace=? WHERE id=?",
                rusqlite::params![dir.path().to_str(), worker],
            )
            .unwrap();
        db.claim(&task, worker, &["file".into()]).unwrap();
        db.conn.execute("INSERT INTO attempts(id,step,worker,state,started) VALUES('attempt',?,?,'running',0)",rusqlite::params![step,worker]).unwrap();
        db.finish(
            &step,
            "attempt",
            worker,
            Err(RepeatedToolCall {
                tool: "put_artifact".into(),
                repetitions: 3,
            }
            .into()),
        )
        .unwrap();
        assert_eq!(db.task(&task).unwrap()["status"], "blocked");
        assert_eq!(db.steps(&task).unwrap()[0]["state"], "failed");
        assert_eq!(
            db.rows("SELECT * FROM claims WHERE worker=?", &[&worker])
                .unwrap()
                .len(),
            1
        );
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    #[test]
    fn streamed_requests_default_to_usage_and_preserve_explicit_provider_options() {
        let history = Conversation::new(vec![]).unwrap();
        let tools = RawValue::from_string("[]".into()).unwrap();
        for (stream, extra, expected) in [
            (
                true,
                serde_json::json!({}),
                serde_json::json!({"include_usage":true}),
            ),
            (false, serde_json::json!({}), Value::Null),
            (
                true,
                serde_json::json!({"stream_options":{"include_obfuscation":false}}),
                serde_json::json!({"include_usage":true,"include_obfuscation":false}),
            ),
            (
                true,
                serde_json::json!({"stream_options":{"include_usage":false}}),
                serde_json::json!({"include_usage":false}),
            ),
            (
                true,
                serde_json::json!({"stream_options":null}),
                Value::Null,
            ),
        ] {
            let fields: std::collections::BTreeMap<String, Value> =
                serde_json::from_value(extra).unwrap();
            let original = fields.clone();
            let body =
                request_bytes("model", &history, &tools, stream, 128, None, &fields).unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["stream_options"], expected);
            assert_eq!(fields, original);
            if !stream {
                assert!(body.get("stream_options").is_none());
            }
        }
    }

    #[test]
    fn appended_messages_preserve_previous_wire_bytes_and_tool_order() {
        let arguments = "{ \"z\": 2, \"a\": [1, 3] }";
        let mut history = Conversation::new(vec![
            json!({"role":"system","content":"固定\ncontract"}),
            json!({"role":"assistant","content":"line\nwith \\\"quotes\\\"", "tool_calls":[{"id":"one","function":{"name":"call","arguments":arguments}}]}),
        ]).unwrap();
        let tools = RawValue::from_string("[ {\"name\":\"z\"}, {\"name\":\"a\"} ]".into()).unwrap();
        #[derive(Deserialize)]
        struct Wire {
            messages: Vec<Box<RawValue>>,
            tools: Box<RawValue>,
        }
        let first: Wire = serde_json::from_slice(
            &request_bytes(
                "model",
                &history,
                &tools,
                false,
                128,
                None,
                &Default::default(),
            )
            .unwrap(),
        )
        .unwrap();
        history
            .push(json!({"role":"tool","tool_call_id":"one","content":"done"}))
            .unwrap();
        history
            .push(json!({"role":"user","content":"New budget notice at the tail"}))
            .unwrap();
        let second: Wire = serde_json::from_slice(
            &request_bytes(
                "model",
                &history,
                &tools,
                false,
                128,
                None,
                &Default::default(),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(first.tools.get(), tools.get());
        assert_eq!(first.tools.get(), second.tools.get());
        for (old, new) in first.messages.iter().zip(&second.messages) {
            assert_eq!(old.get().as_bytes(), new.get().as_bytes());
        }
        assert_eq!(second.messages.len(), first.messages.len() + 2);
        let assistant: Value = serde_json::from_str(second.messages[1].get()).unwrap();
        assert_eq!(
            assistant["tool_calls"][0]["function"]["arguments"],
            arguments
        );
    }
}

/// Decode SSE incrementally. No caller receives an executable call until DONE.
#[derive(Default)]
struct StreamResponse {
    pending: Vec<u8>,
    event: Vec<String>,
    event_bytes: usize,
    total: usize,
    done: bool,
    first: bool,
    message: serde_json::Map<String, Value>,
    calls: std::collections::BTreeMap<u64, Value>,
    metadata: serde_json::Map<String, Value>,
    finish: Value,
}
impl StreamResponse {
    fn feed(&mut self, bytes: &[u8], progress: &mut impl FnMut(Value) -> Result<()>) -> Result<()> {
        self.total += bytes.len();
        anyhow::ensure!(
            self.total <= 16 * 1024 * 1024,
            "native response exceeds 16 MiB"
        );
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = self.pending.drain(..=end).collect();
            let line = std::str::from_utf8(&line)?.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                self.flush(progress)?;
            } else if let Some(data) = line.strip_prefix("data:") {
                self.event_bytes += data.len();
                anyhow::ensure!(
                    self.event_bytes <= 1024 * 1024,
                    "native SSE event exceeds 1 MiB"
                );
                self.event
                    .push(data.strip_prefix(' ').unwrap_or(data).to_owned());
            }
        }
        anyhow::ensure!(
            self.pending.len() <= 1024 * 1024,
            "native SSE line exceeds 1 MiB"
        );
        Ok(())
    }
    fn flush(&mut self, progress: &mut impl FnMut(Value) -> Result<()>) -> Result<()> {
        if self.event.is_empty() {
            return Ok(());
        }
        let data = std::mem::take(&mut self.event).join("\n");
        self.event_bytes = 0;
        anyhow::ensure!(!self.done, "native stream sent data after DONE");
        if data.trim() == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let value: Value = serde_json::from_str(&data)?;
        anyhow::ensure!(
            value.get("error").is_none(),
            "native stream returned an error"
        );
        let object = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("native SSE object required"))?;
        for (key, value) in object {
            if key != "choices" && !value.is_null() {
                self.metadata.insert(key.clone(), value.clone());
            }
        }
        if let Some(choices) = value["choices"].as_array() {
            for choice in choices {
                anyhow::ensure!(
                    choice["index"].as_u64().unwrap_or(0) == 0 && choices.len() == 1,
                    "native stream requires one choice"
                );
                if !choice["finish_reason"].is_null() {
                    self.finish = choice["finish_reason"].clone();
                }
                let delta = &choice["delta"];
                for key in ["content", "reasoning_content", "reasoning"] {
                    if let Some(fragment) = delta[key].as_str() {
                        append_fragment(&mut self.message, key, fragment)?;
                        if !fragment.is_empty() && !self.first {
                            self.first = true;
                            progress(serde_json::json!({"kind":"first_token"}))?;
                        }
                    }
                }
                if let Some(calls) = delta["tool_calls"].as_array() {
                    for call in calls {
                        let index = call["index"].as_u64().unwrap_or(0);
                        anyhow::ensure!(index < 128, "too many streamed tool calls");
                        let assembled = self.calls.entry(index).or_insert_with(|| serde_json::json!({"type":"function","function":{"name":"","arguments":""}}));
                        if let Some(id) = call["id"].as_str() {
                            append_fragment(assembled.as_object_mut().unwrap(), "id", id)?;
                        }
                        let function = assembled["function"].as_object_mut().unwrap();
                        for key in ["name", "arguments"] {
                            if let Some(fragment) = call["function"][key].as_str() {
                                append_fragment(function, key, fragment)?;
                            }
                        }
                        if !self.first {
                            self.first = true;
                            progress(serde_json::json!({"kind":"first_token"}))?;
                        }
                        if call["function"]["name"]
                            .as_str()
                            .is_some_and(|s| !s.is_empty())
                        {
                            progress(
                                serde_json::json!({"kind":"tool_intent","index":index,"tool":function["name"]}),
                            )?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
    fn finish(mut self) -> Result<Value> {
        anyhow::ensure!(
            self.done,
            "native stream ended before DONE; no tools executed"
        );
        self.message
            .insert("role".into(), Value::String("assistant".into()));
        self.message
            .entry("content")
            .or_insert(Value::String(String::new()));
        if !self.calls.is_empty() {
            self.message.insert(
                "tool_calls".into(),
                Value::Array(self.calls.into_values().collect()),
            );
        }
        self.metadata.insert(
            "choices".into(),
            serde_json::json!([{"message":self.message,"finish_reason":self.finish}]),
        );
        Ok(Value::Object(self.metadata))
    }
}
fn append_fragment(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    fragment: &str,
) -> Result<()> {
    let old = object
        .entry(key)
        .or_insert_with(|| Value::String(String::new()));
    let Value::String(text) = old else {
        anyhow::bail!("invalid stream fragment");
    };
    anyhow::ensure!(
        text.len() + fragment.len() <= 4 * 1024 * 1024,
        "native stream field exceeds 4 MiB"
    );
    text.push_str(fragment);
    Ok(())
}

pub(crate) async fn read_response(
    response: reqwest::Response,
    streaming: bool,
    mut progress: impl FnMut(Value) -> Result<()>,
) -> Result<Value> {
    use futures_util::StreamExt;
    let mut chunks = response.bytes_stream();
    let mut parser = StreamResponse::default();
    let mut body = Vec::new();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk?;
        if streaming {
            parser.feed(&chunk, &mut progress)?;
            if parser.done {
                return parser.finish();
            }
        } else {
            body.extend_from_slice(&chunk);
            anyhow::ensure!(
                body.len() <= 16 * 1024 * 1024,
                "native response exceeds 16 MiB"
            );
        }
    }
    if streaming {
        parser.finish()
    } else {
        Ok(serde_json::from_slice(&body)?)
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    use serde_json::json;
    fn event(value: Value) -> String {
        format!("data: {value}\r\n\r\n")
    }
    #[test]
    fn byte_fragmented_stream_assembles_interleaved_calls_and_usage_before_done() {
        let text = [
            event(json!({"choices":[{"index":0,"delta":{"content":"hé界","reasoning_content":"check"}}]})),
            event(json!({"choices":[{"delta":{"tool_calls":[{"index":1,"id":"two","function":{"name":"read_","arguments":"{\"p\":"}},{"index":0,"id":"one","function":{"name":"ping","arguments":"{}"}}]}}]})),
            event(json!({"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"name":"file","arguments":"\"a\"}"}}]},"finish_reason":"tool_calls"}]})),
            event(json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":4,"prefix_tokens_reused":8},"speculation":{"accepted":3}})),
        ].concat();
        let mut parser = StreamResponse::default();
        let mut progress = vec![];
        for byte in text.as_bytes() {
            parser
                .feed(&[*byte], &mut |v| {
                    progress.push(v);
                    Ok(())
                })
                .unwrap();
        }
        assert!(!parser.done);
        assert_eq!(
            progress
                .iter()
                .filter(|v| v["kind"] == "first_token")
                .count(),
            1
        );
        assert!(progress.iter().any(|v| v["tool"] == "read_file"));
        parser.feed(b"data: [DONE]\n\n", &mut |_| Ok(())).unwrap();
        let result = parser.finish().unwrap();
        assert_eq!(result["choices"][0]["message"]["content"], "hé界");
        assert_eq!(
            result["choices"][0]["message"]["reasoning_content"],
            "check"
        );
        let calls = &result["choices"][0]["message"]["tool_calls"];
        assert_eq!(calls[0]["id"], "one");
        assert_eq!(calls[1]["function"]["arguments"], "{\"p\":\"a\"}");
        assert_eq!(result["usage"]["prefix_tokens_reused"], 8);
        assert_eq!(result["speculation"]["accepted"], 3);
    }
    #[test]
    fn interrupted_or_oversized_stream_is_rejected() {
        let mut parser = StreamResponse::default();
        parser.feed(event(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"command","arguments":"{\"command\":"}}]}}]})).as_bytes(), &mut |_| Ok(())).unwrap();
        assert!(
            parser
                .finish()
                .unwrap_err()
                .to_string()
                .contains("before DONE")
        );
        let mut parser = StreamResponse::default();
        assert!(
            parser
                .feed(&vec![b'x'; 1024 * 1024 + 1], &mut |_| Ok(()))
                .is_err()
        );
    }
}

/// A completion action is local to the native conversation. Store::finish remains
/// responsible for workflow validation and acceptance after the executor returns.
pub(crate) fn completion_tool(step: &crate::template::Step) -> Value {
    let mut tool = serde_json::json!({"type":"function","function":{
        "name":"complete_step",
        "description":"Finish this assigned step and return control to the runtime. Call alone, after required work and checks. For planning, return the completed plan; do not wait for proposed implementation steps. Use accepted=false to report a blocker. This does not certify artifacts or bypass runtime acceptance checks.",
        "parameters":{"type":"object","properties":{
            "result":{"type":"string","description":"Completed work or the reason this step is blocked."},
            "accepted":{"type":"boolean"},
            "artifacts":{"type":"array","items":{"type":"string"}}
        },"required":["result","accepted","artifacts"],"additionalProperties":false}
    }});
    for (name, kind) in &step.output_types {
        tool["function"]["parameters"]["properties"]
            .as_object_mut()
            .unwrap()
            .entry(name.clone())
            .or_insert_with(|| serde_json::json!({"type":kind}));
    }
    tool
}

pub(crate) fn completion_result(
    args: Value,
    calls: usize,
    step: &crate::template::Step,
) -> Result<Value> {
    anyhow::ensure!(
        calls == 1,
        "complete_step must be the only tool call in its turn; finish other tool calls first"
    );
    let object = args
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("complete_step requires an object"))?;
    anyhow::ensure!(
        object.keys().all(
            |k| ["result", "accepted", "artifacts"].contains(&k.as_str())
                || step.output_types.contains_key(k)
        ),
        "complete_step accepts only result, accepted, artifacts, and declared named outputs"
    );
    anyhow::ensure!(
        args["result"].is_string() && args["accepted"].is_boolean(),
        "complete_step requires a string result and boolean accepted"
    );
    anyhow::ensure!(
        args["artifacts"]
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string)),
        "complete_step artifacts must be an array of paths"
    );
    if args["accepted"] == true {
        crate::template::validate_result(step, &args)?;
    }
    Ok(args)
}

#[cfg(test)]
mod completion_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn completion_preserves_declared_outputs_and_rejects_runtime_fields() {
        let step: crate::template::Step =
            serde_json::from_value(json!({"id":"work","output_types":{"count":"integer"}}))
                .unwrap();
        let tool = completion_tool(&step);
        assert_eq!(
            tool["function"]["parameters"]["properties"]["count"]["type"],
            "integer"
        );
        let valid = json!({"result":"done","accepted":true,"artifacts":[],"count":3});
        assert_eq!(completion_result(valid.clone(), 1, &step).unwrap(), valid);
        let mut invalid = valid.clone();
        invalid["count"] = json!("3");
        assert!(
            completion_result(invalid, 1, &step)
                .unwrap_err()
                .to_string()
                .contains("count must be integer")
        );
        for field in ["task", "worker", "verified", "usage", "_token"] {
            let mut invalid = valid.clone();
            invalid[field] = json!(true);
            assert!(completion_result(invalid, 1, &step).is_err(), "{field}");
        }
        assert!(
            completion_result(
                json!({"result":"blocked","accepted":false,"artifacts":[]}),
                1,
                &step
            )
            .is_ok()
        );
    }
}
