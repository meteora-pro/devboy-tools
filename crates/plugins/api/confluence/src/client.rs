use std::collections::HashMap;
use std::fmt;

use async_trait::async_trait;
use devboy_core::{
    CreatePageParams, Error, KbPage, KbPageContent, KbSpace, KnowledgeBaseProvider,
    ListPagesParams, Pagination, ProviderResult, Result, SearchKbParams, UpdatePageParams,
};
use reqwest::RequestBuilder;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
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

#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct ConfluenceListResponse<T> {
    #[serde(default)]
    results: Vec<T>,
    #[serde(default)]
    start: Option<u32>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    size: Option<u32>,
    #[serde(default, rename = "totalSize")]
    total_size: Option<u32>,
    #[serde(default)]
    _links: ConfluenceLinks,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ConfluenceLinks {
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    webui: Option<String>,
    #[serde(default)]
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConfluenceSpace {
    id: String,
    key: String,
    name: String,
    #[serde(rename = "type", default)]
    space_type: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    description: Option<ConfluenceSpaceDescription>,
    #[serde(default)]
    _links: ConfluenceLinks,
}

#[derive(Debug, Deserialize)]
struct ConfluenceSpaceDescription {
    #[serde(default)]
    plain: Option<ConfluenceValueContainer>,
    #[serde(default)]
    view: Option<ConfluenceValueContainer>,
}

#[derive(Debug, Deserialize)]
struct ConfluenceValueContainer {
    #[serde(default)]
    value: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluencePage {
    id: String,
    title: String,
    #[serde(default)]
    space: Option<ConfluenceSpaceRef>,
    #[serde(default)]
    version: Option<ConfluenceVersion>,
    #[serde(default)]
    history: Option<ConfluenceHistory>,
    #[serde(default)]
    body: Option<ConfluenceBody>,
    #[serde(default)]
    metadata: Option<ConfluenceMetadata>,
    #[serde(default)]
    ancestors: Vec<ConfluenceAncestor>,
    #[serde(default)]
    _links: ConfluenceLinks,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceSpaceRef {
    #[serde(default)]
    key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceVersion {
    #[serde(default)]
    number: Option<u32>,
    #[serde(default)]
    when: Option<String>,
    #[serde(default)]
    by: Option<ConfluenceUser>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceHistory {
    #[serde(default, rename = "lastUpdated")]
    last_updated: Option<ConfluenceVersion>,
    #[serde(default, rename = "createdBy")]
    created_by: Option<ConfluenceUser>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceUser {
    #[serde(default, rename = "displayName")]
    display_name: Option<String>,
    #[serde(default)]
    username: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceBody {
    #[serde(default)]
    storage: Option<ConfluenceBodyValue>,
    #[serde(default)]
    view: Option<ConfluenceBodyValue>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceBodyValue {
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    representation: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceMetadata {
    #[serde(default)]
    labels: Option<ConfluenceLabelList>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceLabelList {
    #[serde(default)]
    results: Vec<ConfluenceLabel>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceLabel {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceAncestor {
    id: String,
    title: String,
    #[serde(default)]
    _links: ConfluenceLinks,
}

fn join_link(base_url: &str, base_hint: Option<&str>, path: Option<&str>) -> Option<String> {
    let path = path?;
    if path.starts_with("http://") || path.starts_with("https://") {
        return Some(path.to_string());
    }
    let base = base_hint.unwrap_or(base_url).trim_end_matches('/');
    if path.starts_with('/') {
        Some(format!("{base}{path}"))
    } else {
        Some(format!("{base}/{path}"))
    }
}

fn display_name(user: Option<&ConfluenceUser>) -> Option<String> {
    user.and_then(|u| u.display_name.clone().or_else(|| u.username.clone()))
}

fn page_excerpt(page: &ConfluencePage) -> Option<String> {
    page.body
        .as_ref()
        .and_then(|body| body.view.as_ref().or(body.storage.as_ref()))
        .and_then(|body| body.value.clone())
        .map(|value| truncate_string(strip_html_tags(&value), 280))
        .filter(|value| !value.is_empty())
}

fn strip_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_string(input: String, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input;
    }
    input.chars().take(max_chars).collect::<String>()
}

fn map_space(base_url: &str, raw: ConfluenceSpace) -> KbSpace {
    let description = raw
        .description
        .and_then(|d| {
            d.plain
                .and_then(|v| v.value)
                .or_else(|| d.view.and_then(|v| v.value))
        })
        .map(|value| truncate_string(strip_html_tags(&value), 500))
        .filter(|value| !value.is_empty());

    KbSpace {
        id: raw.id,
        key: raw.key,
        name: raw.name,
        space_type: raw.space_type,
        status: raw.status,
        description,
        url: join_link(
            base_url,
            raw._links.base.as_deref(),
            raw._links.webui.as_deref(),
        ),
    }
}

fn map_page_summary(base_url: &str, raw: &ConfluencePage) -> KbPage {
    let version = raw
        .history
        .as_ref()
        .and_then(|h| h.last_updated.as_ref())
        .or(raw.version.as_ref());

    KbPage {
        id: raw.id.clone(),
        title: raw.title.clone(),
        space_key: raw.space.as_ref().and_then(|space| space.key.clone()),
        url: join_link(
            base_url,
            raw._links.base.as_deref(),
            raw._links.webui.as_deref(),
        ),
        version: version.and_then(|v| v.number),
        last_modified: version.and_then(|v| v.when.clone()),
        author: display_name(version.and_then(|v| v.by.as_ref()))
            .or_else(|| display_name(raw.history.as_ref().and_then(|h| h.created_by.as_ref()))),
        excerpt: page_excerpt(raw),
    }
}

fn map_pagination<T>(
    response: &ConfluenceListResponse<T>,
    requested_limit: Option<u32>,
) -> Pagination {
    let offset = response.start.unwrap_or(0);
    let limit = requested_limit
        .or(response.limit)
        .or(response.size)
        .unwrap_or(response.results.len() as u32);
    let total = response.total_size;
    let has_more = response._links.next.is_some()
        || total
            .map(|total| {
                offset.saturating_add(response.size.unwrap_or(response.results.len() as u32))
                    < total
            })
            .unwrap_or(false);

    Pagination {
        offset,
        limit,
        total,
        has_more,
        next_cursor: response._links.next.clone(),
    }
}

fn encode_query_value(value: &str) -> String {
    value.replace(' ', "%20")
}

fn escape_cql_string(value: &str) -> String {
    value.replace('\\', r"\\").replace('"', r#"\""#)
}

fn build_search_cql(params: &SearchKbParams) -> String {
    if params.raw_query {
        return params.query.clone();
    }

    let mut parts = vec!["type = page".to_string()];
    if let Some(space_key) = params.space_key.as_ref() {
        parts.push(format!("space = \"{}\"", escape_cql_string(space_key)));
    }
    parts.push(format!("text ~ \"{}\"", escape_cql_string(&params.query)));
    parts.join(" AND ")
}

fn search_path_from_cursor(cursor: &str) -> String {
    if let Some(path) = cursor.strip_prefix("/rest/api/") {
        path.to_string()
    } else if let Some(path) = cursor.strip_prefix(DEFAULT_CONFLUENCE_API_PATH) {
        path.trim_start_matches('/').to_string()
    } else if let Some(path) = cursor.strip_prefix("http://") {
        let path = path.split_once("/rest/api/").map(|(_, rhs)| rhs);
        path.unwrap_or(cursor).to_string()
    } else if let Some(path) = cursor.strip_prefix("https://") {
        let path = path.split_once("/rest/api/").map(|(_, rhs)| rhs);
        path.unwrap_or(cursor).to_string()
    } else {
        cursor.trim_start_matches('/').to_string()
    }
}

#[async_trait]
impl KnowledgeBaseProvider for ConfluenceClient {
    fn provider_name(&self) -> &'static str {
        "confluence"
    }

    async fn get_spaces(&self) -> Result<ProviderResult<KbSpace>> {
        let response: ConfluenceListResponse<ConfluenceSpace> = self
            .get_json("space?limit=100&type=global,personal")
            .await?;
        let pagination = map_pagination(&response, Some(100));
        let items = response
            .results
            .into_iter()
            .map(|space| map_space(&self.base_url, space))
            .collect::<Vec<_>>();

        Ok(ProviderResult::new(items).with_pagination(pagination))
    }

    async fn list_pages(&self, params: ListPagesParams) -> Result<ProviderResult<KbPage>> {
        let limit = params.limit.unwrap_or(25);
        let offset = params.offset.unwrap_or(0);

        let query = [
            format!("spaceKey={}", encode_query_value(&params.space_key)),
            "type=page".to_string(),
            format!("limit={limit}"),
            format!("start={offset}"),
            "expand=space,version,history.lastUpdated,body.view,ancestors".to_string(),
        ];

        let path = format!("content?{}", query.join("&"));
        let response: ConfluenceListResponse<ConfluencePage> = self.get_json(&path).await?;
        let pagination = map_pagination(&response, Some(limit));
        let mut items = response
            .results
            .iter()
            .filter(|page| {
                params
                    .parent_id
                    .as_ref()
                    .map(|parent_id| {
                        page.ancestors
                            .last()
                            .map(|ancestor| ancestor.id == *parent_id)
                            .unwrap_or(false)
                    })
                    .unwrap_or(true)
            })
            .map(|page| map_page_summary(&self.base_url, page))
            .collect::<Vec<_>>();

        if let Some(search) = params.search.as_ref() {
            let search = search.to_ascii_lowercase();
            items.retain(|page| {
                page.title.to_ascii_lowercase().contains(&search)
                    || page
                        .excerpt
                        .as_ref()
                        .map(|excerpt| excerpt.to_ascii_lowercase().contains(&search))
                        .unwrap_or(false)
            });
        }

        Ok(ProviderResult::new(items).with_pagination(pagination))
    }

    async fn get_page(&self, page_id: &str) -> Result<KbPageContent> {
        let path = format!(
            "content/{page_id}?expand=space,version,history.lastUpdated,body.storage,metadata.labels,ancestors"
        );
        let page: ConfluencePage = self.get_json(&path).await?;
        let summary = map_page_summary(&self.base_url, &page);
        let content = page
            .body
            .as_ref()
            .and_then(|body| body.storage.as_ref())
            .and_then(|storage| storage.value.clone())
            .unwrap_or_default();
        let content_type = page
            .body
            .as_ref()
            .and_then(|body| body.storage.as_ref())
            .and_then(|storage| storage.representation.clone())
            .unwrap_or_else(|| "storage".to_string());
        let ancestors = page
            .ancestors
            .iter()
            .map(|ancestor| KbPage {
                id: ancestor.id.clone(),
                title: ancestor.title.clone(),
                space_key: None,
                url: join_link(
                    &self.base_url,
                    ancestor._links.base.as_deref(),
                    ancestor._links.webui.as_deref(),
                ),
                version: None,
                last_modified: None,
                author: None,
                excerpt: None,
            })
            .collect();
        let labels = page
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.labels.as_ref())
            .map(|labels| {
                labels
                    .results
                    .iter()
                    .filter_map(|label| label.name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Ok(KbPageContent {
            page: summary,
            content,
            content_type,
            ancestors,
            labels,
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

    async fn search(&self, params: SearchKbParams) -> Result<ProviderResult<KbPage>> {
        let limit = params.limit.unwrap_or(25);

        let path = if let Some(cursor) = params.cursor.as_ref() {
            search_path_from_cursor(cursor)
        } else {
            let cql = build_search_cql(&params);
            format!(
                "content/search?cql={}&limit={limit}&expand=space,version,history.lastUpdated,body.view",
                encode_query_value(&cql)
            )
        };

        let response: ConfluenceListResponse<ConfluencePage> = self.get_json(&path).await?;
        let pagination = map_pagination(&response, Some(limit));
        let items = response
            .results
            .iter()
            .map(|page| map_page_summary(&self.base_url, page))
            .collect::<Vec<_>>();

        Ok(ProviderResult::new(items).with_pagination(pagination))
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

    #[tokio::test]
    async fn get_spaces_maps_confluence_spaces() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/space")
                .query_param("limit", "100")
                .query_param("type", "global,personal");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [
                            {
                                "id": "123",
                                "key": "ENG",
                                "name": "Engineering",
                                "type": "global",
                                "status": "current",
                                "description": { "plain": { "value": "Team docs" } },
                                "_links": { "base": "https://wiki.example.com", "webui": "/spaces/ENG/overview" }
                            }
                        ],
                        "start": 0,
                        "limit": 100,
                        "size": 1,
                        "totalSize": 1,
                        "_links": {}
                    }"#,
                );
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        );
        let result = client.get_spaces().await.unwrap();

        mock.assert();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].key, "ENG");
        assert_eq!(result.items[0].name, "Engineering");
        assert_eq!(result.items[0].description.as_deref(), Some("Team docs"));
        assert_eq!(
            result.items[0].url.as_deref(),
            Some("https://wiki.example.com/spaces/ENG/overview")
        );
        assert_eq!(result.pagination.unwrap().total, Some(1));
    }

    #[tokio::test]
    async fn list_pages_maps_page_summaries_and_pagination() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content")
                .query_param("spaceKey", "ENG")
                .query_param("type", "page")
                .query_param("limit", "25")
                .query_param("start", "0")
                .query_param("expand", "space,version,history.lastUpdated,body.view,ancestors");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [
                            {
                                "id": "42",
                                "title": "ADR-001",
                                "space": { "key": "ENG" },
                                "version": {
                                    "number": 7,
                                    "when": "2026-04-26T10:00:00.000Z",
                                    "by": { "displayName": "Alice" }
                                },
                                "body": {
                                    "view": { "value": "<p>Architecture decision record</p>", "representation": "view" }
                                },
                                "ancestors": [],
                                "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=42", "next": "/rest/api/content?start=25" }
                            }
                        ],
                        "start": 0,
                        "limit": 25,
                        "size": 1,
                        "totalSize": 30,
                        "_links": { "next": "/rest/api/content?start=25" }
                    }"#,
                );
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        );
        let result = client
            .list_pages(ListPagesParams {
                space_key: "ENG".into(),
                limit: Some(25),
                offset: Some(0),
                cursor: None,
                search: None,
                parent_id: None,
            })
            .await
            .unwrap();

        mock.assert();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "42");
        assert_eq!(result.items[0].space_key.as_deref(), Some("ENG"));
        assert_eq!(result.items[0].version, Some(7));
        assert_eq!(result.items[0].author.as_deref(), Some("Alice"));
        assert_eq!(
            result.items[0].excerpt.as_deref(),
            Some("Architecture decision record")
        );
        let pagination = result.pagination.unwrap();
        assert!(pagination.has_more);
        assert_eq!(
            pagination.next_cursor.as_deref(),
            Some("/rest/api/content?start=25")
        );
        assert_eq!(pagination.total, Some(30));
    }

    #[tokio::test]
    async fn get_page_maps_storage_content_labels_and_ancestors() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content/42")
                .query_param(
                    "expand",
                    "space,version,history.lastUpdated,body.storage,metadata.labels,ancestors",
                );
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "42",
                        "title": "ADR-001",
                        "space": { "key": "ENG" },
                        "version": {
                            "number": 7,
                            "when": "2026-04-26T10:00:00.000Z",
                            "by": { "displayName": "Alice" }
                        },
                        "body": {
                            "storage": {
                                "value": "<p>Hello <strong>world</strong></p>",
                                "representation": "storage"
                            }
                        },
                        "metadata": {
                            "labels": {
                                "results": [
                                    { "name": "adr" },
                                    { "name": "architecture" }
                                ]
                            }
                        },
                        "ancestors": [
                            {
                                "id": "10",
                                "title": "Architecture Decisions",
                                "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=10" }
                            }
                        ],
                        "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=42" }
                    }"#,
                );
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        );
        let page = client.get_page("42").await.unwrap();

        mock.assert();
        assert_eq!(page.page.id, "42");
        assert_eq!(page.page.title, "ADR-001");
        assert_eq!(page.page.version, Some(7));
        assert_eq!(page.content_type, "storage");
        assert_eq!(page.content, "<p>Hello <strong>world</strong></p>");
        assert_eq!(page.labels, vec!["adr", "architecture"]);
        assert_eq!(page.ancestors.len(), 1);
        assert_eq!(page.ancestors[0].id, "10");
        assert_eq!(page.ancestors[0].title, "Architecture Decisions");
    }

    #[tokio::test]
    async fn search_builds_free_text_cql_and_maps_results() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content/search")
                .query_param("cql", "type = page AND space = \"ENG\" AND text ~ \"architecture\"")
                .query_param("limit", "10")
                .query_param("expand", "space,version,history.lastUpdated,body.view");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [
                            {
                                "id": "99",
                                "title": "Architecture Overview",
                                "space": { "key": "ENG" },
                                "version": {
                                    "number": 3,
                                    "when": "2026-04-26T10:00:00.000Z",
                                    "by": { "displayName": "Alice" }
                                },
                                "body": {
                                    "view": { "value": "<p>System architecture</p>", "representation": "view" }
                                },
                                "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=99" }
                            }
                        ],
                        "start": 0,
                        "limit": 10,
                        "size": 1,
                        "totalSize": 1,
                        "_links": {}
                    }"#,
                );
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        );
        let result = client
            .search(SearchKbParams {
                query: "architecture".into(),
                space_key: Some("ENG".into()),
                cursor: None,
                limit: Some(10),
                raw_query: false,
            })
            .await
            .unwrap();

        mock.assert();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "99");
        assert_eq!(result.items[0].title, "Architecture Overview");
        assert_eq!(result.items[0].space_key.as_deref(), Some("ENG"));
    }

    #[tokio::test]
    async fn search_uses_raw_cql_and_cursor_path() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content/search")
                .query_param("cql", "label = \"adr\"")
                .query_param("limit", "5")
                .query_param("expand", "space,version,history.lastUpdated,body.view");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [],
                        "start": 0,
                        "limit": 5,
                        "size": 0,
                        "totalSize": 6,
                        "_links": { "next": "/rest/api/content/search?cql=label%20%3D%20%22adr%22&limit=5&start=5" }
                    }"#,
                );
        });
        let next_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content/search")
                .query_param("cql", "label = \"adr\"")
                .query_param("limit", "5")
                .query_param("start", "5");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [
                            {
                                "id": "123",
                                "title": "ADR-123",
                                "space": { "key": "ENG" },
                                "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=123" }
                            }
                        ],
                        "start": 5,
                        "limit": 5,
                        "size": 1,
                        "totalSize": 6,
                        "_links": {}
                    }"#,
                );
        });

        let client = ConfluenceClient::new(
            server.base_url(),
            ConfluenceAuth::BearerToken("secret-token".into()),
        );
        let first = client
            .search(SearchKbParams {
                query: r#"label = "adr""#.into(),
                space_key: None,
                cursor: None,
                limit: Some(5),
                raw_query: true,
            })
            .await
            .unwrap();
        let next_cursor = first.pagination.as_ref().and_then(|p| p.next_cursor.clone());

        mock.assert();
        assert!(first.items.is_empty());
        assert_eq!(
            next_cursor.as_deref(),
            Some("/rest/api/content/search?cql=label%20%3D%20%22adr%22&limit=5&start=5")
        );

        let second = client
            .search(SearchKbParams {
                query: String::new(),
                space_key: None,
                cursor: next_cursor,
                limit: Some(5),
                raw_query: true,
            })
            .await
            .unwrap();

        next_mock.assert();
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].id, "123");
        assert_eq!(second.items[0].title, "ADR-123");
    }
}
