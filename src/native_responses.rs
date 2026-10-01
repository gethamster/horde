//! Stateless Responses transport for ChatGPT plan usage.
use crate::native_protocol::Conversation;
use anyhow::{Result, ensure};
use serde_json::{Value, json, value::RawValue};
use std::collections::BTreeMap;

pub(crate) struct ResponseError {
    pub http_status: Option<u16>,
    pub code: Option<String>,
    pub param: Option<String>,
    pub request_id: Option<String>,
    /// Kept for diagnosis; never included in Display or progress events.
    pub body: Value,
}
impl ResponseError {
    pub fn usage_limited(&self) -> bool {
        self.code.as_deref() == Some("subscription_sharing_usage_limit_exceeded")
    }
    pub fn retryable(&self) -> bool {
        !self.usage_limited()
            && (matches!(self.http_status, Some(502..=504))
                || matches!(
                    self.code.as_deref(),
                    Some(
                        "subscription_sharing_usage_unavailable"
                            | "subscription_sharing_user_unavailable"
                    )
                ))
    }
}
impl std::fmt::Display for ResponseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ChatGPT inference failed (HTTP {:?}, code {:?}, parameter {:?}, request ID {:?})",
            self.http_status, self.code, self.param, self.request_id
        )
    }
}
impl std::fmt::Debug for ResponseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}
impl std::error::Error for ResponseError {}
fn failure(body: Value, status: Option<u16>, request_id: Option<String>) -> ResponseError {
    let error = body.get("error").unwrap_or(&body);
    let safe = |key: &str| {
        error[key]
            .as_str()
            .filter(|s| {
                s.len() <= 200
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_-.[]".contains(c))
            })
            .map(str::to_owned)
    };
    ResponseError {
        http_status: status,
        code: safe("code"),
        param: safe("param"),
        request_id,
        body,
    }
}

pub(crate) fn request_bytes(
    model: &str,
    conversation: &Conversation,
    tools: &RawValue,
    extra: &BTreeMap<String, Value>,
) -> Result<Vec<u8>> {
    let forbidden = [
        "model",
        "input",
        "tools",
        "stream",
        "store",
        "background",
        "conversation",
        "max_output_tokens",
        "max_tool_calls",
        "metadata",
        "moderation",
        "multi_agent",
        "prompt",
        "prompt_cache_retention",
        "safety_identifier",
        "temperature",
        "top_logprobs",
        "top_p",
        "truncation",
        "user",
        "previous_response_id",
        "max_tokens",
        "messages",
        "stream_options",
        "max_price",
        "n",
        "include",
    ];
    for key in extra.keys() {
        ensure!(
            !forbidden.contains(&key.as_str()),
            "extra_body.{key} is reserved or unsupported for ChatGPT Responses"
        );
    }
    let mut input = Vec::new();
    for message in conversation.values()? {
        if let Some(output) = message["responses_output"].as_array() {
            input.extend(output.iter().cloned());
            continue;
        }
        match message["role"].as_str() {
            Some("system") => input.push(json!({"role":"developer","content":message["content"]})),
            Some("tool") => input.push(json!({"type":"function_call_output","call_id":message["tool_call_id"],"output":message["content"]})),
            Some("assistant") => {
                if let Some(text) = message["content"].as_str().filter(|s| !s.is_empty()) { input.push(json!({"role":"assistant","content":text})); }
                if let Some(calls) = message["tool_calls"].as_array() { for call in calls { input.push(json!({"type":"function_call","call_id":call["id"],"name":call["function"]["name"],"arguments":call["function"]["arguments"],"namespace":"horde"})); } }
            }
            _ => input.push(message),
        }
    }
    let tools: Vec<Value> = serde_json::from_str(tools.get())?;
    let functions: Vec<Value> = tools
        .into_iter()
        .map(|tool| -> Result<Value> {
            ensure!(
                tool["type"] == "function",
                "ChatGPT supports Horde function tools only"
            );
            let mut function = tool.get("function").cloned().unwrap_or(tool);
            ensure!(
                function.is_object() && function["name"].is_string(),
                "malformed Horde tool definition"
            );
            function["type"] = json!("function");
            Ok(function)
        })
        .collect::<Result<_>>()?;
    let mut body = json!({"model":model,"input":input,"tools":[{"type":"namespace","name":"horde","description":"Horde local worker tools","tools":functions}],"store":false,"stream":true,"include":["reasoning.encrypted_content"]});
    for (key, value) in extra {
        body[key] = value.clone();
    }
    Ok(serde_json::to_vec(&body)?)
}

