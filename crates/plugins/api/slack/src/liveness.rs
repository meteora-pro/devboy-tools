//! Slack [`LivenessProbe`] stub per [ADR-021] §6.
//!
//! Slack's `auth.test` API method validates the bot token and
//! returns workspace + bot user info. Routing the response into
//! the typed liveness shape (active / revoked / expired) is a
//! follow-up. The stub uses the trait's default `test` impl
//! (returns [`LivenessStatus::NotImplemented`]) until the real
//! probe lands.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md
//! [`LivenessStatus::NotImplemented`]: devboy_core::liveness::LivenessStatus

use async_trait::async_trait;
use devboy_core::LivenessProbe;

use crate::client::SlackClient;

#[async_trait]
impl LivenessProbe for SlackClient {
    fn provider_name(&self) -> &str {
        "slack"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::liveness::LivenessStatus;
    use secrecy::SecretString;

    #[tokio::test]
    async fn provider_name_is_slack_and_default_returns_not_implemented() {
        let client = SlackClient::new(SecretString::from("any".to_owned()));
        assert_eq!(client.provider_name(), "slack");
        let r = client
            .test(&SecretString::from("any".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::NotImplemented);
    }
}
