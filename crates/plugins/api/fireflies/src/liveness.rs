//! Fireflies [`LivenessProbe`] stub per [ADR-021] §6.
//!
//! The Fireflies GraphQL API has a `me` query that returns the
//! authenticated user; routing the response through the typed
//! liveness shape (active / revoked / expired) is a follow-up.
//! The stub uses the trait's default `test` impl (returns
//! [`LivenessStatus::NotImplemented`]) so `doctor` shows a clear
//! "not implemented" line until the real probe lands.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md
//! [`LivenessStatus::NotImplemented`]: devboy_core::liveness::LivenessStatus

use async_trait::async_trait;
use devboy_core::LivenessProbe;

use crate::client::FirefliesClient;

#[async_trait]
impl LivenessProbe for FirefliesClient {
    fn provider_name(&self) -> &str {
        "fireflies"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::liveness::LivenessStatus;
    use secrecy::SecretString;

    #[tokio::test]
    async fn provider_name_is_fireflies_and_default_returns_not_implemented() {
        let client = FirefliesClient::new(SecretString::from("any".to_owned()));
        assert_eq!(client.provider_name(), "fireflies");
        let r = client
            .test(&SecretString::from("any".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::NotImplemented);
    }
}
