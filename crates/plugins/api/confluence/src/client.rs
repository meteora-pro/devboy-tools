use std::collections::HashMap;
use std::fmt;

use async_trait::async_trait;
use devboy_core::{
    CreatePageParams, Error, KbPage, KbPageContent, KbSpace, KnowledgeBaseProvider,
    ListPagesParams, ProviderResult, Result, SearchKbParams, UpdatePageParams,
};
use reqwest::RequestBuilder;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::DEFAULT_CONFLUENCE_API_PATH;

#[derive(Clone, PartialEq, Eq)]
pub enum ConfluenceAuth {
    None,
    BearerToken(String),
    Basic { username: String, password: String },
}

impl fmt::Debug for ConfluenceAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("None"),
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
    proxy_headers: Option<HashMap<String, String>>,
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
            proxy_headers: None,
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

    /// Configure proxy mode with headers added to every request.
    /// When proxy is active, provider auth headers are suppressed.
    pub fn with_proxy(mut self, headers: HashMap<String, String>) -> Self {
        self.proxy_headers = Some(headers);
        self
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
        if let Some(headers) = &self.proxy_headers {
            return request.headers(proxy_headers_to_headermap(headers));
        }

        match &self.auth {
            ConfluenceAuth::None => request,
            ConfluenceAuth::BearerToken(token) => request.bearer_auth(token),
            ConfluenceAuth::Basic { username, password } => {
                request.basic_auth(username, Some(password))
            }
        }
    }
}

fn proxy_headers_to_headermap(headers: &HashMap<String, String>) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (key, value) in headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::try_from(key.as_str()),
            HeaderValue::try_from(value.as_str()),
        ) {
            map.insert(name, value);
        }
    }
    map
}

fn normalize_base_url(base_url: String) -> String {
    base_url.trim_end_matches('/').to_string()
}

#[async_trait]
impl KnowledgeBaseProvider for ConfluenceClient {
    fn provider_name(&self) -> &'static str {
        "confluence"
    }

    async fn get_spaces(&self) -> Result<ProviderResult<KbSpace>> {
        Err(Error::ProviderUnsupported {
            provider: "confluence".into(),
            operation: "get_spaces not yet implemented".into(),
        })
    }

    async fn list_pages(&self, _params: ListPagesParams) -> Result<ProviderResult<KbPage>> {
        Err(Error::ProviderUnsupported {
            provider: "confluence".into(),
            operation: "list_pages not yet implemented".into(),
        })
    }

    async fn get_page(&self, _page_id: &str) -> Result<KbPageContent> {
        Err(Error::ProviderUnsupported {
            provider: "confluence".into(),
            operation: "get_page not yet implemented".into(),
        })
    }

    async fn create_page(&self, _params: CreatePageParams) -> Result<KbPage> {
        Err(Error::ProviderUnsupported {
            provider: "confluence".into(),
            operation: "create_page not yet implemented".into(),
        })
    }

    async fn update_page(&self, _params: UpdatePageParams) -> Result<KbPage> {
        Err(Error::ProviderUnsupported {
            provider: "confluence".into(),
            operation: "update_page not yet implemented".into(),
        })
    }

    async fn search(&self, _params: SearchKbParams) -> Result<ProviderResult<KbPage>> {
        Err(Error::ProviderUnsupported {
            provider: "confluence".into(),
            operation: "search not yet implemented".into(),
        })
    }
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

    #[tokio::test]
    async fn proxy_headers_suppress_provider_auth() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content")
                .header("x-proxy-auth", "secret")
                .header_missing("authorization");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"ok":true}"#);
        });

        let mut headers = HashMap::new();
        headers.insert("x-proxy-auth".into(), "secret".into());

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        )
        .with_proxy(headers);
        let response: EchoResponse = client.get_json("content").await.unwrap();

        mock.assert();
        assert!(response.ok);
    }
}
