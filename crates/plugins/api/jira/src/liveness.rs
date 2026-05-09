//! Jira [`LivenessProbe`] stub per [ADR-021] §6.
//!
//! Both Jira Cloud (API v3) and Self-Hosted/DC (API v2) expose
//! `GET /rest/api/{2,3}/myself` for the authenticated user.
//! Routing the response into the typed liveness shape requires
//! handling both flavours and the email+token vs PAT auth modes.
//! The stub uses the trait's default `test` impl (returns
//! [`LivenessStatus::NotImplemented`]) until a real probe lands.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md
//! [`LivenessStatus::NotImplemented`]: devboy_core::liveness::LivenessStatus

use async_trait::async_trait;
use devboy_core::LivenessProbe;

use crate::client::JiraClient;

#[async_trait]
impl LivenessProbe for JiraClient {
    fn provider_name(&self) -> &str {
        "jira"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::liveness::LivenessStatus;
    use secrecy::SecretString;

    #[tokio::test]
    async fn provider_name_is_jira_and_default_returns_not_implemented() {
        let client = JiraClient::new(
            "https://example.atlassian.net",
            "PROJ",
            "user@example.com",
            SecretString::from("any".to_owned()),
        );
        assert_eq!(client.provider_name(), "jira");
        let r = client
            .test(&SecretString::from("any".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::NotImplemented);
    }
}
