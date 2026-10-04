//! Additive native archive identity; OCI checkpoint serialization remains unchanged.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Platform {
    pub os: String,
    pub architecture: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub commit: String,
    pub tree: String,
    pub recipe_digest: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeArtifact {
    pub kind: String,
    pub digest: String,
    pub bytes: u64,
    pub platform: Platform,
    pub entrypoint: Vec<String>,
    pub build_id: String,
    pub source: Source,
}
pub fn hex_identity(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn executable(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.chars().any(char::is_control)
        && !std::path::Path::new(path).is_absolute()
        && std::path::Path::new(path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}
impl Platform {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            ["linux", "darwin"].contains(&self.os.as_str())
                && ["arm64", "x86_64"].contains(&self.architecture.as_str()),
            "invalid native platform"
        );
        Ok(())
    }
}
impl NativeArtifact {
    pub fn parse(value: &Value) -> Result<Self> {
        let artifact: Self = serde_json::from_value(value.clone())?;
        artifact.validate()?;
        Ok(artifact)
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            self.kind == "native-archive"
                && self
                    .digest
                    .strip_prefix("sha256:")
                    .is_some_and(|h| hex_identity(h, 64)),
            "invalid native archive digest/kind"
        );
        ensure!(
            self.bytes > 0 && self.bytes <= 256 * 1024 * 1024,
            "native archive exceeds size bound"
        );
        self.platform.validate()?;
        ensure!(
            !self.entrypoint.is_empty()
                && self.entrypoint.len() <= 32
                && executable(&self.entrypoint[0])
                && self
                    .entrypoint
                    .iter()
                    .all(|a| a.len() <= 4096 && !a.contains('\0')),
            "invalid native entrypoint"
        );
        ensure!(
            !self.build_id.trim().is_empty()
                && self.build_id.len() <= 256
                && !self.build_id.chars().any(char::is_control),
            "invalid native build ID"
        );
        ensure!(
            hex_identity(&self.source.commit, 40)
                && hex_identity(&self.source.tree, 40)
                && hex_identity(&self.source.recipe_digest, 64),
            "invalid native source identity"
        );
        Ok(())
    }
}
