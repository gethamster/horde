//! Bounded Chat Completions diagnostics using the configured transport mode.
use crate::config::ExecutorConfig;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

pub(crate) fn validate(config: &ExecutorConfig) -> Result<()> {
    crate::native_protocol::validate_extra_body(&config.extra_body)?;
    for field in ["max_completion_tokens", "max_output_tokens"] {
        ensure!(
            !config.extra_body.contains_key(field),
            "probe reserves extra_body.{field}; the probe limits output to 128 tokens"
        );
    }
    Ok(())
}

pub(crate) async fn tools(config: &ExecutorConfig, key: &str, model: &str) -> Result<Value> {
    validate(config)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut messages = vec![
        json!({"role":"user","content":"Call ping with value ok. After the tool response, reply OK."}),
    ];
    let mut extensions = false;
    let mut model_verified = true;
    for round in 0..2 {
        let response = client
            .post(format!(
                "{}/chat/completions",
                config.base_url.trim_end_matches('/')
            ))
            .bearer_auth(key)
            .json(&request(config, model, &messages, round))
            .send()
            .await?;
        ensure!(
            response.status().is_success(),
            "provider tool probe returned {}",
            response.status()
        );
        let mut bytes = 0usize;
        let data = crate::native_protocol::read_response_bounded(
            response,
            config.stream,
            |_| Ok(()),
            |chunk| {
                bytes = bytes
                    .checked_add(chunk)
                    .context("probe response size overflow")?;
                ensure!(
                    bytes <= 64 * 1024,
                    "provider tool probe response exceeds 64 KiB"
                );
                Ok(())
            },
        )
        .await?;
        if let Some(returned) = data.get("model") {
            ensure!(
                returned == model,
                "provider tool probe returned another model"
            );
        }
        model_verified &= data["model"] == model;
        let assistant = data["choices"][0]["message"].clone();
        ensure!(
            assistant["role"] == "assistant",
            "provider probe returned no assistant message"
        );
        if round == 0 {
            let (id, observed) = tool_identity(&assistant)?;
            extensions = observed;
            // Preserve the complete assistant turn, including opaque vendor
            // metadata needed to accept the following tool-result turn.
            messages.push(assistant);
            messages.push(json!({"role":"tool","tool_call_id":id,"content":"ok"}));
        } else {
            ensure!(
                assistant.get("tool_calls").is_none_or(
                    |calls| calls.is_null() || calls.as_array().is_some_and(Vec::is_empty)
                ) && assistant["content"]
                    .as_str()
                    .is_some_and(|text| !text.trim().is_empty()),
                "provider probe returned no completion after the tool response"
            );
        }
    }
    Ok(
        json!({"model":model,"requested_model":model,"catalog_verified":true,"model_verified":model_verified,
        "tool_calls_verified":true,"tool_round_trip_verified":true,"streaming_tool_calls_verified":config.stream,
        "tool_extensions_preserved":extensions}),
    )
}

fn request(config: &ExecutorConfig, model: &str, messages: &[Value], round: usize) -> Value {
    let mut body = json!({"model":model,"messages":messages,"stream":config.stream,"max_tokens":128,
        "tools":[{"type":"function","function":{"name":"ping","parameters":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]}}}]});
    body.as_object_mut().expect("request object").extend(
        crate::native_protocol::extra_body_with_usage(&config.extra_body, config.stream),
    );
    body["tool_choice"] = if round == 0 {
        json!({"type":"function","function":{"name":"ping"}})
    } else {
        json!("none")
    };
    if let Some(price) = &config.max_price {
        body["max_price"] = json!(price);
    }
    body
}

fn tool_identity(assistant: &Value) -> Result<(String, bool)> {
    let calls = assistant["tool_calls"]
        .as_array()
        .context("probe returned no tool calls")?;
    ensure!(
        calls.len() == 1
            && calls[0]["function"]["name"] == "ping"
            && serde_json::from_str::<Value>(
                calls[0]["function"]["arguments"]
                    .as_str()
                    .context("probe arguments")?
            )? == json!({"value":"ok"}),
        "model did not satisfy tool-call contract"
    );
    let id = calls[0]["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .context("probe tool identity")?
        .to_owned();
    Ok((
        id,
        assistant.get("extra_content").is_some() || calls[0].get("extra_content").is_some(),
    ))
}
