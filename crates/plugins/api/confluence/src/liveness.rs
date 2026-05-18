//! Confluence [`LivenessProbe`] stub per [ADR-021] §6.
//!
//! Confluence Server / Data Center exposes
//! `GET /rest/api/user/current` (Cloud uses
//! `/wiki/rest/api/user/current`); routing the response through
//! the typed liveness shape requires distinguishing Server vs
//! Cloud auth modes. The stub uses the trait's default `test`
//! impl (returns [`LivenessStatus::NotImplemented`]) so `doctor`
//! shows a clear "not implemented" line until a real probe lands.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md
//! [`LivenessStatus::NotImplemented`]: devboy_core::liveness::LivenessStatus

use async_trait::async_trait;
use devboy_core::LivenessProbe;

use crate::client::ConfluenceClient;

#[async_trait]
impl LivenessProbe for ConfluenceClient {
    fn provider_name(&self) -> &str {
        "confluence"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::ConfluenceAuth;
    use devboy_core::liveness::LivenessStatus;
    use secrecy::SecretString;

    #[tokio::test]
    async fn provider_name_is_confluence_and_default_returns_not_implemented() {
        let client = ConfluenceClient::new(
            "https://example.atlassian.net/wiki",
            ConfluenceAuth::BearerToken(SecretString::from("any".to_owned())),
        );
        assert_eq!(client.provider_name(), "confluence");
        let r = client
            .test(&SecretString::from("any".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::NotImplemented);
        assert!(r.detail.unwrap().contains("confluence"));
    }
}
