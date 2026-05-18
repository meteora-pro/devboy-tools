//! ClickUp [`LivenessProbe`] stub per [ADR-021] §6.
//!
//! ClickUp's introspection endpoint is `GET /api/v2/user`, but
//! routing the response through the typed liveness shape (active /
//! revoked / expired) requires more sniffing than this commit's
//! scope allows. The stub uses the trait's default `test` impl
//! (which returns [`LivenessStatus::NotImplemented`]) and wires
//! the provider name so `doctor` shows
//! "clickup liveness probe is not implemented; format-only
//! validation only" instead of "no probe configured".
//!
//! Replacing the stub: drop a real `test` override against
//! `<base>/api/v2/user`, mirroring `crates/plugins/api/github`.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md
//! [`LivenessStatus::NotImplemented`]: devboy_core::liveness::LivenessStatus

use async_trait::async_trait;
use devboy_core::LivenessProbe;

use crate::client::ClickUpClient;

#[async_trait]
impl LivenessProbe for ClickUpClient {
    fn provider_name(&self) -> &str {
        "clickup"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::liveness::LivenessStatus;
    use secrecy::SecretString;

    #[tokio::test]
    async fn provider_name_is_clickup_and_default_returns_not_implemented() {
        let client = ClickUpClient::new("list-id", SecretString::from("any".to_owned()));
        assert_eq!(client.provider_name(), "clickup");
        let r = client
            .test(&SecretString::from("any".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::NotImplemented);
        assert!(r.detail.unwrap().contains("clickup"));
    }
}