#[derive(Default)]
struct Stream {
    pending: Vec<u8>,
    data: Vec<String>,
    total: usize,
    completed: Option<Value>,
    first: bool,
}
impl Stream {
    fn feed(&mut self, bytes: &[u8], progress: &mut impl FnMut(Value) -> Result<()>) -> Result<()> {
        self.total += bytes.len();
        ensure!(
            self.total <= 16 * 1024 * 1024,
            "Responses stream exceeds 16 MiB"
        );
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = self.pending.drain(..=end).collect();
            let line = std::str::from_utf8(&line)?.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                self.flush(progress)?;
            } else if let Some(data) = line.strip_prefix("data:") {
                self.data.push(data.trim_start_matches(' ').to_owned());
            }
        }
        ensure!(
            self.pending.len() <= 1024 * 1024,
            "Responses SSE line exceeds 1 MiB"
        );
        Ok(())
    }
    fn flush(&mut self, progress: &mut impl FnMut(Value) -> Result<()>) -> Result<()> {
        if self.data.is_empty() {
            return Ok(());
        }
        let data = std::mem::take(&mut self.data).join("\n");
        ensure!(
            data.len() <= 4 * 1024 * 1024,
            "Responses SSE event exceeds 4 MiB"
        );
        if data == "[DONE]" {
            return Ok(());
        }
        ensure!(
            self.completed.is_none(),
            "Responses sent events after completion"
        );
        let event: Value = serde_json::from_str(&data)?;
        match event["type"].as_str() {
            Some("response.completed") => {
                ensure!(
                    event["response"]["status"] == "completed",
                    "Responses completed event has invalid status"
                );
                self.completed = Some(event["response"].clone());
            }
            Some("response.failed" | "response.incomplete") => {
                return Err(failure(event["response"].clone(), None, None).into());
            }
            Some("error") => return Err(failure(event, None, None).into()),
            Some("response.output_text.delta" | "response.function_call_arguments.delta")
                if !self.first =>
            {
                self.first = true;
                progress(json!({"kind":"first_token"}))?;
            }
            Some("response.output_item.added") if event["item"]["type"] == "function_call" => {
                if event["item"]["namespace"]
                    .as_str()
                    .is_some_and(|n| n != "horde")
                {
                    anyhow::bail!("Responses requested an unknown tool namespace");
                }
                progress(
                    json!({"kind":"tool_intent","index":event["output_index"],"tool":event["item"]["name"]}),
                )?;
            }
            _ => {}
        }
        Ok(())
    }
    fn finish(self) -> Result<Value> {
        let response = self.completed.ok_or_else(|| {
            anyhow::anyhow!("Responses stream ended before response.completed; no tools executed")
        })?;
        let output = response["output"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Responses completed output array required"))?;
        let mut text = String::new();
        let mut calls = Vec::new();
        let mut call_ids = std::collections::BTreeSet::new();
        for item in output {
            match item["type"].as_str() {
                Some("message") => {
                    if let Some(content) = item["content"].as_array() {
                        for part in content {
                            if part["type"] == "output_text" {
                                text.push_str(part["text"].as_str().unwrap_or(""));
                            }
                        }
                    }
                }
                Some("function_call") => {
                    ensure!(
                        item["namespace"].as_str().is_none_or(|n| n == "horde"),
                        "Responses requested an unknown tool namespace"
                    );
                    ensure!(
                        item["call_id"].is_string()
                            && item["name"].is_string()
                            && item["arguments"].is_string(),
                        "Responses returned malformed function call"
                    );
                    ensure!(calls.len() < 128, "too many Responses function calls");
                    ensure!(
                        call_ids.insert(item["call_id"].as_str().unwrap()),
                        "duplicate Responses call ID"
                    );
                    ensure!(
                        item["status"].as_str().is_none_or(|s| s == "completed"),
                        "incomplete Responses function call"
                    );
                    calls.push(json!({"id":item["call_id"],"type":"function","function":{"name":item["name"],"arguments":item["arguments"]}}));
                }
                Some("reasoning") => {}
                _ => anyhow::bail!("Responses returned unsupported output item"),
            }
        }
        Ok(
            json!({"id":response["id"],"usage":{"prompt_tokens":response["usage"]["input_tokens"],"completion_tokens":response["usage"]["output_tokens"],"total_tokens":response["usage"]["total_tokens"],"prompt_tokens_details":response["usage"]["input_tokens_details"]},"choices":[{"message":{"role":"assistant","content":text,"tool_calls":calls,"responses_output":output},"finish_reason":if calls.is_empty() {"stop"} else {"tool_calls"}}]}),
        )
    }
}

