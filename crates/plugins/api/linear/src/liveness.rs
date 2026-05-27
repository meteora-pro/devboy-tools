//! Linear liveness probe.

use async_trait::async_trait;
use devboy_core::{LivenessProbe, LivenessResult, liveness::LivenessStatus};
use secrecy::SecretString;

use crate::client::LinearClient;

#[async_trait]
impl LivenessProbe for LinearClient {
    async fn test(&self, token: &SecretString) -> devboy_core::Result<LivenessResult> {
        match self.viewer_with_token(token).await {
            Ok(viewer) => Ok(LivenessResult::live(format!(
                "{} ({})",
                viewer.name,
                viewer.email.unwrap_or_else(|| "no email".to_string())
            ))),
            Err(devboy_core::Error::Unauthorized(_)) => {
                Ok(LivenessResult::revoked("Linear API rejected the token"))
            }
            Err(devboy_core::Error::RateLimited { retry_after }) => Ok(LivenessResult::throttled(
                format!("retry_after={}", retry_after.unwrap_or(0)),
            )),
            Err(err) => Ok(LivenessResult {
                status: LivenessStatus::Error,
                detail: Some(err.to_string()),
                expires_at: None,
            }),
        }
    }

    fn provider_name(&self) -> &str {
        "linear"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::POST;
    use httpmock::MockServer;

    #[tokio::test]
    async fn probe_returns_live_for_valid_token() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/graphql");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"data":{"viewer":{"id":"u1","name":"Alice","displayName":"alice","email":"alice@example.com"}}}"#);
        });

        let client = LinearClient::with_base_url(
            format!("{}/graphql", server.base_url()),
            "team-1",
            SecretString::from("lin_api_live".to_owned()),
        );

        let result = client
            .test(&SecretString::from("lin_api_live".to_owned()))
            .await
            .unwrap();
        assert_eq!(result.status, LivenessStatus::Live);
        mock.assert();
    }
}
