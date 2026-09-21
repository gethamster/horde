use super::*;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionMode {
    #[default]
    Disabled,
    Shadow,
}

/// Native conversation pruning is authorized separately from advisory routing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeContextMode {
    #[default]
    Disabled,
    Shadow,
    Active,
}

/// Operator authorization for decision-model-guided browser tests.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserTestMode {
    #[default]
    Disabled,
    Shadow,
    Active,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct CapabilityGuidance {
    pub runtime: String,
    pub capability: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Decision {
    pub mode: DecisionMode,
    /// Opt in to advisory review of durable workflow checkpoints.
    pub review_enabled: bool,
    pub native_context_mode: NativeContextMode,
    pub browser_test_mode: BrowserTestMode,
    /// Optional hard ceiling for serialized native requests, in bytes.
    pub native_context_max_request_bytes: usize,
    pub native_context_trigger_bytes: usize,
    pub native_context_min_savings_bytes: usize,
    pub native_context_min_savings_ratio_percent: usize,
    pub backend: String,
    pub base_url: String,
    pub api_key_env: String,
    pub model: String,
    /// Wire contract spoken by the configured decision service.
    pub protocol: String,
    pub policy: String,
    pub deadline_ms: u64,
    pub max_attempts: usize,
    pub max_decisions_per_task: usize,
    pub capability_guidance: Vec<CapabilityGuidance>,
}

impl Default for Decision {
    fn default() -> Self {
        Self {
            mode: DecisionMode::Disabled,
            review_enabled: false,
            native_context_mode: NativeContextMode::Disabled,
            browser_test_mode: BrowserTestMode::Disabled,
            native_context_max_request_bytes: 0,
            native_context_trigger_bytes: 64 * 1024,
            native_context_min_savings_bytes: 16 * 1024,
            native_context_min_savings_ratio_percent: 15,
            backend: "tuara".into(),
            base_url: String::new(),
            api_key_env: String::new(),
            model: "jev-1.13.0".into(),
            protocol: "systemone-v1".into(),
            policy: "routing-v1".into(),
            deadline_ms: 5_000,
            max_attempts: 2,
            max_decisions_per_task: 64,
            capability_guidance: vec![],
        }
    }
}

impl Decision {
    pub fn validate(&self) -> Result<()> {
        if self.mode == DecisionMode::Disabled {
            if self.native_context_mode != NativeContextMode::Disabled {
                bail!("native context pruning requires decision.mode=shadow");
            }
            if self.browser_test_mode != BrowserTestMode::Disabled {
                bail!("browser testing requires decision.mode=shadow");
            }
            return Ok(());
        }
        if self.backend.is_empty()
            || self.backend.len() > 64
            || !self.backend.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
            })
        {
            bail!("decision.backend must name a provider");
        }
        if self.model.is_empty()
            || self.model.len() > 128
            || !self.model.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/')
            })
        {
            bail!("decision.model must be a bounded model identifier");
        }
        if self.protocol != "systemone-v1" {
            bail!("decision.protocol must be systemone-v1");
        }
        if self.policy != "routing-v1" {
            bail!("decision.policy must be routing-v1");
        }
        if !(100..=30_000).contains(&self.deadline_ms)
            || !(1..=3).contains(&self.max_attempts)
            || !(1..=256).contains(&self.max_decisions_per_task)
        {
            bail!("decision deadline, attempts, or per-task limit is outside the supported range");
        }
        if self.native_context_max_request_bytes != 0
            && !(8 * 1024..=16 * 1024 * 1024).contains(&self.native_context_max_request_bytes)
        {
            bail!("native context request byte ceiling is outside the supported range");
        }
        if !(8 * 1024..=16 * 1024 * 1024).contains(&self.native_context_trigger_bytes) {
            bail!("native context trigger byte threshold is outside the supported range");
        }
        if !(1024..=1024 * 1024).contains(&self.native_context_min_savings_bytes) {
            bail!("native context minimum savings is outside the supported range");
        }
        if !(5..=90).contains(&self.native_context_min_savings_ratio_percent) {
            bail!("native context minimum savings ratio is outside the supported range");
        }
        if self.base_url.is_empty() {
            bail!(
                "decision provider {} needs a configured Jev-compatible base_url before it can be enabled",
                self.backend
            );
        }
        if self.api_key_env.is_empty()
            || self.api_key_env.len() > 128
            || !self
                .api_key_env
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            bail!("decision.api_key_env must name an environment variable");
        }
        crate::decision::typesafe::endpoint(&self.base_url)?;
        if self.capability_guidance.len() > 256 {
            bail!("decision capability guidance accepts at most 256 entries");
        }
        let mut pairs = std::collections::BTreeSet::new();
        for item in &self.capability_guidance {
            if item.runtime.is_empty()
                || item.capability.is_empty()
                || item.description.trim().is_empty()
                || item.runtime.len() > 256
                || item.capability.len() > 256
                || item.description.len() > 2048
                || !pairs.insert((&item.runtime, &item.capability))
            {
                bail!("invalid or duplicate decision capability guidance");
            }
        }
        Ok(())
    }

    /// Stable service identity used for decision evidence and qualifications.
    /// Credential values are never included.
    pub fn fingerprint(&self) -> Result<String> {
        let endpoint = crate::decision::typesafe::endpoint(&self.base_url)?;
        let bytes = serde_json::to_vec(&serde_json::json!({
            "backend": self.backend,
            "endpoint": endpoint.as_str(),
            "api_key_env": self.api_key_env,
            "model": self.model,
            "protocol": self.protocol,
        }))?;
        Ok(crate::store::hash(&bytes))
    }
}
