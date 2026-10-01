//! Direct Sign in with ChatGPT. Tokens stay in protected managed-account storage.
use crate::{
    accounts,
    store::{Store, now},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
mod lifecycle;
mod protocol;
mod session;
mod storage;
pub use lifecycle::{access_token, export, export_file, import, sign_out};
use lifecycle::{from_response, summary};
use protocol::*;
pub use session::dispatch;
use storage::*;
pub use storage::{Registration, prepare_remote, validate_credential};
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registration_rejects_dynamic_client_and_missing_scope() {
        let mut record = sample();
        record.client_id = "dynamic_agent_client".into();
        assert!(record.validate().is_err());
        record.client_id = "oaiapp_test".into();
        record.scopes = vec!["openid".into()];
        assert!(record.validate().is_ok());
        assert!(!record.plan_enabled());
    }
    #[test]
    fn returning_callback_cannot_change_client() {
        assert!(callback_client(Some("oaiapp_saved"), Some("oaiapp_other")).is_err());
        assert_eq!(
            callback_client(Some("oaiapp_saved"), None).unwrap(),
            "oaiapp_saved"
        );
        assert!(callback_client(None, Some("dynamic_agent_client")).is_err());
        assert!(callback_client(None, None).is_err());
    }
    #[test]
    fn rejects_unsigned_identity_and_wrong_nonce() {
        assert!(
            validate_jwt(
                "e30.e30.",
                &serde_json::json!({"keys": []}),
                "oaiapp_test",
                Some("nonce"),
                None
            )
            .is_err()
        );
    }
    pub(super) fn sample() -> Registration {
        Registration {
            issuer: ISSUER.into(),
            subject: "subject".into(),
            email: None,
            client_id: "oaiapp_test".into(),
            ext_agent_host_id: "urn:uuid:de31b716-84d7-4852-a6da-3bb1206d6baa".into(),
            access_token: "access".into(),
            refresh_token: "test-refresh-token-secret".into(),
            id_token: "id".into(),
            token_type: "Bearer".into(),
            scopes: vec!["chatgpt.tokens.use.direct".into(), "resource.invoke".into()],
            expires_at: crate::store::now() + 3600,
            earliest_refresh_at: None,
        }
    }
}

#[cfg(test)]
mod integration_tests;
