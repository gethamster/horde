use super::*;
use std::{process::Command, time::Duration};
pub(super) fn publisher_command(p: &Policy) -> Command {
    let mut c = Command::new(&p.publisher);
    c.env_clear().arg("publish");
    // No controller key, provider credential, SSH agent, or arbitrary inherited variables.
    for key in [
        "PATH",
        "HOME",
        "USER",
        "TMPDIR",
        "LANG",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "CARGO_TARGET_DIR",
    ] {
        if let Some(v) = std::env::var_os(key) {
            c.env(key, v);
        }
    }
    if let Some(endpoint) = &p.registry_publish_endpoint {
        c.env("SYSTEM_REGISTRY_PUBLISH_ENDPOINT", endpoint);
    }
    if p.native_recipe.is_none() {
        c.env(
            "DOCKER_HOST",
            p.docker_host
                .as_deref()
                .unwrap_or("tcp://sandbox-docker:2375"),
        );
    }
    if let Some(store) = &p.native_artifact_store {
        c.env("SYSTEM_NATIVE_ARTIFACT_STORE", store);
    }
    c.env("SYSTEM_REGISTRY_ADMISSION_REQUIRED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("CI", "true");
    c
}
pub(super) async fn helper(p: Policy, request: Value, workspace: &Path) -> Result<Value> {
    let mut command = publisher_command(&p);
    command.current_dir(workspace);
    let result = crate::executor::run_process(
        command,
        Some(serde_json::to_string(&request)?),
        p.timeout_seconds,
        None,
    )
    .await?;
    ensure!(
        result["success"] == true,
        "preview publisher failed; inspect publication provenance and admission"
    );
    let text = result["stdout"].as_str().context("publisher stdout")?;
    ensure!(
        text.len() <= 1024 * 1024,
        "preview publisher receipt exceeds bound"
    );
    serde_json::from_str(text).context("invalid preview publication receipt")
}
pub(super) fn verify_receipt(
    p: &Policy,
    scope: &str,
    task: &str,
    head: &str,
    tree: &str,
    r: &Value,
) -> Result<()> {
    ensure!(
        r["schema_version"] == if p.native_recipe.is_some() { 2 } else { 1 }
            && r["scope"] == scope
            && r["project_id"] == p.project_id
            && r["project_slug"] == p.project_slug
            && r["run_id"] == task
            && r["built_commit"] == head
            && r["tree_sha"] == tree
            && r["component"] == json!(p.component)
            && r["recipe_hash"] == p.recipe_hash(),
        "preview receipt source/tree/recipe mismatch"
    );
    if let Some(recipe) = &p.native_recipe {
        let artifact = crate::run::native_artifact::NativeArtifact::parse(&r["artifact"])?;
        ensure!(
            artifact.source.commit == head
                && artifact.source.tree == tree
                && artifact.source.recipe_digest == p.recipe_hash()
                && artifact.digest == r["artifact_digest"]
                && artifact.platform.os == recipe.platform.os
                && artifact.platform.architecture == recipe.platform.architecture
                && artifact.entrypoint == recipe.entrypoint,
            "native receipt identity or recipe mismatch"
        );
        ensure!(
            r["image"].is_null() && r["artifact_protocol_version"] == 1,
            "native receipt cannot masquerade as OCI"
        );
        let store = p
            .native_artifact_store
            .as_ref()
            .context("native artifact store")?;
        let path = store.join(format!(
            "{}.tar",
            artifact
                .digest
                .strip_prefix("sha256:")
                .context("native digest")?
        ));
        let metadata = std::fs::symlink_metadata(&path)?;
        ensure!(
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() == artifact.bytes,
            "native archive bytes or file type mismatch"
        );
        let mut file = std::fs::File::open(path)?;
        let mut hash = sha2::Sha256::new();
        use sha2::Digest;
        use std::io::Read;
        let mut buffer = [0u8; 65536];
        let mut bytes = 0u64;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            bytes += read as u64;
            ensure!(
                bytes <= artifact.bytes,
                "native archive grew during verification"
            );
            hash.update(&buffer[..read]);
        }
        ensure!(
            bytes == artifact.bytes,
            "native archive changed during verification"
        );
        ensure!(
            format!("sha256:{}", hex::encode(hash.finalize())) == artifact.digest,
            "native archive digest mismatch"
        );
        return Ok(());
    }
    ensure!(
        r["artifact"].is_null() && r["artifact_protocol_version"].is_null(),
        "OCI receipt cannot contain native artifact identity"
    );
    let artifact = r["image"].as_str().context("preview image")?;
    ensure!(
        image(artifact)
            && r["artifact_digest"] == artifact.rsplit_once('@').map(|(_, d)| d).unwrap_or("")
            && artifact.starts_with(&format!("registry:5000/{scope}/{}@sha256:", p.project_slug)),
        "preview artifact is not scoped and immutable"
    );
    Ok(())
}
pub(super) async fn admission(
    p: &Policy,
    method: reqwest::Method,
    path: &str,
    body: Option<&Value>,
) -> Result<Value> {
    let metadata = std::fs::symlink_metadata(&p.admission_token_file)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 4096,
        "invalid admission credential file"
    );
    let token = std::fs::read_to_string(&p.admission_token_file)?;
    let token = token.trim();
    ensure!(token.len() >= 32, "invalid admission credential");
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()?;
    let mut r = client
        .request(
            method,
            format!("{}{path}", p.admission_url.trim_end_matches('/')),
        )
        .bearer_auth(token);
    if let Some(body) = body {
        r = r.json(body)
    }
    let mut response = r.send().await.context("registry admission unavailable")?;
    ensure!(
        response.status().is_success(),
        "registry admission request failed"
    );
    ensure!(
        response.content_length().is_none_or(|n| n <= 65536),
        "admission receipt exceeds bound"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= 65536,
            "admission receipt exceeds bound"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}
