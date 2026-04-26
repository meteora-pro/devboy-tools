use std::fmt;

use devboy_core::{Error, Result};
use reqwest::RequestBuilder;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::DEFAULT_CONFLUENCE_API_PATH;

#[derive(Clone, PartialEq, Eq)]
pub enum ConfluenceAuth {
    BearerToken(String),
    Basic { username: String, password: String },
}

impl fmt::Debug for ConfluenceAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BearerToken(_) => f.debug_tuple("BearerToken").field(&"<redacted>").finish(),
            Self::Basic { username, .. } => f
                .debug_struct("Basic")
                .field("username", username)
                .field("password", &"<redacted>")
                .finish(),
        }
    }
}

#[derive(Clone)]
pub struct ConfluenceClient {
    base_url: String,
    auth: ConfluenceAuth,
    http: reqwest::Client,
}

impl fmt::Debug for ConfluenceClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfluenceClient")
            .field("base_url", &self.base_url)
            .field("auth", &self.auth)
            .field("http", &self.http)
            .finish()
    }
}

impl ConfluenceClient {
    pub fn new(base_url: impl Into<String>, auth: ConfluenceAuth) -> Self {
        Self {
            base_url: normalize_base_url(base_url.into()),
            auth,
            http: reqwest::Client::new(),
        }
    }

    pub fn with_http_client(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn auth(&self) -> &ConfluenceAuth {
        &self.auth
    }

    pub fn rest_api_url(&self, path: &str) -> String {
        let path = path.trim_start_matches('/');
        format!("{}{DEFAULT_CONFLUENCE_API_PATH}/{}", self.base_url, path)
    }

    pub async fn get_json<T>(&self, path: &str) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let request = self
            .http
            .get(self.rest_api_url(path))
            .header(reqwest::header::ACCEPT, "application/json");
        self.send_json(request).await
    }

    pub async fn post_json<T, B>(&self, path: &str, body: &B) -> Result<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let request = self
            .http
            .post(self.rest_api_url(path))
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body);
        self.send_json(request).await
    }

    pub async fn put_json<T, B>(&self, path: &str, body: &B) -> Result<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let request = self
            .http
            .put(self.rest_api_url(path))
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body);
        self.send_json(request).await
    }

    async fn send_json<T>(&self, request: RequestBuilder) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let response = self
            .apply_auth(request)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(Error::from_status(status.as_u16(), message));
        }

        response
            .json()
            .await
            .map_err(|e| Error::InvalidData(e.to_string()))
    }

    fn apply_auth(&self, request: RequestBuilder) -> RequestBuilder {
        match &self.auth {
            ConfluenceAuth::BearerToken(token) => request.bearer_auth(token),
            ConfluenceAuth::Basic { username, password } => {
                request.basic_auth(username, Some(password))
            }
        }
    }
}

fn normalize_base_url(base_url: String) -> String {
    base_url.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use httpmock::Method::{GET, POST};
    use httpmock::MockServer;
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Debug, Deserialize)]
    struct EchoResponse {
        ok: bool,
    }

    #[derive(Debug, Serialize)]
    struct CreatePayload {
        title: String,
    }

    #[tokio::test]
    async fn rest_api_url_normalizes_base_url() {
        let client = ConfluenceClient::new(
            "https://wiki.example.com/",
            ConfluenceAuth::BearerToken("token".into()),
        );

        assert_eq!(
            client.rest_api_url("content"),
            "https://wiki.example.com/rest/api/content"
        );
    }

    #[tokio::test]
    async fn get_json_uses_bearer_auth() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content")
                .header("authorization", "Bearer secret-token");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"ok":true}"#);
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        );
        let response: EchoResponse = client.get_json("content").await.unwrap();

        mock.assert();
        assert!(response.ok);
    }

    #[tokio::test]
    async fn post_json_uses_basic_auth() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/rest/api/content")
                .header(
                    "authorization",
                    "Basic dXNlckBleGFtcGxlLmNvbTpwYXNzd29yZA==",
                )
                .json_body_obj(&serde_json::json!({ "title": "ADR-001" }));
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"ok":true}"#);
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::Basic {
                username: "user@example.com".into(),
                password: "password".into(),
            },
        );
        let response: EchoResponse = client
            .post_json(
                "content",
                &CreatePayload {
                    title: "ADR-001".into(),
                },
            )
            .await
            .unwrap();

        mock.assert();
        assert!(response.ok);
    }
}