pub(crate) async fn read_response(
    response: reqwest::Response,
    mut progress: impl FnMut(Value) -> Result<()>,
) -> Result<Value> {
    use futures_util::StreamExt;
    let status = response.status();
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| {
            s.len() <= 200
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
        })
        .map(str::to_owned);
    let mut chunks = response.bytes_stream();
    let mut parser = Stream::default();
    let mut body = Vec::new();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk?;
        if !status.is_success() {
            body.extend_from_slice(&chunk);
            ensure!(
                body.len() <= 1024 * 1024,
                "Responses error body exceeds 1 MiB"
            );
        } else if let Err(error) = parser.feed(&chunk, &mut progress) {
            if let Some(failure) = error.downcast_ref::<ResponseError>() {
                return Err(ResponseError {
                    http_status: Some(status.as_u16()),
                    code: failure.code.clone(),
                    param: failure.param.clone(),
                    request_id,
                    body: failure.body.clone(),
                }
                .into());
            }
            return Err(error);
        } else if parser.completed.is_some() {
            return parser.finish();
        }
    }
    if !status.is_success() {
        return Err(failure(
            serde_json::from_slice(&body)
                .unwrap_or_else(|_| json!({"detail":String::from_utf8_lossy(&body)})),
            Some(status.as_u16()),
            request_id,
        )
        .into());
    }
    parser.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(value: Value) -> String {
        format!("data: {value}\r\n\r\n")
    }
    fn completed(output: Value) -> Value {
        json!({"type":"response.completed","response":{"id":"resp_one","status":"completed","output":output,"usage":{"input_tokens":12,"output_tokens":3,"total_tokens":15}}})
    }
    #[test]
    fn fragmented_stream_preserves_reasoning_multiple_calls_and_history() {
        let output = json!([
            {"id":"rs_one","type":"reasoning","encrypted_content":"opaque","summary":[]},
            {"id":"fc_one","type":"function_call","call_id":"call_one","namespace":"horde","name":"read_file","arguments":"{\"path\":\"hé界\"}"},
            {"id":"fc_two","type":"function_call","call_id":"call_two","namespace":"horde","name":"ping","arguments":"{}"}]);
        let events = [
            event(json!({"type":"response.output_item.added","output_index":1,"item":output[1]})),
            event(json!({"type":"response.function_call_arguments.delta","delta":"hé界"})),
            event(completed(output.clone())),
        ]
        .concat();
        let mut parser = Stream::default();
        let mut progress = vec![];
        for byte in events.as_bytes() {
            parser
                .feed(&[*byte], &mut |v| {
                    progress.push(v);
                    Ok(())
                })
                .unwrap();
        }
        let result = parser.finish().unwrap();
        assert_eq!(result["usage"]["prompt_tokens"], 12);
        assert_eq!(
            result["choices"][0]["message"]["tool_calls"][1]["id"],
            "call_two"
        );
        assert_eq!(
            progress
                .iter()
                .filter(|v| v["kind"] == "first_token")
                .count(),
            1
        );
        let history = Conversation::new(vec![
            json!({"role":"system","content":"rules"}),
            result["choices"][0]["message"].clone(),
            json!({"role":"tool","tool_call_id":"call_one","content":"found"}),
            json!({"role":"tool","tool_call_id":"call_two","content":"ok"}),
        ])
        .unwrap();
        let tools = RawValue::from_string(
            json!([{"type":"function","function":{"name":"ping","parameters":{"type":"object"}}}])
                .to_string(),
        )
        .unwrap();
        let request: Value = serde_json::from_slice(
            &request_bytes("exact-slug", &history, &tools, &BTreeMap::new()).unwrap(),
        )
        .unwrap();
        assert_eq!(request["input"][0]["role"], "developer");
        assert_eq!(request["input"][1], output[0]);
        assert_eq!(request["input"][2], output[1]);
        assert_eq!(
            request["input"][4],
            json!({"type":"function_call_output","call_id":"call_one","output":"found"})
        );
        assert_eq!(request["store"], false);
        assert_eq!(request["stream"], true);
        assert_eq!(request["tools"][0]["type"], "namespace");
        assert_eq!(request["tools"][0]["tools"][0]["name"], "ping");
    }
    #[test]
    fn text_and_usage_only_accepted_after_completed() {
        let mut parser = Stream::default();
        parser
            .feed(
                event(json!({"type":"response.output_text.delta","delta":"hello"})).as_bytes(),
                &mut |_| Ok(()),
            )
            .unwrap();
        assert!(
            parser
                .finish()
                .unwrap_err()
                .to_string()
                .contains("before response.completed")
        );
        let mut parser = Stream::default();
        parser.feed(event(completed(json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}]))).as_bytes(),&mut |_| Ok(())).unwrap();
        let result = parser.finish().unwrap();
        assert_eq!(result["choices"][0]["message"]["content"], "hello");
        assert_eq!(result["choices"][0]["finish_reason"], "stop");
    }
    #[test]
    fn failures_have_safe_codes_and_do_not_accept_partial_tools() {
        for kind in ["response.failed", "response.incomplete"] {
            let mut parser = Stream::default();
            let error = parser.feed(event(json!({"type":kind,"response":{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"private_token"}}})).as_bytes(),&mut |_| Ok(())).unwrap_err();
            let failure = error.downcast_ref::<ResponseError>().unwrap();
            assert!(failure.usage_limited());
            assert!(!failure.retryable());
            assert!(!error.to_string().contains("private_token"));
        }
        let error = failure(
            json!({"detail":"admission failure"}),
            Some(503),
            Some("req_one".into()),
        );
        assert!(error.retryable());
        assert_eq!(error.body["detail"], "admission failure");
        let mut parser = Stream::default();
        assert!(
            parser
                .feed(&vec![b'x'; 1024 * 1024 + 1], &mut |_| Ok(()))
                .is_err()
        );
    }
    #[test]
    fn reserved_and_unsupported_options_are_rejected() {
        let history = Conversation::new(vec![]).unwrap();
        let tools = RawValue::from_string("[]".into()).unwrap();
        for key in [
            "store",
            "stream",
            "max_output_tokens",
            "previous_response_id",
            "temperature",
            "tools",
            "stream_options",
            "max_price",
            "n",
        ] {
            let extra = BTreeMap::from([(key.into(), json!(true))]);
            assert!(
                request_bytes("model", &history, &tools, &extra).is_err(),
                "{key}"
            );
        }
    }
    #[test]
    fn unknown_namespace_and_malformed_calls_are_rejected() {
        for item in [
            json!({"type":"function_call","namespace":"foreign","name":"ping","call_id":"one","arguments":"{}"}),
            json!({"type":"function_call","name":"ping","arguments":{}}),
        ] {
            let mut parser = Stream::default();
            parser
                .feed(event(completed(json!([item]))).as_bytes(), &mut |_| Ok(()))
                .unwrap();
            assert!(parser.finish().is_err());
        }
    }
    async fn mock_response(status: &str, body: &str) -> reqwest::Response {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let reply = format!(
            "HTTP/1.1 {status}\r\nx-request-id: req_test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            for chunk in reply.as_bytes().chunks(3) {
                socket.write_all(chunk).await.unwrap();
            }
        });
        reqwest::Client::new()
            .get(format!("http://{address}"))
            .send()
            .await
            .unwrap()
    }
    #[tokio::test]
    async fn http_admission_and_midstream_errors_preserve_metadata_safely() {
        let response = mock_response("403 Forbidden", r#"{"detail":"secret diagnostic"}"#).await;
        let error = read_response(response, |_| Ok(())).await.unwrap_err();
        let error = error.downcast_ref::<ResponseError>().unwrap();
        assert_eq!(error.http_status, Some(403));
        assert_eq!(error.request_id.as_deref(), Some("req_test"));
        assert_eq!(error.body["detail"], "secret diagnostic");
        assert!(!format!("{error:?}").contains("secret diagnostic"));
        let body = event(
            json!({"type":"response.failed","response":{"error":{"code":"subscription_sharing_usage_unavailable","param":"model","message":"secret"}}}),
        );
        let response = mock_response("200 OK", &body).await;
        let error = read_response(response, |_| Ok(())).await.unwrap_err();
        let error = error.downcast_ref::<ResponseError>().unwrap();
        assert_eq!(error.http_status, Some(200));
        assert_eq!(error.request_id.as_deref(), Some("req_test"));
        assert!(error.retryable());
    }
    #[tokio::test]
    async fn http_stream_returns_completed_response_and_rejects_disconnect() {
        let body = event(completed(
            json!([{"type":"message","content":[{"type":"output_text","text":"done"}]}]),
        ));
        let response = mock_response("200 OK", &body).await;
        assert_eq!(
            read_response(response, |_| Ok(())).await.unwrap()["choices"][0]["message"]["content"],
            "done"
        );
        let response = mock_response(
            "200 OK",
            &event(json!({"type":"response.output_text.delta","delta":"partial"})),
        )
        .await;
        assert!(
            read_response(response, |_| Ok(()))
                .await
                .unwrap_err()
                .to_string()
                .contains("before response.completed")
        );
    }
}
