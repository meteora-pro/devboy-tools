use async_trait::async_trait;
use devboy_core::{
    Error, GetChatsParams, GetMessagesParams, MessengerChat, MessengerMessage, MessengerProvider,
    ProviderResult, Result, SearchMessagesParams, SendMessageParams,
};
use reqwest::header::HeaderMap;
use serde::Deserialize;
use tracing::debug;

use crate::DEFAULT_SLACK_API_URL;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackAuthInfo {
    pub user_id: String,
    pub user_name: Option<String>,
    pub team_id: String,
    pub team_name: String,
    pub bot_id: Option<String>,
    pub url: Option<String>,
    pub scopes: Vec<String>,
    pub missing_scopes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SlackClient {
    token: String,
    base_url: String,
    http: reqwest::Client,
    required_scopes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SlackAuthTestResponse {
    ok: bool,
    error: Option<String>,
    url: Option<String>,
    team: Option<String>,
    user: Option<String>,
    #[serde(rename = "team_id")]
    team_id: Option<String>,
    #[serde(rename = "user_id")]
    user_id: Option<String>,
    #[serde(rename = "bot_id")]
    bot_id: Option<String>,
}

impl SlackClient {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            base_url: DEFAULT_SLACK_API_URL.to_string(),
            http: reqwest::Client::new(),
            required_scopes: devboy_core::default_slack_required_scopes(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    pub fn with_required_scopes(mut self, required_scopes: Vec<String>) -> Self {
        self.required_scopes = required_scopes;
        self
    }

    pub fn required_scopes(&self) -> &[String] {
        &self.required_scopes
    }

    pub async fn auth_info(&self) -> Result<SlackAuthInfo> {
        let url = format!("{}/auth.test", self.base_url);
        debug!(url, "slack auth.test request");

        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        let status = response.status();
        let headers = response.headers().clone();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(Error::from_status(status.as_u16(), text));
        }

        let payload: SlackAuthTestResponse = response
            .json()
            .await
            .map_err(|e| Error::InvalidData(e.to_string()))?;

        if !payload.ok {
            let message = payload
                .error
                .unwrap_or_else(|| "unknown_slack_error".to_string());
            return Err(match message.as_str() {
                "invalid_auth" | "not_authed" => Error::Unauthorized(message),
                "missing_scope" => Error::Forbidden(message),
                _ => Error::Api {
                    status: 200,
                    message,
                },
            });
        }

        let scopes = parse_scopes(&headers);
        let missing_scopes = self
            .required_scopes
            .iter()
            .filter(|scope| !scopes.iter().any(|actual| actual == *scope))
            .cloned()
            .collect();

        Ok(SlackAuthInfo {
            user_id: payload.user_id.unwrap_or_default(),
            user_name: payload.user,
            team_id: payload.team_id.unwrap_or_default(),
            team_name: payload.team.unwrap_or_default(),
            bot_id: payload.bot_id,
            url: payload.url,
            scopes,
            missing_scopes,
        })
    }

    pub async fn ensure_healthy(&self) -> Result<SlackAuthInfo> {
        let info = self.auth_info().await?;
        if info.missing_scopes.is_empty() {
            Ok(info)
        } else {
            Err(Error::Forbidden(format!(
                "Slack token is missing required scopes: {}",
                info.missing_scopes.join(", ")
            )))
        }
    }
}

#[async_trait]
impl MessengerProvider for SlackClient {
    fn provider_name(&self) -> &'static str {
        "slack"
    }

    async fn get_chats(&self, _params: GetChatsParams) -> Result<ProviderResult<MessengerChat>> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "get_chats".to_string(),
        })
    }

    async fn get_messages(
        &self,
        _params: GetMessagesParams,
    ) -> Result<ProviderResult<MessengerMessage>> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "get_messages".to_string(),
        })
    }

    async fn search_messages(
        &self,
        _params: SearchMessagesParams,
    ) -> Result<ProviderResult<MessengerMessage>> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "search_messages".to_string(),
        })
    }

    async fn send_message(&self, params: SendMessageParams) -> Result<MessengerMessage> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: format!("send_message to {}", params.chat_id),
        })
    }
}

fn parse_scopes(headers: &HeaderMap) -> Vec<String> {
    headers
        .get("x-oauth-scopes")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|scope| !scope.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::POST;
    use httpmock::MockServer;

    #[tokio::test]
    async fn auth_info_reads_identity_and_scopes() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/auth.test");
            then.status(200)
                .header(
                    "x-oauth-scopes",
                    "channels:read, channels:history, chat:write, users:read",
                )
                .json_body(serde_json::json!({
                    "ok": true,
                    "url": "https://example.slack.com/",
                    "team": "Example",
                    "user": "devboy",
                    "team_id": "T123",
                    "user_id": "U123",
                    "bot_id": "B123"
                }));
        });

        let info = SlackClient::new("xoxb-test")
            .with_base_url(server.base_url())
            .auth_info()
            .await
            .unwrap();

        assert_eq!(info.team_name, "Example");
        assert_eq!(info.user_id, "U123");
        assert!(info.missing_scopes.is_empty());
    }

    #[tokio::test]
    async fn ensure_healthy_fails_when_scopes_missing() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/auth.test");
            then.status(200)
                .header("x-oauth-scopes", "channels:read")
                .json_body(serde_json::json!({
                    "ok": true,
                    "team": "Example",
                    "team_id": "T123",
                    "user_id": "U123"
                }));
        });

        let error = SlackClient::new("xoxb-test")
            .with_base_url(server.base_url())
            .ensure_healthy()
            .await
            .unwrap_err();

        assert!(error.to_string().contains("missing required scopes"));
    }

    #[tokio::test]
    async fn messenger_methods_are_scaffolded_as_unsupported() {
        let client = SlackClient::new("xoxb-test");

        let err = client
            .get_chats(GetChatsParams::default())
            .await
            .unwrap_err();
        assert!(matches!(err, Error::ProviderUnsupported { .. }));

        let err = client
            .send_message(SendMessageParams {
                chat_id: "C123".to_string(),
                text: "hello".to_string(),
                thread_id: None,
                reply_to_id: None,
                attachments: vec![devboy_core::MessageAttachment::default()],
            })
            .await
            .unwrap_err();
        assert!(matches!(err, Error::ProviderUnsupported { .. }));
    }
}
