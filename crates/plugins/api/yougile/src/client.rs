//! YouGile API client scaffold.

use secrecy::SecretString;

use crate::DEFAULT_YOUGILE_URL;

/// Minimal YouGile client used by the workspace wiring layer.
///
/// Provider methods are added in follow-up steps once the config and scope
/// decisions are finalized.
#[derive(Clone)]
pub struct YouGileClient {
    base_url: String,
    board_id: String,
    token: SecretString,
    client: reqwest::Client,
}

impl YouGileClient {
    /// Create a new YouGile client with the default API base URL.
    pub fn new(board_id: impl Into<String>, token: SecretString) -> Self {
        Self::with_base_url(DEFAULT_YOUGILE_URL, board_id, token)
    }

    /// Create a new YouGile client with a custom base URL.
    pub fn with_base_url(
        base_url: impl Into<String>,
        board_id: impl Into<String>,
        token: SecretString,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            board_id: board_id.into(),
            token,
            client: reqwest::Client::builder()
                .user_agent("devboy-tools")
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Effective YouGile API base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Default board scope attached to this client.
    pub fn board_id(&self) -> &str {
        &self.board_id
    }

    /// Shared request client.
    pub fn http_client(&self) -> &reqwest::Client {
        &self.client
    }

    /// API token accessor for internal follow-up implementation work.
    pub fn token(&self) -> &SecretString {
        &self.token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_default_api_url() {
        let client = YouGileClient::new("board-1", SecretString::from("token".to_owned()));
        assert_eq!(client.base_url(), DEFAULT_YOUGILE_URL);
        assert_eq!(client.board_id(), "board-1");
    }

    #[test]
    fn with_base_url_trims_trailing_slash() {
        let client = YouGileClient::with_base_url(
            "https://example.invalid/api-v2/",
            "board-2",
            SecretString::from("token".to_owned()),
        );
        assert_eq!(client.base_url(), "https://example.invalid/api-v2");
        assert_eq!(client.board_id(), "board-2");
    }
}
