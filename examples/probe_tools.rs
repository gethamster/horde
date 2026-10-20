//! Opt-in wire qualification without opening a Horde Store or resolving accounts.
use anyhow::{Context, Result, ensure};
use horde::{config::ExecutorConfig, executor::probe_tools_with_key};
use serde_json::Value;
use std::{io::Read, path::Path};

fn bounded_file(path: &Path) -> Result<String> {
    let mut text = String::new();
    std::fs::File::open(path)?
        .take(16 * 1024 + 1)
        .read_to_string(&mut text)?;
    ensure!(text.len() <= 16 * 1024, "probe input exceeds 16 KiB");
    Ok(text)
}

async fn probe(config_path: &Path, key_path: &Path) -> Result<Value> {
    let document: toml::Value = toml::from_str(&bounded_file(config_path)?)?;
    ensure!(
        document
            .get("model")
            .and_then(toml::Value::as_str)
            .is_some_and(|model| !model.trim().is_empty() && model != "auto"),
        "qualification requires an explicit model"
    );
    let config: ExecutorConfig = document.try_into()?;
    ensure!(
        config.kind == "tuara" && !config.stream,
        "this example requires kind=tuara and stream=false"
    );
    let key = bounded_file(key_path)?;
    ensure!(!key.trim().is_empty(), "empty probe credential");
    probe_tools_with_key(&config, key.trim()).await
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(
        args.len() == 2,
        "usage: probe_tools CONFIG.toml PRIVATE_KEY_FILE"
    );
    // Only qualification flags reach stdout. Do not print credentials, configuration,
    // raw responses or opaque model metadata, including when qualification fails.
    let result = probe(Path::new(&args[0]), Path::new(&args[1]))
        .await
        .map_err(|_| anyhow::anyhow!("provider tool qualification failed"))?;
    println!("{}", serde_json::to_string(&result).context("probe flags")?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        routing::{get, post},
    };
    use serde_json::json;

    #[tokio::test]
    async fn qualification_requires_an_explicit_model_before_network_or_credentials() {
        for model in ["", "model='auto'", "model=''", "model='  '"] {
            let directory = tempfile::tempdir().unwrap();
            let config = directory.path().join("provider.toml");
            std::fs::write(
                &config,
                format!("kind='tuara'\nstream=false\nbase_url='http://127.0.0.1:1'\n{model}\n"),
            )
            .unwrap();
            let error = probe(&config, &directory.path().join("absent-key"))
                .await
                .unwrap_err();
            assert!(error.to_string().contains("explicit model"), "{error:#}");
        }
    }

    #[tokio::test]
    async fn wire_probe_uses_only_explicit_files_without_creating_runtime_state() {
        let app = Router::new()
            .route("/models", get(|| async { Json(json!({"data":[{"id":"exact-model"}]})) }))
            .route("/chat/completions", post(|Json(body): Json<Value>| async move {
                let message = if body["messages"].as_array().unwrap().len() == 1 {
                    json!({"role":"assistant","content":"","tool_calls":[{"id":"ping1","function":{"name":"ping","arguments":"{\"value\":\"ok\"}"}}]})
                } else {
                    json!({"role":"assistant","content":"OK"})
                };
                Json(json!({"model":"exact-model","choices":[{"message":message}]}))
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("provider.toml");
        let key = directory.path().join("key");
        std::fs::write(
            &config,
            format!(
                "kind='tuara'\nstream=false\nmodel='exact-model'\nbase_url='http://{address}'\n"
            ),
        )
        .unwrap();
        std::fs::write(&key, "synthetic-fixture-key\n").unwrap();
        let result = probe(&config, &key).await.unwrap();
        assert_eq!(result["tool_round_trip_verified"], true);
        assert_eq!(result["streaming_tool_calls_verified"], false);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
        server.abort();
    }
}
