use std::collections::HashMap;
use std::fmt;

use async_trait::async_trait;
use devboy_core::{
    CreatePageParams, Error, KbPage, KbPageContent, KbSpace, KnowledgeBaseProvider,
    ListPagesParams, Pagination, ProviderResult, Result, SearchKbParams, UpdatePageParams,
};
use reqwest::RequestBuilder;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::DEFAULT_CONFLUENCE_API_PATH;

#[derive(Clone)]
pub enum ConfluenceFlavor {
    SelfHosted,
    Cloud,
}

impl fmt::Debug for ConfluenceFlavor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SelfHosted => f.write_str("SelfHosted"),
            Self::Cloud => f.write_str("Cloud"),
        }
    }
}

impl Default for ConfluenceFlavor {
    fn default() -> Self {
        Self::SelfHosted
    }
}

#[derive(Clone)]
pub enum ConfluenceAuth {
    None,
    BearerToken(SecretString),
    Basic {
        username: String,
        password: SecretString,
    },
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

impl ConfluenceAuth {
    /// Build a bearer-token auth from any string-like input.
    pub fn bearer(token: impl Into<String>) -> Self {
        Self::BearerToken(SecretString::from(token.into()))
    }

    /// Build a basic-auth pair from username and password strings.
    pub fn basic(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self::Basic {
            username: username.into(),
            password: SecretString::from(password.into()),
        }
    }
}

#[derive(Clone)]
pub struct ConfluenceClient {
    base_url: String,
    flavor: ConfluenceFlavor,
    /// Original Confluence instance URL for generating browse links
    /// (`_links.webui`, `/pages/<id>`). When the client is configured
    /// for proxy mode, `base_url` points at the proxy host so API
    /// requests transit it, while `instance_url` stays pinned to the
    /// real Confluence host so URLs returned in responses remain
    /// clickable. Defaults to `base_url` when not overridden.
    instance_url: String,
    api_path: String,
    page_api_path: String,
    space_api_path: String,
    auth: ConfluenceAuth,
    proxy_headers: Option<HashMap<String, String>>,
    http: reqwest::Client,
}

impl fmt::Debug for ConfluenceClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfluenceClient")
            .field("base_url", &self.base_url)
            .field("flavor", &self.flavor)
            .field("instance_url", &self.instance_url)
            .field("api_path", &self.api_path)
            .field("page_api_path", &self.page_api_path)
            .field("space_api_path", &self.space_api_path)
            .field("auth", &self.auth)
            .field("http", &self.http)
            .finish()
    }
}

impl ConfluenceClient {
    pub fn new(base_url: impl Into<String>, auth: ConfluenceAuth) -> Self {
        let base = normalize_base_url(base_url.into());
        Self {
            instance_url: base.clone(),
            base_url: base,
            flavor: ConfluenceFlavor::default(),
            api_path: DEFAULT_CONFLUENCE_API_PATH.to_string(),
            page_api_path: DEFAULT_CONFLUENCE_API_PATH.to_string(),
            space_api_path: DEFAULT_CONFLUENCE_API_PATH.to_string(),
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

    /// Real Confluence host used in user-facing links. Equal to
    /// `base_url()` unless `with_instance_url` was called (the typical
    /// proxy case).
    pub fn instance_url(&self) -> &str {
        &self.instance_url
    }

    pub fn auth(&self) -> &ConfluenceAuth {
        &self.auth
    }

    pub fn flavor(&self) -> &ConfluenceFlavor {
        &self.flavor
    }

    pub fn with_flavor(mut self, flavor: ConfluenceFlavor) -> Self {
        self.flavor = flavor;
        self
    }

    pub fn with_api_version(mut self, api_version: Option<&str>) -> Self {
        self.page_api_path = api_path_for_version(api_version);
        self.space_api_path = api_path_for_version(api_version);
        self
    }

    /// Configure proxy mode with headers added to every request.
    /// When proxy is active, provider auth headers are suppressed.
    /// Note: this does **not** change browse-link generation — set
    /// `with_instance_url` to the real Confluence host so links in
    /// responses don't point at the proxy.
    pub fn with_proxy(mut self, headers: HashMap<String, String>) -> Self {
        self.proxy_headers = Some(headers);
        self
    }

    /// Override the host used for generating browse links (`_links.webui`,
    /// `/pages/<id>`). Useful when `base_url` is a proxy URL — callers
    /// would otherwise see proxy-host URLs in tool responses.
    pub fn with_instance_url(mut self, url: impl Into<String>) -> Self {
        self.instance_url = normalize_base_url(url.into());
        self
    }

    pub fn rest_api_url(&self, path: &str) -> String {
        self.api_url(&self.api_path, path)
    }

    fn api_url(&self, api_path: &str, path: &str) -> String {
        let path = path.trim_start_matches('/');
        format!("{}{}/{}", self.base_url, api_path, path)
    }

    #[cfg(test)]
    fn space_api_url(&self, path: &str) -> String {
        self.api_url(&self.space_api_path, path)
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

    async fn get_json_from_api<T>(&self, api_path: &str, path: &str) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let request = self
            .http
            .get(self.api_url(api_path, path))
            .header(reqwest::header::ACCEPT, "application/json");
        self.send_json(request).await
    }

    async fn post_json_to_api<T, B>(&self, api_path: &str, path: &str, body: &B) -> Result<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let request = self
            .http
            .post(self.api_url(api_path, path))
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body);
        self.send_json(request).await
    }

    async fn put_json_to_api<T, B>(&self, api_path: &str, path: &str, body: &B) -> Result<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let request = self
            .http
            .put(self.api_url(api_path, path))
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body);
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

    pub async fn post_empty_json<B>(&self, path: &str, body: &B) -> Result<()>
    where
        B: Serialize + ?Sized,
    {
        let request = self
            .http
            .post(self.rest_api_url(path))
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body);
        self.send_empty(request).await
    }

    pub async fn delete_empty(&self, path: &str) -> Result<()> {
        let request = self
            .http
            .delete(self.rest_api_url(path))
            .header(reqwest::header::ACCEPT, "application/json");
        self.send_empty(request).await
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
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        // Read once as bytes — `from_slice` lets us decode the happy
        // path without an extra UTF-8 validation pass over multi-MB
        // page bodies (Copilot review on PR #286). Lossy decode is
        // reserved for the error branch when we actually need a
        // human-readable preview.
        let body = response
            .bytes()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        if !status.is_success() {
            return Err(Error::from_status(
                status.as_u16(),
                String::from_utf8_lossy(&body).into_owned(),
            ));
        }

        serde_json::from_slice::<T>(&body).map_err(|e| {
            // Surface enough context to diagnose self-hosted misconfigs:
            // a successful HTTP status with a non-JSON body usually means
            // the request landed on an auth gateway (HTML login page) or
            // a reverse proxy that rewrote the response.
            let preview_bytes = &body[..body.len().min(200)];
            let preview = String::from_utf8_lossy(preview_bytes);
            Error::InvalidData(format!(
                "JSON decode failed (status={}, content-type='{}'): {}; body preview: {:?}",
                status, content_type, e, preview
            ))
        })
    }

    async fn send_empty(&self, request: RequestBuilder) -> Result<()> {
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

        Ok(())
    }

    fn apply_auth(&self, request: RequestBuilder) -> RequestBuilder {
        if let Some(headers) = &self.proxy_headers {
            return request.headers(proxy_headers_to_headermap(headers));
        }

        match &self.auth {
            ConfluenceAuth::None => request,
            ConfluenceAuth::BearerToken(token) => request.bearer_auth(token.expose_secret()),
            ConfluenceAuth::Basic { username, password } => {
                request.basic_auth(username, Some(password.expose_secret()))
            }
        }
    }
}

fn should_fallback_to_rest_api(error: &Error) -> bool {
    matches!(
        error,
        Error::NotFound(_)
            | Error::Api {
                status: 400 | 404 | 405,
                ..
            }
    )
}

fn uses_v2_api(api_path: &str) -> bool {
    api_path == "/api/v2"
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

fn api_path_for_version(api_version: Option<&str>) -> String {
    match api_version.map(str::trim).filter(|v| !v.is_empty()) {
        Some("v2") => "/api/v2".to_string(),
        _ => DEFAULT_CONFLUENCE_API_PATH.to_string(),
    }
}

// Cloud Confluence v2 returns object ids as strings; on-prem Server / DC
// `/rest/api/space` returns them as JSON integers. Accept both so the
// same code path works against either flavour.
fn deserialize_id_string_or_int<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrInt {
        String(String),
        Int(i64),
    }
    Ok(match StringOrInt::deserialize(deserializer)? {
        StringOrInt::String(s) => s,
        StringOrInt::Int(i) => i.to_string(),
    })
}

fn deserialize_opt_id_string_or_int<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrInt {
        String(String),
        Int(i64),
    }
    Ok(
        Option::<StringOrInt>::deserialize(deserializer)?.map(|v| match v {
            StringOrInt::String(s) => s,
            StringOrInt::Int(i) => i.to_string(),
        }),
    )
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
    #[serde(deserialize_with = "deserialize_id_string_or_int")]
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
    #[serde(deserialize_with = "deserialize_id_string_or_int")]
    id: String,
    title: String,
    #[serde(default)]
    space: Option<ConfluenceSpaceRef>,
    #[serde(
        default,
        rename = "spaceId",
        deserialize_with = "deserialize_opt_id_string_or_int"
    )]
    space_id: Option<String>,
    #[serde(
        default,
        rename = "parentId",
        deserialize_with = "deserialize_opt_id_string_or_int"
    )]
    parent_id: Option<String>,
    #[serde(default)]
    version: Option<ConfluenceVersion>,
    #[serde(default)]
    history: Option<ConfluenceHistory>,
    #[serde(default)]
    body: Option<ConfluenceBody>,
    #[serde(default)]
    metadata: Option<ConfluenceMetadata>,
    #[serde(default)]
    labels: Option<ConfluenceLabelList>,
    #[serde(default)]
    ancestors: Vec<ConfluenceAncestor>,
    #[serde(default)]
    _links: ConfluenceLinks,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceSpaceRef {
    #[serde(default, deserialize_with = "deserialize_opt_id_string_or_int")]
    id: Option<String>,
    #[serde(default)]
    key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceVersion {
    #[serde(default)]
    number: Option<u32>,
    #[serde(default, rename = "createdAt")]
    created_at: Option<String>,
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
    #[serde(default, rename = "accountId")]
    account_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceBody {
    #[serde(default)]
    storage: Option<ConfluenceBodyValue>,
    #[serde(default)]
    view: Option<ConfluenceBodyValue>,
    #[serde(default)]
    value: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceBodyValue {
    #[serde(default)]
    value: Option<String>,
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
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Serialize)]
struct ConfluenceWriteLabel<'a> {
    prefix: &'static str,
    name: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfluenceAncestor {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    _links: ConfluenceLinks,
}

#[derive(Debug, Serialize)]
struct ConfluenceContentBody<'a> {
    value: &'a str,
    representation: &'static str,
}

#[derive(Debug, Serialize)]
struct ConfluenceContentPayload<'a> {
    #[serde(rename = "type")]
    content_type: &'static str,
    title: &'a str,
    space: ConfluenceCreateSpaceRef<'a>,
    body: ConfluenceCreateBodyPayload<'a>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ancestors: Vec<ConfluenceCreateAncestorRef<'a>>,
}

#[derive(Debug, Serialize)]
struct ConfluenceCreateSpaceRef<'a> {
    key: &'a str,
}

#[derive(Debug, Serialize)]
struct ConfluenceCreateBodyPayload<'a> {
    storage: ConfluenceContentBody<'a>,
}

#[derive(Debug, Serialize)]
struct ConfluenceCreateAncestorRef<'a> {
    id: &'a str,
}

#[derive(Debug, Serialize)]
struct ConfluenceUpdatePayload<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    content_type: &'static str,
    title: &'a str,
    version: ConfluenceUpdateVersion,
    body: ConfluenceCreateBodyPayload<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ancestors: Option<Vec<ConfluenceCreateAncestorRef<'a>>>,
}

#[derive(Debug, Serialize)]
struct ConfluenceUpdateVersion {
    number: u32,
}

#[derive(Debug, Serialize)]
struct ConfluenceV2PagePayload<'a> {
    #[serde(rename = "spaceId")]
    space_id: &'a str,
    status: &'static str,
    title: &'a str,
    #[serde(rename = "parentId", skip_serializing_if = "Option::is_none")]
    parent_id: Option<&'a str>,
    body: ConfluenceContentBody<'a>,
}

#[derive(Debug, Serialize)]
struct ConfluenceV2UpdatePayload<'a> {
    id: &'a str,
    status: &'static str,
    title: &'a str,
    #[serde(rename = "spaceId")]
    space_id: &'a str,
    #[serde(rename = "parentId", skip_serializing_if = "Option::is_none")]
    parent_id: Option<&'a str>,
    body: ConfluenceContentBody<'a>,
    version: ConfluenceUpdateVersion,
}

/// Build a fully-qualified browse URL out of a relative path returned
/// by Confluence (`_links.webui`).
///
/// Historically this honoured `_links.base` from the response over the
/// caller-supplied `base_url`, on the theory that Confluence knows its
/// own canonical host better than the client. In proxy mode that flips
/// against us: if the upstream is fronted by a reverse proxy that
/// rewrites `_links.base` to the proxy host (or worse, an internal
/// hostname unreachable from the client), every link returned to the
/// caller would point at the wrong place.
///
/// `instance_url` (DEV / ADR) is the single source of truth for the
/// user-facing host; only fall back to `base_hint` when it shares the
/// same host as `base_url` (a tail-path override like `/wiki` on
/// Cloud) or when the upstream returned an absolute URL on the same
/// host. Cross-host hints are ignored.
fn join_link(base_url: &str, base_hint: Option<&str>, path: Option<&str>) -> Option<String> {
    let path = path?;
    if path.starts_with("http://") || path.starts_with("https://") {
        return Some(path.to_string());
    }
    let base = base_url.trim_end_matches('/');
    // `_links.base` is honoured only when it stays on the same host as
    // `base_url` (i.e. is just a path-prefix variant, e.g. `/wiki`).
    // Cross-host hints — including the proxy host upstream might
    // advertise — are discarded so links always come out clickable.
    let effective_base = match base_hint {
        Some(hint) if same_host_prefix(base, hint) => hint.trim_end_matches('/'),
        _ => base,
    };
    if path.starts_with('/') {
        Some(format!("{effective_base}{path}"))
    } else {
        Some(format!("{effective_base}/{path}"))
    }
}

/// True when `hint` is an absolute URL on the same scheme+host as
/// `base_url`, or a relative path. Anything else (different host,
/// different scheme) is treated as untrusted.
fn same_host_prefix(base_url: &str, hint: &str) -> bool {
    if hint.starts_with('/') || !hint.contains("://") {
        // Relative — by definition same host.
        return true;
    }
    let base_origin = url_origin(base_url);
    let hint_origin = url_origin(hint);
    match (base_origin, hint_origin) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Return the `scheme://host[:port]` part of a URL, lowercased, without
/// any trailing slash. `None` if the input isn't a recognisable
/// absolute URL.
fn url_origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split('/').next()?;
    if host.is_empty() {
        return None;
    }
    Some(format!("{}://{}", scheme.to_ascii_lowercase(), host))
}

fn display_name(user: Option<&ConfluenceUser>) -> Option<String> {
    user.and_then(|u| {
        u.display_name
            .clone()
            .or_else(|| u.username.clone())
            .or_else(|| u.account_id.clone())
    })
}

fn body_value(body: &ConfluenceBody) -> Option<String> {
    body.view
        .as_ref()
        .and_then(|value| value.value.clone())
        .or_else(|| body.storage.as_ref().and_then(|value| value.value.clone()))
        .or_else(|| body.value.clone())
}

fn extract_labels(page: &ConfluencePage) -> Vec<String> {
    page.labels
        .as_ref()
        .or_else(|| {
            page.metadata
                .as_ref()
                .and_then(|metadata| metadata.labels.as_ref())
        })
        .map(|labels| {
            labels
                .results
                .iter()
                .filter_map(|label| label.name.clone().or_else(|| label.label.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn normalize_labels(labels: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for label in labels {
        let trimmed = label.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !out.iter().any(|existing| existing == trimmed) {
            out.push(trimmed.to_string());
        }
    }
    out
}

fn page_excerpt(page: &ConfluencePage) -> Option<String> {
    page.body
        .as_ref()
        .and_then(body_value)
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

fn strip_html_tags_preserve_layout(input: &str) -> String {
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
    out
}

fn truncate_string(input: String, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input;
    }
    input.chars().take(max_chars).collect::<String>()
}

fn normalize_confluence_write_content(content: &str, content_type: Option<&str>) -> Result<String> {
    match content_type
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("markdown")
    {
        "markdown" => Ok(markdown_to_confluence_storage(content)),
        "html" => Ok(html_to_confluence_storage(content)),
        "storage" => Ok(content.to_string()),
        other => Err(Error::InvalidData(format!(
            "unsupported confluence content_type '{other}', expected markdown, html, or storage"
        ))),
    }
}

fn html_to_confluence_storage(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.contains('<') && trimmed.contains('>') {
        trimmed.to_string()
    } else {
        format!("<p>{}</p>", escape_html(trimmed))
    }
}

fn markdown_to_confluence_storage(markdown: &str) -> String {
    let markdown = markdown.replace("\r\n", "\n");
    let mut out = String::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut in_ul = false;
    let mut in_ol = false;
    let mut lines = markdown.lines().peekable();

    let flush_paragraph = |out: &mut String, paragraph: &mut Vec<String>| {
        if paragraph.is_empty() {
            return;
        }
        let text = paragraph.join(" ");
        out.push_str("<p>");
        out.push_str(&markdown_inline_to_html(&text));
        out.push_str("</p>");
        paragraph.clear();
    };

    let close_lists = |out: &mut String, in_ul: &mut bool, in_ol: &mut bool| {
        if *in_ul {
            out.push_str("</ul>");
            *in_ul = false;
        }
        if *in_ol {
            out.push_str("</ol>");
            *in_ol = false;
        }
    };

    while let Some(line) = lines.next() {
        let trimmed = line.trim();

        if trimmed.starts_with("```") {
            flush_paragraph(&mut out, &mut paragraph);
            close_lists(&mut out, &mut in_ul, &mut in_ol);

            let mut code_lines = Vec::new();
            for code_line in lines.by_ref() {
                if code_line.trim_start().starts_with("```") {
                    break;
                }
                code_lines.push(code_line);
            }

            let code_content = code_lines.join("\n").replace("]]>", "]]]]><![CDATA[>");
            out.push_str(r#"<ac:structured-macro ac:name="code"><ac:plain-text-body><![CDATA["#);
            out.push_str(&code_content);
            out.push_str("]]></ac:plain-text-body></ac:structured-macro>");
            continue;
        }

        if trimmed.is_empty() {
            flush_paragraph(&mut out, &mut paragraph);
            close_lists(&mut out, &mut in_ul, &mut in_ol);
            continue;
        }

        if let Some((level, title)) = parse_markdown_heading(trimmed) {
            flush_paragraph(&mut out, &mut paragraph);
            close_lists(&mut out, &mut in_ul, &mut in_ol);
            out.push_str(&format!(
                "<h{level}>{}</h{level}>",
                markdown_inline_to_html(title)
            ));
            continue;
        }

        if let Some(item) = parse_unordered_list_item(trimmed) {
            flush_paragraph(&mut out, &mut paragraph);
            if in_ol {
                out.push_str("</ol>");
                in_ol = false;
            }
            if !in_ul {
                out.push_str("<ul>");
                in_ul = true;
            }
            out.push_str("<li>");
            out.push_str(&markdown_inline_to_html(item));
            out.push_str("</li>");
            continue;
        }

        if let Some(item) = parse_ordered_list_item(trimmed) {
            flush_paragraph(&mut out, &mut paragraph);
            if in_ul {
                out.push_str("</ul>");
                in_ul = false;
            }
            if !in_ol {
                out.push_str("<ol>");
                in_ol = true;
            }
            out.push_str("<li>");
            out.push_str(&markdown_inline_to_html(item));
            out.push_str("</li>");
            continue;
        }

        close_lists(&mut out, &mut in_ul, &mut in_ol);
        paragraph.push(trimmed.to_string());
    }

    flush_paragraph(&mut out, &mut paragraph);
    close_lists(&mut out, &mut in_ul, &mut in_ol);
    out
}

fn parse_markdown_heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.chars().take_while(|&ch| ch == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = line.get(hashes..)?.trim_start();
    if rest.is_empty() {
        return None;
    }
    Some((hashes, rest))
}

fn parse_unordered_list_item(line: &str) -> Option<&str> {
    line.strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .map(str::trim)
}

fn parse_ordered_list_item(line: &str) -> Option<&str> {
    let digits = line.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let rest = line.get(digits..)?;
    rest.strip_prefix(". ").map(str::trim)
}

fn markdown_inline_to_html(input: &str) -> String {
    let escaped = escape_html(input);
    let linked = replace_markdown_links(&escaped);
    let code = replace_inline_delimited(&linked, "`", "<code>", "</code>");
    let bold = replace_inline_delimited(&code, "**", "<strong>", "</strong>");
    replace_inline_delimited(&bold, "*", "<em>", "</em>")
}

fn replace_markdown_links(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0usize;

    while let Some(start_rel) = input[cursor..].find('[') {
        let start = cursor + start_rel;
        out.push_str(&input[cursor..start]);

        let Some(text_end_rel) = input[start + 1..].find(']') else {
            out.push_str(&input[start..]);
            return out;
        };
        let text_end = start + 1 + text_end_rel;
        let after_bracket = text_end + 1;
        if !input[after_bracket..].starts_with('(') {
            out.push('[');
            cursor = start + 1;
            continue;
        }

        let Some(url_end_rel) = input[after_bracket + 1..].find(')') else {
            out.push_str(&input[start..]);
            return out;
        };
        let url_end = after_bracket + 1 + url_end_rel;
        let text = &input[start + 1..text_end];
        let url = &input[after_bracket + 1..url_end];
        out.push_str(&format!(r#"<a href="{url}">{text}</a>"#));
        cursor = url_end + 1;
    }

    out.push_str(&input[cursor..]);
    out
}

fn replace_inline_delimited(input: &str, delimiter: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0usize;
    let mut is_open = false;

    while let Some(found_rel) = input[cursor..].find(delimiter) {
        let found = cursor + found_rel;
        out.push_str(&input[cursor..found]);
        if is_open {
            out.push_str(close);
        } else {
            out.push_str(open);
        }
        is_open = !is_open;
        cursor = found + delimiter.len();
    }

    out.push_str(&input[cursor..]);
    if is_open && let Some(position) = out.rfind(open) {
        out.replace_range(position..position + open.len(), delimiter);
        out.push_str(delimiter);
    }
    out
}

fn confluence_storage_to_markdown(storage: &str) -> String {
    let with_code_blocks = replace_confluence_code_macros(storage);
    let with_links = replace_anchor_tags(&with_code_blocks);
    let with_formatting = replace_paired_tag(
        &replace_paired_tag(
            &replace_paired_tag(
                &replace_paired_tag(&with_links, "strong", "**", "**"),
                "b",
                "**",
                "**",
            ),
            "em",
            "*",
            "*",
        ),
        "i",
        "*",
        "*",
    );
    let with_inline_code = replace_paired_tag(&with_formatting, "code", "`", "`");
    let markdownish = with_inline_code
        .replace("<br />", "\n")
        .replace("<br/>", "\n")
        .replace("<br>", "\n")
        .replace("<p>", "")
        .replace("</p>", "\n\n")
        .replace("<div>", "")
        .replace("</div>", "\n\n")
        .replace("<ul>", "")
        .replace("</ul>", "\n")
        .replace("<ol>", "")
        .replace("</ol>", "\n")
        .replace("<li>", "- ")
        .replace("</li>", "\n")
        .replace("<h1>", "# ")
        .replace("</h1>", "\n\n")
        .replace("<h2>", "## ")
        .replace("</h2>", "\n\n")
        .replace("<h3>", "### ")
        .replace("</h3>", "\n\n")
        .replace("<h4>", "#### ")
        .replace("</h4>", "\n\n")
        .replace("<h5>", "##### ")
        .replace("</h5>", "\n\n")
        .replace("<h6>", "###### ")
        .replace("</h6>", "\n\n");

    let text = strip_html_tags_preserve_layout(&markdownish);
    collapse_markdown_whitespace(&decode_html_entities(&text))
}

fn replace_confluence_code_macros(input: &str) -> String {
    let mut out = String::new();
    let mut cursor = 0usize;
    let macro_start = r#"<ac:structured-macro ac:name="code">"#;
    let body_start = "<ac:plain-text-body><![CDATA[";
    let body_end = "]]></ac:plain-text-body>";
    let macro_end = "</ac:structured-macro>";

    while let Some(start_rel) = input[cursor..].find(macro_start) {
        let start = cursor + start_rel;
        out.push_str(&input[cursor..start]);
        let Some(code_start_rel) = input[start..].find(body_start) else {
            out.push_str(&input[start..]);
            return out;
        };
        let code_start = start + code_start_rel + body_start.len();
        let Some(code_end_rel) = input[code_start..].find(body_end) else {
            out.push_str(&input[start..]);
            return out;
        };
        let code_end = code_start + code_end_rel;
        let Some(macro_end_rel) = input[code_end..].find(macro_end) else {
            out.push_str(&input[start..]);
            return out;
        };
        let end = code_end + macro_end_rel + macro_end.len();
        let code = &input[code_start..code_end];
        out.push_str("```");
        out.push('\n');
        out.push_str(code);
        out.push('\n');
        out.push_str("```");
        cursor = end;
    }

    out.push_str(&input[cursor..]);
    out
}

fn replace_anchor_tags(input: &str) -> String {
    let mut out = String::new();
    let mut cursor = 0usize;

    while let Some(start_rel) = input[cursor..].find("<a ") {
        let start = cursor + start_rel;
        out.push_str(&input[cursor..start]);
        let Some(tag_end_rel) = input[start..].find('>') else {
            out.push_str(&input[start..]);
            return out;
        };
        let tag_end = start + tag_end_rel;
        let tag = &input[start..=tag_end];
        let Some(close_rel) = input[tag_end + 1..].find("</a>") else {
            out.push_str(&input[start..]);
            return out;
        };
        let close = tag_end + 1 + close_rel;
        let label = &input[tag_end + 1..close];
        let href = extract_attribute(tag, "href").unwrap_or_default();
        out.push('[');
        out.push_str(label);
        out.push_str("](");
        out.push_str(&href);
        out.push(')');
        cursor = close + "</a>".len();
    }

    out.push_str(&input[cursor..]);
    out
}

fn replace_paired_tag(input: &str, tag: &str, open: &str, close: &str) -> String {
    input
        .replace(&format!("<{tag}>"), open)
        .replace(&format!("</{tag}>"), close)
}

fn extract_attribute(tag: &str, attr: &str) -> Option<String> {
    let needle = format!(r#"{attr}=""#);
    let start = tag.find(&needle)? + needle.len();
    let rest = tag.get(start..)?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn collapse_markdown_whitespace(input: &str) -> String {
    let mut normalized = Vec::new();
    let mut previous_blank = false;

    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !normalized.is_empty() && !previous_blank {
                normalized.push(String::new());
                previous_blank = true;
            }
            continue;
        }

        normalized.push(trimmed.to_string());
        previous_blank = false;
    }

    normalized.join("\n").trim().to_string()
}

fn decode_html_entities(input: &str) -> String {
    input
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
    let version_number = version.and_then(|v| v.number);
    let last_modified = version.and_then(|v| v.when.clone().or_else(|| v.created_at.clone()));

    KbPage {
        id: raw.id.clone(),
        title: raw.title.clone(),
        space_key: raw.space.as_ref().and_then(|space| space.key.clone()),
        url: join_link(
            base_url,
            raw._links.base.as_deref(),
            raw._links.webui.as_deref(),
        ),
        version: version_number,
        last_modified,
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
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                encoded.push('%');
                encoded.push(HEX[(byte >> 4) as usize] as char);
                encoded.push(HEX[(byte & 0x0F) as usize] as char);
            }
        }
    }
    encoded
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

fn path_from_cursor(cursor: &str, api_path: &str) -> String {
    let api_prefix = format!("{}/", api_path.trim_end_matches('/'));
    if let Some(path) = cursor.strip_prefix(&api_prefix) {
        path.to_string()
    } else if let Some(path) = cursor.strip_prefix(api_path) {
        path.trim_start_matches('/').to_string()
    } else if let Some(path) = cursor.strip_prefix("http://") {
        let path = path.split_once(&api_prefix).map(|(_, rhs)| rhs);
        path.unwrap_or(cursor).to_string()
    } else if let Some(path) = cursor.strip_prefix("https://") {
        let path = path.split_once(&api_prefix).map(|(_, rhs)| rhs);
        path.unwrap_or(cursor).to_string()
    } else {
        cursor.trim_start_matches('/').to_string()
    }
}

impl ConfluenceClient {
    async fn resolve_space_by_key(&self, space_key: &str) -> Result<ConfluenceSpace> {
        let spaces = self.get_spaces().await?;
        spaces
            .items
            .into_iter()
            .find(|space| space.key == space_key)
            .map(|space| ConfluenceSpace {
                id: space.id,
                key: space.key,
                name: space.name,
                space_type: space.space_type,
                status: space.status,
                description: None,
                _links: ConfluenceLinks::default(),
            })
            .ok_or_else(|| Error::NotFound(format!("confluence space '{space_key}' not found")))
    }

    async fn resolve_space_key_by_id(&self, space_id: &str) -> Result<Option<String>> {
        let spaces = self.get_spaces().await?;
        Ok(spaces
            .items
            .into_iter()
            .find(|space| space.id == space_id)
            .map(|space| space.key))
    }

    async fn get_page_ancestor_chain_v2(&self, page_id: &str) -> Result<Vec<KbPage>> {
        let path = format!("pages/{page_id}/ancestors?limit=100");
        let response: ConfluenceListResponse<ConfluenceAncestor> =
            self.get_json_from_api(&self.page_api_path, &path).await?;
        let mut tasks = tokio::task::JoinSet::new();
        for (index, ancestor) in response.results.into_iter().enumerate() {
            let client = self.clone();
            tasks.spawn(async move {
                let detail_path = format!("pages/{}", ancestor.id);
                let detail: ConfluencePage = client
                    .get_json_from_api(&client.page_api_path, &detail_path)
                    .await?;
                let mut summary = map_page_summary(&client.instance_url, &detail);
                if summary.url.is_none() {
                    summary.url = Some(format!("{}/pages/{}", client.instance_url, detail.id));
                }
                Ok::<(usize, KbPage), Error>((index, summary))
            });
        }

        let mut ancestors = Vec::with_capacity(tasks.len());
        while let Some(result) = tasks.join_next().await {
            let (index, summary) = result.map_err(|error| {
                Error::Network(format!("ancestor fetch task failed: {error}"))
            })??;
            ancestors.push((index, summary));
        }
        ancestors.sort_by_key(|(index, _)| *index);
        Ok(ancestors.into_iter().map(|(_, summary)| summary).collect())
    }

    async fn add_labels(&self, page_id: &str, labels: &[String]) -> Result<()> {
        let labels = normalize_labels(labels);
        if labels.is_empty() {
            return Ok(());
        }

        let payload = labels
            .iter()
            .map(|label| ConfluenceWriteLabel {
                prefix: "global",
                name: label.as_str(),
            })
            .collect::<Vec<_>>();
        self.post_empty_json(&format!("content/{page_id}/label"), &payload)
            .await
    }

    async fn sync_labels(
        &self,
        page_id: &str,
        desired: &[String],
        current: &[String],
    ) -> Result<()> {
        let desired = normalize_labels(desired);
        let current = normalize_labels(current);

        for label in current.iter().filter(|label| !desired.contains(*label)) {
            let path = format!("content/{page_id}/label?name={}", encode_query_value(label));
            self.delete_empty(&path).await?;
        }

        let to_add = desired
            .iter()
            .filter(|label| !current.contains(*label))
            .cloned()
            .collect::<Vec<_>>();
        self.add_labels(page_id, &to_add).await
    }

    async fn get_spaces_v1(&self) -> Result<ProviderResult<KbSpace>> {
        let response: ConfluenceListResponse<ConfluenceSpace> = self
            .get_json("space?limit=100&type=global,personal")
            .await?;
        let pagination = map_pagination(&response, Some(100));
        let items = response
            .results
            .into_iter()
            .map(|space| map_space(&self.instance_url, space))
            .collect::<Vec<_>>();

        Ok(ProviderResult::new(items).with_pagination(pagination))
    }

    async fn list_pages_v1(&self, params: ListPagesParams) -> Result<ProviderResult<KbPage>> {
        let limit = params.limit.unwrap_or(25);
        let path = if let Some(cursor) = params.cursor.as_ref() {
            path_from_cursor(cursor, &self.api_path)
        } else if let Some(parent_id) = params.parent_id.as_ref() {
            let offset = params.offset.unwrap_or(0);
            let query = [
                format!("limit={limit}"),
                format!("start={offset}"),
                "expand=space,version,history.lastUpdated,body.view".to_string(),
            ];
            format!("content/{parent_id}/child/page?{}", query.join("&"))
        } else {
            let offset = params.offset.unwrap_or(0);
            let query = [
                format!("spaceKey={}", encode_query_value(&params.space_key)),
                "type=page".to_string(),
                format!("limit={limit}"),
                format!("start={offset}"),
                "expand=space,version,history.lastUpdated,body.view,ancestors".to_string(),
            ];
            format!("content?{}", query.join("&"))
        };

        let response: ConfluenceListResponse<ConfluencePage> = self.get_json(&path).await?;
        let pagination = map_pagination(&response, Some(limit));
        let mut items = response
            .results
            .iter()
            .map(|page| map_page_summary(&self.instance_url, page))
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

    async fn get_page_v1(&self, page_id: &str) -> Result<KbPageContent> {
        let path = format!(
            "content/{page_id}?expand=space,version,history.lastUpdated,body.storage,metadata.labels,ancestors"
        );
        let page: ConfluencePage = self.get_json(&path).await?;
        let summary = map_page_summary(&self.instance_url, &page);
        let storage_content = page
            .body
            .as_ref()
            .and_then(|body| body.storage.as_ref())
            .and_then(|storage| storage.value.clone())
            .unwrap_or_default();
        let content = confluence_storage_to_markdown(&storage_content);
        let content_type = "markdown".to_string();
        let ancestors = page
            .ancestors
            .iter()
            .map(|ancestor| KbPage {
                id: ancestor.id.clone(),
                title: ancestor.title.clone(),
                space_key: None,
                url: join_link(
                    &self.instance_url,
                    ancestor._links.base.as_deref(),
                    ancestor._links.webui.as_deref(),
                ),
                version: None,
                last_modified: None,
                author: None,
                excerpt: None,
            })
            .collect();
        let labels = extract_labels(&page);

        Ok(KbPageContent {
            page: summary,
            content,
            content_type,
            ancestors,
            labels,
        })
    }

    async fn create_page_v1(&self, params: CreatePageParams) -> Result<KbPage> {
        let storage_content =
            normalize_confluence_write_content(&params.content, params.content_type.as_deref())?;

        let payload = ConfluenceContentPayload {
            content_type: "page",
            title: &params.title,
            space: ConfluenceCreateSpaceRef {
                key: &params.space_key,
            },
            body: ConfluenceCreateBodyPayload {
                storage: ConfluenceContentBody {
                    value: &storage_content,
                    representation: "storage",
                },
            },
            ancestors: params
                .parent_id
                .as_deref()
                .map(|id| vec![ConfluenceCreateAncestorRef { id }])
                .unwrap_or_default(),
        };

        let page: ConfluencePage = self.post_json("content", &payload).await?;
        self.add_labels(&page.id, &params.labels).await?;
        Ok(map_page_summary(&self.instance_url, &page))
    }

    async fn update_page_v1(&self, params: UpdatePageParams) -> Result<KbPage> {
        let current_expand = if params.labels.is_some() {
            "space,version,body.storage,ancestors,metadata.labels"
        } else {
            "space,version,body.storage,ancestors"
        };
        let current_path = format!("content/{}?expand={current_expand}", params.page_id);
        let current: ConfluencePage = self.get_json(&current_path).await?;

        let current_title = current.title.clone();
        let current_content = current
            .body
            .as_ref()
            .and_then(|body| body.storage.as_ref())
            .and_then(|storage| storage.value.clone())
            .unwrap_or_default();
        let current_version = current
            .version
            .as_ref()
            .and_then(|version| version.number)
            .ok_or_else(|| {
                Error::InvalidData(format!(
                    "confluence page {} is missing a version number",
                    params.page_id
                ))
            })?;

        if let Some(expected_version) = params.version
            && expected_version != current_version
        {
            return Err(Error::Api {
                status: 409,
                message: format!(
                    "version conflict for page {}: expected current version {}, found {}",
                    params.page_id, expected_version, current_version
                ),
            });
        }

        let title = params.title.as_deref().unwrap_or(&current_title);
        let content = match params.content.as_deref() {
            Some(updated) => {
                normalize_confluence_write_content(updated, params.content_type.as_deref())?
            }
            None => current_content,
        };
        let ancestors = params
            .parent_id
            .as_deref()
            .map(|id| vec![ConfluenceCreateAncestorRef { id }]);

        let payload = ConfluenceUpdatePayload {
            id: &params.page_id,
            content_type: "page",
            title,
            version: ConfluenceUpdateVersion {
                number: current_version.saturating_add(1),
            },
            body: ConfluenceCreateBodyPayload {
                storage: ConfluenceContentBody {
                    value: &content,
                    representation: "storage",
                },
            },
            ancestors,
        };

        let path = format!("content/{}", params.page_id);
        let page: ConfluencePage = self.put_json(&path, &payload).await?;
        if let Some(labels) = params.labels.as_ref() {
            let current_labels = extract_labels(&current);
            self.sync_labels(&params.page_id, labels, &current_labels)
                .await?;
        }
        Ok(map_page_summary(&self.instance_url, &page))
    }

    async fn list_pages_v2(&self, params: ListPagesParams) -> Result<ProviderResult<KbPage>> {
        let limit = params.limit.unwrap_or(25);
        let path = if let Some(cursor) = params.cursor.as_ref() {
            path_from_cursor(cursor, &self.page_api_path)
        } else {
            let mut query = vec![format!("limit={limit}")];
            if let Some(parent_id) = params.parent_id.as_ref() {
                format!("pages/{parent_id}/children?{}", query.join("&"))
            } else {
                let space = self.resolve_space_by_key(&params.space_key).await?;
                query.push("body-format=view".to_string());
                if let Some(search) = params.search.as_ref() {
                    query.push(format!("title={}", encode_query_value(search)));
                }
                format!("spaces/{}/pages?{}", space.id, query.join("&"))
            }
        };

        let response: ConfluenceListResponse<ConfluencePage> =
            self.get_json_from_api(&self.page_api_path, &path).await?;
        let pagination = map_pagination(&response, Some(limit));
        let mut items = response
            .results
            .iter()
            .map(|page| {
                let mut summary = map_page_summary(&self.instance_url, page);
                if summary.space_key.is_none() {
                    summary.space_key = Some(params.space_key.clone());
                }
                if summary.url.is_none() {
                    summary.url = Some(format!("{}/pages/{}", self.instance_url, page.id));
                }
                summary
            })
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

    async fn get_page_v2(&self, page_id: &str) -> Result<KbPageContent> {
        let path = format!("pages/{page_id}?body-format=storage&include-labels=true");
        let page: ConfluencePage = self.get_json_from_api(&self.page_api_path, &path).await?;
        let mut summary = map_page_summary(&self.instance_url, &page);
        if summary.space_key.is_none()
            && let Some(space_id) = page.space_id.as_deref()
        {
            summary.space_key = self.resolve_space_key_by_id(space_id).await?;
        }
        if summary.url.is_none() {
            summary.url = Some(format!("{}/pages/{}", self.instance_url, page.id));
        }

        let storage_content = page.body.as_ref().and_then(body_value).unwrap_or_default();
        let content = confluence_storage_to_markdown(&storage_content);
        let content_type = "markdown".to_string();
        let ancestors = match self.get_page_ancestor_chain_v2(page_id).await {
            Ok(ancestors) => ancestors,
            Err(error) if should_fallback_to_rest_api(&error) => Vec::new(),
            Err(error) => return Err(error),
        };
        let labels = extract_labels(&page);

        Ok(KbPageContent {
            page: summary,
            content,
            content_type,
            ancestors,
            labels,
        })
    }

    async fn create_page_v2(&self, params: CreatePageParams) -> Result<KbPage> {
        let storage_content =
            normalize_confluence_write_content(&params.content, params.content_type.as_deref())?;
        let space = self.resolve_space_by_key(&params.space_key).await?;
        let payload = ConfluenceV2PagePayload {
            space_id: &space.id,
            status: "current",
            title: &params.title,
            parent_id: params.parent_id.as_deref(),
            body: ConfluenceContentBody {
                value: &storage_content,
                representation: "storage",
            },
        };

        let page: ConfluencePage = self
            .post_json_to_api(&self.page_api_path, "pages", &payload)
            .await?;
        self.add_labels(&page.id, &params.labels).await?;
        let mut summary = map_page_summary(&self.instance_url, &page);
        if summary.space_key.is_none() {
            summary.space_key = Some(params.space_key);
        }
        if summary.url.is_none() {
            summary.url = Some(format!("{}/pages/{}", self.instance_url, page.id));
        }
        Ok(summary)
    }

    async fn update_page_v2(&self, params: UpdatePageParams) -> Result<KbPage> {
        let current_path = if params.labels.is_some() {
            format!(
                "pages/{}?body-format=storage&include-labels=true",
                params.page_id
            )
        } else {
            format!("pages/{}?body-format=storage", params.page_id)
        };
        let current: ConfluencePage = self
            .get_json_from_api(&self.page_api_path, &current_path)
            .await?;
        let current_title = current.title.clone();
        let current_content = current
            .body
            .as_ref()
            .and_then(body_value)
            .unwrap_or_default();
        let current_version = current
            .version
            .as_ref()
            .and_then(|version| version.number)
            .ok_or_else(|| {
                Error::InvalidData(format!(
                    "confluence page {} is missing a version number",
                    params.page_id
                ))
            })?;

        if let Some(expected_version) = params.version
            && expected_version != current_version
        {
            return Err(Error::Api {
                status: 409,
                message: format!(
                    "version conflict for page {}: expected current version {}, found {}",
                    params.page_id, expected_version, current_version
                ),
            });
        }

        let title = params.title.as_deref().unwrap_or(&current_title);
        let content = match params.content.as_deref() {
            Some(updated) => {
                normalize_confluence_write_content(updated, params.content_type.as_deref())?
            }
            None => current_content,
        };
        let space_id = current
            .space_id
            .as_deref()
            .or_else(|| current.space.as_ref().and_then(|space| space.id.as_deref()))
            .ok_or_else(|| {
                Error::InvalidData(format!(
                    "confluence page {} is missing a space id",
                    params.page_id
                ))
            })?;
        let parent_id = params.parent_id.as_deref().or(current.parent_id.as_deref());
        let payload = ConfluenceV2UpdatePayload {
            id: &params.page_id,
            status: "current",
            title,
            space_id,
            parent_id,
            body: ConfluenceContentBody {
                value: &content,
                representation: "storage",
            },
            version: ConfluenceUpdateVersion {
                number: current_version.saturating_add(1),
            },
        };

        let path = format!("pages/{}", params.page_id);
        let page: ConfluencePage = self
            .put_json_to_api(&self.page_api_path, &path, &payload)
            .await?;
        if let Some(labels) = params.labels.as_ref() {
            let current_labels = extract_labels(&current);
            self.sync_labels(&params.page_id, labels, &current_labels)
                .await?;
        }
        let mut summary = map_page_summary(&self.instance_url, &page);
        if summary.space_key.is_none() {
            summary.space_key = self.resolve_space_key_by_id(space_id).await?;
        }
        if summary.url.is_none() {
            summary.url = Some(format!("{}/pages/{}", self.instance_url, page.id));
        }
        Ok(summary)
    }
}

#[async_trait]
impl KnowledgeBaseProvider for ConfluenceClient {
    fn provider_name(&self) -> &'static str {
        "confluence"
    }

    async fn get_spaces(&self) -> Result<ProviderResult<KbSpace>> {
        if uses_v2_api(&self.space_api_path) {
            let path = "space?limit=100&type=global,personal";
            let response: ConfluenceListResponse<ConfluenceSpace> =
                match self.get_json_from_api(&self.space_api_path, path).await {
                    Ok(response) => response,
                    Err(error) if should_fallback_to_rest_api(&error) => {
                        return self.get_spaces_v1().await;
                    }
                    Err(error) => return Err(error),
                };
            let pagination = map_pagination(&response, Some(100));
            let items = response
                .results
                .into_iter()
                .map(|space| map_space(&self.instance_url, space))
                .collect::<Vec<_>>();

            Ok(ProviderResult::new(items).with_pagination(pagination))
        } else {
            self.get_spaces_v1().await
        }
    }

    async fn list_pages(&self, params: ListPagesParams) -> Result<ProviderResult<KbPage>> {
        if uses_v2_api(&self.page_api_path) {
            match self.list_pages_v2(params.clone()).await {
                Ok(result) => Ok(result),
                Err(error) if should_fallback_to_rest_api(&error) => {
                    self.list_pages_v1(params).await
                }
                Err(error) => Err(error),
            }
        } else {
            self.list_pages_v1(params).await
        }
    }

    async fn get_page(&self, page_id: &str) -> Result<KbPageContent> {
        if uses_v2_api(&self.page_api_path) {
            match self.get_page_v2(page_id).await {
                Ok(result) => Ok(result),
                Err(error) if should_fallback_to_rest_api(&error) => {
                    self.get_page_v1(page_id).await
                }
                Err(error) => Err(error),
            }
        } else {
            self.get_page_v1(page_id).await
        }
    }

    async fn create_page(&self, params: CreatePageParams) -> Result<KbPage> {
        if uses_v2_api(&self.page_api_path) {
            match self.create_page_v2(params.clone()).await {
                Ok(result) => Ok(result),
                Err(error) if should_fallback_to_rest_api(&error) => {
                    self.create_page_v1(params).await
                }
                Err(error) => Err(error),
            }
        } else {
            self.create_page_v1(params).await
        }
    }

    async fn update_page(&self, params: UpdatePageParams) -> Result<KbPage> {
        if uses_v2_api(&self.page_api_path) {
            match self.update_page_v2(params.clone()).await {
                Ok(result) => Ok(result),
                Err(error) if should_fallback_to_rest_api(&error) => {
                    self.update_page_v1(params).await
                }
                Err(error) => Err(error),
            }
        } else {
            self.update_page_v1(params).await
        }
    }

    async fn search(&self, params: SearchKbParams) -> Result<ProviderResult<KbPage>> {
        let limit = params.limit.unwrap_or(25);

        let path = if let Some(cursor) = params.cursor.as_ref() {
            path_from_cursor(cursor, &self.api_path)
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
            .map(|page| map_page_summary(&self.instance_url, page))
            .collect::<Vec<_>>();

        Ok(ProviderResult::new(items).with_pagination(pagination))
    }
}

#[cfg(test)]
mod tests {
    use httpmock::Method::{GET, POST, PUT};
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
        let client =
            ConfluenceClient::new("https://wiki.example.com/", ConfluenceAuth::bearer("token"));

        assert_eq!(
            client.rest_api_url("content"),
            "https://wiki.example.com/rest/api/content"
        );
    }

    #[tokio::test]
    async fn new_defaults_to_self_hosted_flavor() {
        let client =
            ConfluenceClient::new("https://wiki.example.com/", ConfluenceAuth::bearer("token"));

        assert!(matches!(client.flavor(), ConfluenceFlavor::SelfHosted));
    }

    #[tokio::test]
    async fn rest_api_url_honors_v2_api_version() {
        let client =
            ConfluenceClient::new("https://wiki.example.com/", ConfluenceAuth::bearer("token"))
                .with_api_version(Some("v2"));

        assert_eq!(
            client.space_api_url("space"),
            "https://wiki.example.com/api/v2/space"
        );
    }

    #[tokio::test]
    async fn with_instance_url_keeps_browse_links_on_real_host_in_proxy_mode() {
        // The proxy use case: API requests go through the proxy host,
        // but `_links.webui` / `/pages/<id>` URLs returned to callers
        // must point at the real Confluence so they remain clickable.
        let api = MockServer::start();
        let _mock = api.mock(|when, then| {
            when.method(GET).path("/rest/api/space");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"results":[{"id":"42","key":"OPS","name":"Ops","type":"global","_links":{"webui":"/display/OPS"}}],"start":0,"limit":100,"size":1,"_links":{}}"#,
                );
        });

        let client = ConfluenceClient::new(api.base_url(), ConfluenceAuth::bearer("t"))
            .with_proxy(HashMap::new())
            .with_instance_url("https://wiki.example.com");

        assert_eq!(client.instance_url(), "https://wiki.example.com");
        assert_ne!(client.base_url(), client.instance_url());

        let resp = client.get_spaces().await.unwrap();
        let url = resp.items[0].url.as_deref().unwrap_or_default();
        assert!(
            url.starts_with("https://wiki.example.com"),
            "browse link must point at the real instance, not the proxy. got: {url}"
        );
    }

    #[tokio::test]
    async fn with_instance_url_defaults_to_base_url_when_unset() {
        let client =
            ConfluenceClient::new("https://wiki.example.com/", ConfluenceAuth::bearer("t"));
        assert_eq!(client.base_url(), client.instance_url());
        assert_eq!(client.instance_url(), "https://wiki.example.com");
    }

    #[tokio::test]
    async fn get_spaces_accepts_integer_id_from_self_hosted_server() {
        // Cloud Confluence v2 returns `"id": "<string>"`; on-prem Server / DC
        // `/rest/api/space` returns it as a JSON integer. The client must
        // accept both flavours without breaking decoding.
        let server = MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method(GET).path("/rest/api/space");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{"results":[{"id":190119946,"key":"1LS","name":"1 line support","type":"global","_links":{"webui":"/display/1LS"}}],"start":0,"limit":100,"size":1,"_links":{}}"#,
                );
        });

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"));

        let response = client.get_spaces().await.unwrap();
        assert_eq!(response.items.len(), 1);
        assert_eq!(response.items[0].id, "190119946");
        assert_eq!(response.items[0].key, "1LS");
    }

    #[tokio::test]
    async fn send_json_decode_failure_surfaces_status_and_body_preview() {
        // When a self-hosted reverse proxy or auth gateway rewrites a 200
        // response to HTML, the decode failure has to surface enough
        // diagnostic context (status, content-type, body preview) so the
        // caller can tell it's a misconfig rather than a tool routing
        // problem.
        let server = MockServer::start();
        let _mock = server.mock(|when, then| {
            when.method(GET).path("/rest/api/space");
            then.status(200)
                .header("content-type", "text/html")
                .body("<html><body>Login required</body></html>");
        });

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"));

        let err = client.get_spaces().await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("JSON decode failed"), "got: {msg}");
        assert!(msg.contains("status=200"), "got: {msg}");
        assert!(msg.contains("text/html"), "got: {msg}");
        assert!(msg.contains("Login required"), "got: {msg}");
    }

    #[tokio::test]
    async fn get_spaces_falls_back_to_rest_api_when_v2_is_unavailable() {
        let server = MockServer::start();
        let v2_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v2/space")
                .query_param("limit", "100")
                .query_param("type", "global,personal");
            then.status(404);
        });
        let v1_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/space")
                .query_param("limit", "100")
                .query_param("type", "global,personal");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"results":[],"start":0,"limit":100,"size":0,"_links":{}}"#);
        });

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"))
                .with_api_version(Some("v2"));

        let response = client.get_spaces().await.unwrap();

        assert!(response.items.is_empty());
        v2_mock.assert();
        v1_mock.assert();
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

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"));
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
            ConfluenceAuth::basic("user@example.com", "password"),
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

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"))
                .with_proxy(headers);
        let response: EchoResponse = client.get_json("content").await.unwrap();

        mock.assert();
        assert!(response.ok);
    }

    #[tokio::test]
    async fn get_spaces_maps_confluence_spaces() {
        let server = MockServer::start();
        // `_links.base` echoes the same origin as the client's base URL —
        // the realistic Cloud / Server case where Confluence advertises
        // its own host back to us. In that situation the hint is
        // honoured verbatim (it may carry a path prefix like `/wiki`).
        let server_origin = server.base_url();
        let body = format!(
            r#"{{
                "results": [
                    {{
                        "id": "123",
                        "key": "ENG",
                        "name": "Engineering",
                        "type": "global",
                        "status": "current",
                        "description": {{ "plain": {{ "value": "Team docs" }} }},
                        "_links": {{ "base": "{server_origin}", "webui": "/spaces/ENG/overview" }}
                    }}
                ],
                "start": 0,
                "limit": 100,
                "size": 1,
                "totalSize": 1,
                "_links": {{}}
            }}"#,
        );
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/space")
                .query_param("limit", "100")
                .query_param("type", "global,personal");
            then.status(200)
                .header("content-type", "application/json")
                .body(&body);
        });

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"));
        let result = client.get_spaces().await.unwrap();

        mock.assert();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].key, "ENG");
        assert_eq!(result.items[0].name, "Engineering");
        assert_eq!(result.items[0].description.as_deref(), Some("Team docs"));
        assert_eq!(
            result.items[0].url.as_deref(),
            Some(format!("{server_origin}/spaces/ENG/overview").as_str())
        );
        assert_eq!(result.pagination.unwrap().total, Some(1));
    }

    #[tokio::test]
    async fn map_link_ignores_cross_host_links_base_in_proxy_mode() {
        // Regression for Copilot review on PR #286: when Confluence is
        // fronted by a reverse proxy that rewrites `_links.base` to the
        // proxy host (or to an internal hostname unreachable from the
        // client), the browse URL we return must still resolve against
        // the caller-supplied `instance_url`, not the misleading hint.
        let server = MockServer::start();
        let _mock = server.mock(|when, then| {
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
                                "key": "OPS",
                                "name": "Ops",
                                "type": "global",
                                "_links": {
                                    "base": "https://internal-proxy.local",
                                    "webui": "/display/OPS"
                                }
                            }
                        ],
                        "start": 0,
                        "limit": 100,
                        "size": 1,
                        "_links": {}
                    }"#,
                );
        });

        let client = ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("t"))
            .with_instance_url("https://wiki.example.com");
        let result = client.get_spaces().await.unwrap();

        let url = result.items[0].url.as_deref().unwrap_or_default();
        assert!(
            url.starts_with("https://wiki.example.com"),
            "cross-host `_links.base` must be ignored in favour of \
             `instance_url`. got: {url}"
        );
    }

    #[tokio::test]
    async fn list_pages_falls_back_to_rest_content_api_when_v2_pages_are_unavailable() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET)
                .path("/api/v2/space")
                .query_param("limit", "100")
                .query_param("type", "global,personal");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [
                            { "id": "123", "key": "ENG", "name": "Engineering" }
                        ],
                        "_links": {}
                    }"#,
                );
        });
        server.mock(|when, then| {
            when.method(GET)
                .path("/api/v2/spaces/123/pages")
                .query_param("limit", "25")
                .query_param("body-format", "view");
            then.status(404);
        });
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content")
                .query_param("spaceKey", "ENG")
                .query_param("type", "page")
                .query_param("limit", "25")
                .query_param("start", "0")
                .query_param(
                    "expand",
                    "space,version,history.lastUpdated,body.view,ancestors",
                );
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"results":[],"start":0,"limit":25,"size":0,"_links":{}}"#);
        });

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"))
                .with_api_version(Some("v2"));
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
        assert!(result.items.is_empty());
    }

    #[tokio::test]
    async fn list_pages_uses_v2_pages_when_preferred() {
        let server = MockServer::start();
        let pages_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v2/pages/10/children")
                .query_param("limit", "25");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [
                            {
                                "id": "42",
                                "title": "ADR-001",
                                "spaceId": "123",
                                "parentId": "10",
                                "_links": { "next": "/api/v2/pages/10/children?cursor=abc" }
                            }
                        ],
                        "limit": 25,
                        "size": 1,
                        "_links": { "next": "/api/v2/pages/10/children?cursor=abc" }
                    }"#,
                );
        });

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"))
                .with_api_version(Some("v2"));
        let result = client
            .list_pages(ListPagesParams {
                space_key: "ENG".into(),
                limit: Some(25),
                offset: Some(0),
                cursor: None,
                search: None,
                parent_id: Some("10".into()),
            })
            .await
            .unwrap();

        pages_mock.assert();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "42");
        assert_eq!(result.items[0].space_key.as_deref(), Some("ENG"));
        assert_eq!(
            result
                .pagination
                .and_then(|pagination| pagination.next_cursor),
            Some("/api/v2/pages/10/children?cursor=abc".into())
        );
    }

    #[tokio::test]
    async fn list_pages_uses_v1_child_endpoint_when_parent_filter_is_set() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content/10/child/page")
                .query_param("limit", "25")
                .query_param("start", "0")
                .query_param("expand", "space,version,history.lastUpdated,body.view");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [
                            {
                                "id": "42",
                                "title": "ADR-001",
                                "space": { "key": "ENG" },
                                "version": { "number": 7 },
                                "body": {
                                    "view": { "value": "<p>Architecture decision record</p>" }
                                },
                                "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=42" }
                            }
                        ],
                        "start": 0,
                        "limit": 25,
                        "size": 1,
                        "_links": {}
                    }"#,
                );
        });

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"));
        let result = client
            .list_pages(ListPagesParams {
                space_key: "ENG".into(),
                limit: Some(25),
                offset: Some(0),
                cursor: None,
                search: None,
                parent_id: Some("10".into()),
            })
            .await
            .unwrap();

        mock.assert();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "42");
        assert_eq!(result.items[0].space_key.as_deref(), Some("ENG"));
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

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"));
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
    async fn list_pages_uses_cursor_path_for_followup_requests() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content")
                .query_param("limit", "25")
                .query_param("start", "25");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [
                            {
                                "id": "77",
                                "title": "Next Page",
                                "space": { "key": "ENG" },
                                "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=77" }
                            }
                        ],
                        "start": 25,
                        "limit": 25,
                        "size": 1,
                        "totalSize": 26,
                        "_links": {}
                    }"#,
                );
        });

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"));
        let result = client
            .list_pages(ListPagesParams {
                space_key: "ENG".into(),
                limit: Some(25),
                offset: Some(0),
                cursor: Some("/rest/api/content?limit=25&start=25".into()),
                search: None,
                parent_id: None,
            })
            .await
            .unwrap();

        mock.assert();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].id, "77");
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

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"));
        let page = client.get_page("42").await.unwrap();

        mock.assert();
        assert_eq!(page.page.id, "42");
        assert_eq!(page.page.title, "ADR-001");
        assert_eq!(page.page.version, Some(7));
        assert_eq!(page.content_type, "markdown");
        assert_eq!(page.content, "Hello **world**");
        assert_eq!(page.labels, vec!["adr", "architecture"]);
        assert_eq!(page.ancestors.len(), 1);
        assert_eq!(page.ancestors[0].id, "10");
        assert_eq!(page.ancestors[0].title, "Architecture Decisions");
    }

    #[tokio::test]
    async fn get_page_uses_v2_page_and_ancestors_when_preferred() {
        let server = MockServer::start();
        let space_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v2/space")
                .query_param("limit", "100")
                .query_param("type", "global,personal");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [
                            { "id": "123", "key": "ENG", "name": "Engineering" }
                        ],
                        "_links": {}
                    }"#,
                );
        });
        let page_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v2/pages/42")
                .query_param("body-format", "storage")
                .query_param("include-labels", "true");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "42",
                        "title": "ADR-001",
                        "spaceId": "123",
                        "parentId": "10",
                        "version": {
                            "number": 7,
                            "createdAt": "2026-04-26T10:00:00.000Z"
                        },
                        "body": {
                            "representation": "storage",
                            "value": "<p>Hello <strong>world</strong></p>"
                        },
                        "labels": {
                            "results": [
                                { "label": "adr" },
                                { "label": "architecture" }
                            ]
                        }
                    }"#,
                );
        });
        let ancestors_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v2/pages/42/ancestors")
                .query_param("limit", "100");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{ "results": [ { "id": "10", "type": "page" } ], "_links": {} }"#);
        });
        let ancestor_page_mock = server.mock(|when, then| {
            when.method(GET).path("/api/v2/pages/10");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "10",
                        "title": "Architecture Decisions",
                        "spaceId": "123"
                    }"#,
                );
        });

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"))
                .with_api_version(Some("v2"));
        let page = client.get_page("42").await.unwrap();

        space_mock.assert();
        page_mock.assert();
        ancestors_mock.assert();
        ancestor_page_mock.assert();
        assert_eq!(page.page.id, "42");
        assert_eq!(page.page.space_key.as_deref(), Some("ENG"));
        assert_eq!(page.page.version, Some(7));
        assert_eq!(page.content, "Hello **world**");
        assert_eq!(page.labels, vec!["adr", "architecture"]);
        assert_eq!(page.ancestors.len(), 1);
        assert_eq!(page.ancestors[0].title, "Architecture Decisions");
    }

    #[tokio::test]
    async fn get_page_v2_propagates_non_fallback_ancestor_errors() {
        let server = MockServer::start();
        let space_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v2/space")
                .query_param("limit", "100")
                .query_param("type", "global,personal");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [
                            { "id": "123", "key": "ENG", "name": "Engineering" }
                        ],
                        "_links": {}
                    }"#,
                );
        });
        let page_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v2/pages/42")
                .query_param("body-format", "storage")
                .query_param("include-labels", "true");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "42",
                        "title": "ADR-001",
                        "spaceId": "123",
                        "version": { "number": 7 },
                        "body": {
                            "representation": "storage",
                            "value": "<p>Hello</p>"
                        }
                    }"#,
                );
        });
        let ancestors_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v2/pages/42/ancestors")
                .query_param("limit", "100");
            then.status(401).body("unauthorized");
        });

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"))
                .with_api_version(Some("v2"));
        let error = client.get_page("42").await.unwrap_err();

        space_mock.assert();
        page_mock.assert();
        ancestors_mock.assert();
        assert!(matches!(error, Error::Unauthorized(_)));
    }

    #[tokio::test]
    async fn create_page_accepts_markdown_and_posts_storage_payload() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/rest/api/content")
                .header("authorization", "Bearer secret-token")
                .header("content-type", "application/json")
                .json_body_obj(&serde_json::json!({
                    "type": "page",
                    "title": "ADR-002",
                    "space": { "key": "ENG" },
                    "body": {
                        "storage": {
                            "value": "<h1>Decision</h1><p>Hello <strong>world</strong></p>",
                            "representation": "storage"
                        }
                    },
                    "ancestors": [{ "id": "10" }]
                }));
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "43",
                        "title": "ADR-002",
                        "space": { "key": "ENG" },
                        "version": {
                            "number": 1,
                            "when": "2026-04-26T10:00:00.000Z",
                            "by": { "displayName": "Alice" }
                        },
                        "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=43" }
                    }"#,
                );
        });

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"));
        let page = client
            .create_page(CreatePageParams {
                space_key: "ENG".into(),
                title: "ADR-002".into(),
                content: "# Decision\n\nHello **world**".into(),
                content_type: Some("markdown".into()),
                parent_id: Some("10".into()),
                labels: vec![],
            })
            .await
            .unwrap();

        mock.assert();
        assert_eq!(page.id, "43");
        assert_eq!(page.title, "ADR-002");
        assert_eq!(page.space_key.as_deref(), Some("ENG"));
        assert_eq!(page.version, Some(1));
    }

    #[tokio::test]
    async fn create_page_posts_labels_after_create() {
        let server = MockServer::start();
        let create_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/rest/api/content")
                .header("authorization", "Bearer secret-token")
                .header("content-type", "application/json")
                .json_body_obj(&serde_json::json!({
                    "type": "page",
                    "title": "ADR-002",
                    "space": { "key": "ENG" },
                    "body": {
                        "storage": {
                            "value": "<p>Hello</p>",
                            "representation": "storage"
                        }
                    }
                }));
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "43",
                        "title": "ADR-002",
                        "space": { "key": "ENG" },
                        "version": { "number": 1 }
                    }"#,
                );
        });
        let labels_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/rest/api/content/43/label")
                .header("authorization", "Bearer secret-token")
                .header("content-type", "application/json")
                .json_body_obj(&serde_json::json!([
                    { "prefix": "global", "name": "adr" },
                    { "prefix": "global", "name": "architecture" }
                ]));
            then.status(200)
                .header("content-type", "application/json")
                .body("[]");
        });

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"));
        let page = client
            .create_page(CreatePageParams {
                space_key: "ENG".into(),
                title: "ADR-002".into(),
                content: "<p>Hello</p>".into(),
                content_type: Some("storage".into()),
                parent_id: None,
                labels: vec!["adr".into(), "architecture".into()],
            })
            .await
            .unwrap();

        create_mock.assert();
        labels_mock.assert();
        assert_eq!(page.id, "43");
    }

    #[tokio::test]
    async fn create_page_uses_v2_pages_when_preferred() {
        let server = MockServer::start();
        let space_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v2/space")
                .query_param("limit", "100")
                .query_param("type", "global,personal");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [
                            { "id": "123", "key": "ENG", "name": "Engineering" }
                        ],
                        "_links": {}
                    }"#,
                );
        });
        let create_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v2/pages")
                .header("authorization", "Bearer secret-token")
                .header("content-type", "application/json")
                .json_body_obj(&serde_json::json!({
                    "spaceId": "123",
                    "status": "current",
                    "title": "ADR-002",
                    "parentId": "10",
                    "body": {
                        "value": "<h1>Decision</h1><p>Hello <strong>world</strong></p>",
                        "representation": "storage"
                    }
                }));
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "43",
                        "title": "ADR-002",
                        "spaceId": "123",
                        "version": { "number": 1 }
                    }"#,
                );
        });

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"))
                .with_api_version(Some("v2"));
        let page = client
            .create_page(CreatePageParams {
                space_key: "ENG".into(),
                title: "ADR-002".into(),
                content: "# Decision\n\nHello **world**".into(),
                content_type: Some("markdown".into()),
                parent_id: Some("10".into()),
                labels: vec![],
            })
            .await
            .unwrap();

        space_mock.assert();
        create_mock.assert();
        assert_eq!(page.id, "43");
        assert_eq!(page.space_key.as_deref(), Some("ENG"));
        assert_eq!(page.version, Some(1));
    }

    #[tokio::test]
    async fn update_page_accepts_markdown_and_puts_incremented_version() {
        let server = MockServer::start();
        let get_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content/42")
                .query_param("expand", "space,version,body.storage,ancestors");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "42",
                        "title": "ADR-001",
                        "space": { "key": "ENG" },
                        "version": { "number": 7 },
                        "body": {
                            "storage": {
                                "value": "<p>Old</p>",
                                "representation": "storage"
                            }
                        },
                        "ancestors": [
                            { "id": "10", "title": "Architecture", "_links": {} }
                        ],
                        "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=42" }
                    }"#,
                );
        });
        let put_mock = server.mock(|when, then| {
            when.method(PUT)
                .path("/rest/api/content/42")
                .header("authorization", "Bearer secret-token")
                .header("content-type", "application/json")
                .json_body_obj(&serde_json::json!({
                    "id": "42",
                    "type": "page",
                    "title": "ADR-001 Revised",
                    "version": { "number": 8 },
                    "body": {
                        "storage": {
                            "value": "<p>New <strong>decision</strong></p>",
                            "representation": "storage"
                        }
                    },
                    "ancestors": [{ "id": "11" }]
                }));
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "42",
                        "title": "ADR-001 Revised",
                        "space": { "key": "ENG" },
                        "version": {
                            "number": 8,
                            "when": "2026-04-26T11:00:00.000Z",
                            "by": { "displayName": "Bob" }
                        },
                        "_links": { "base": "https://wiki.example.com", "webui": "/pages/viewpage.action?pageId=42" }
                    }"#,
                );
        });

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"));
        let page = client
            .update_page(UpdatePageParams {
                page_id: "42".into(),
                title: Some("ADR-001 Revised".into()),
                content: Some("New **decision**".into()),
                content_type: Some("markdown".into()),
                version: Some(7),
                labels: None,
                parent_id: Some("11".into()),
            })
            .await
            .unwrap();

        get_mock.assert();
        put_mock.assert();
        assert_eq!(page.id, "42");
        assert_eq!(page.title, "ADR-001 Revised");
        assert_eq!(page.version, Some(8));
        assert_eq!(page.author.as_deref(), Some("Bob"));
    }

    #[tokio::test]
    async fn update_page_uses_v2_pages_when_preferred() {
        let server = MockServer::start();
        let get_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v2/pages/42")
                .query_param("body-format", "storage");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "42",
                        "title": "ADR-001",
                        "spaceId": "123",
                        "parentId": "10",
                        "version": { "number": 7 },
                        "body": {
                            "representation": "storage",
                            "value": "<p>Old</p>"
                        }
                    }"#,
                );
        });
        let put_mock = server.mock(|when, then| {
            when.method(PUT)
                .path("/api/v2/pages/42")
                .header("authorization", "Bearer secret-token")
                .header("content-type", "application/json")
                .json_body_obj(&serde_json::json!({
                    "id": "42",
                    "status": "current",
                    "title": "ADR-001 Revised",
                    "spaceId": "123",
                    "parentId": "11",
                    "body": {
                        "value": "<p>New <strong>decision</strong></p>",
                        "representation": "storage"
                    },
                    "version": { "number": 8 }
                }));
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "42",
                        "title": "ADR-001 Revised",
                        "spaceId": "123",
                        "version": { "number": 8, "createdAt": "2026-04-26T11:00:00.000Z" }
                    }"#,
                );
        });
        let space_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/api/v2/space")
                .query_param("limit", "100")
                .query_param("type", "global,personal");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "results": [
                            { "id": "123", "key": "ENG", "name": "Engineering" }
                        ],
                        "_links": {}
                    }"#,
                );
        });

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"))
                .with_api_version(Some("v2"));
        let page = client
            .update_page(UpdatePageParams {
                page_id: "42".into(),
                title: Some("ADR-001 Revised".into()),
                content: Some("New **decision**".into()),
                content_type: Some("markdown".into()),
                version: Some(7),
                labels: None,
                parent_id: Some("11".into()),
            })
            .await
            .unwrap();

        get_mock.assert();
        put_mock.assert();
        space_mock.assert();
        assert_eq!(page.id, "42");
        assert_eq!(page.title, "ADR-001 Revised");
        assert_eq!(page.space_key.as_deref(), Some("ENG"));
        assert_eq!(page.version, Some(8));
    }

    #[tokio::test]
    async fn update_page_returns_conflict_when_expected_version_is_stale() {
        let server = MockServer::start();
        let get_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content/42")
                .query_param("expand", "space,version,body.storage,ancestors");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "42",
                        "title": "ADR-001",
                        "space": { "key": "ENG" },
                        "version": { "number": 7 },
                        "body": {
                            "storage": {
                                "value": "<p>Old</p>",
                                "representation": "storage"
                            }
                        },
                        "ancestors": [],
                        "_links": {}
                    }"#,
                );
        });

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"));
        let error = client
            .update_page(UpdatePageParams {
                page_id: "42".into(),
                title: Some("ADR-001 Revised".into()),
                content: Some("<p>New</p>".into()),
                content_type: Some("storage".into()),
                version: Some(6),
                labels: None,
                parent_id: None,
            })
            .await
            .unwrap_err();

        get_mock.assert();
        match error {
            Error::Api { status, message } => {
                assert_eq!(status, 409);
                assert!(message.contains("expected current version 6"));
                assert!(message.contains("found 7"));
            }
            other => panic!("expected conflict error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_page_replaces_labels() {
        let server = MockServer::start();
        let get_mock = server.mock(|when, then| {
            when.method(GET).path("/rest/api/content/42").query_param(
                "expand",
                "space,version,body.storage,ancestors,metadata.labels",
            );
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "42",
                        "title": "ADR-001",
                        "space": { "key": "ENG" },
                        "version": { "number": 7 },
                        "body": {
                            "storage": {
                                "value": "<p>Old</p>",
                                "representation": "storage"
                            }
                        },
                        "metadata": {
                            "labels": {
                                "results": [
                                    { "name": "adr" },
                                    { "name": "obsolete" }
                                ]
                            }
                        },
                        "ancestors": [],
                        "_links": {}
                    }"#,
                );
        });
        let put_mock = server.mock(|when, then| {
            when.method(PUT)
                .path("/rest/api/content/42")
                .header("authorization", "Bearer secret-token")
                .header("content-type", "application/json");
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                        "id": "42",
                        "title": "ADR-001 Revised",
                        "space": { "key": "ENG" },
                        "version": { "number": 8 }
                    }"#,
                );
        });
        let delete_mock = server.mock(|when, then| {
            when.method(httpmock::Method::DELETE)
                .path("/rest/api/content/42/label")
                .query_param("name", "obsolete")
                .header("authorization", "Bearer secret-token");
            then.status(204);
        });
        let add_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/rest/api/content/42/label")
                .header("authorization", "Bearer secret-token")
                .header("content-type", "application/json")
                .json_body_obj(&serde_json::json!([
                    { "prefix": "global", "name": "architecture" }
                ]));
            then.status(200)
                .header("content-type", "application/json")
                .body("[]");
        });

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"));
        let page = client
            .update_page(UpdatePageParams {
                page_id: "42".into(),
                title: Some("ADR-001 Revised".into()),
                content: Some("<p>New</p>".into()),
                content_type: Some("storage".into()),
                version: Some(7),
                labels: Some(vec!["adr".into(), "architecture".into()]),
                parent_id: None,
            })
            .await
            .unwrap();

        get_mock.assert();
        put_mock.assert();
        delete_mock.assert();
        add_mock.assert();
        assert_eq!(page.version, Some(8));
    }

    #[test]
    fn storage_and_markdown_converters_cover_basic_formatting() {
        let markdown = confluence_storage_to_markdown(
            r#"<h2>ADR</h2><p>Hello <strong>world</strong> and <a href="https://example.com">link</a></p><ul><li>One</li><li>Two</li></ul>"#,
        );
        assert_eq!(
            markdown,
            "## ADR\n\nHello **world** and [link](https://example.com)\n\n- One\n- Two"
        );

        let storage = markdown_to_confluence_storage(
            "## ADR\n\nHello **world** and [link](https://example.com)\n\n- One\n- Two",
        );
        assert_eq!(
            storage,
            "<h2>ADR</h2><p>Hello <strong>world</strong> and <a href=\"https://example.com\">link</a></p><ul><li>One</li><li>Two</li></ul>"
        );
    }

    #[test]
    fn markdown_code_blocks_escape_cdata_terminators() {
        let storage = markdown_to_confluence_storage("```xml\nbefore ]]> after\n```");
        assert!(storage.contains("<![CDATA[before ]]]]><![CDATA[> after"));
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

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"));
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

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"));
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
        let next_cursor = first
            .pagination
            .as_ref()
            .and_then(|p| p.next_cursor.clone());

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

    #[tokio::test]
    async fn search_percent_encodes_reserved_query_characters() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(GET)
                .path("/rest/api/content/search")
                .query_param("cql", "type = page AND text ~ \"R&D?x=y+z\"")
                .query_param("limit", "5")
                .query_param("expand", "space,version,history.lastUpdated,body.view");
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{"results":[],"start":0,"limit":5,"size":0,"_links":{}}"#);
        });

        let client =
            ConfluenceClient::new(server.base_url(), ConfluenceAuth::bearer("secret-token"));
        let result = client
            .search(SearchKbParams {
                query: "R&D?x=y+z".into(),
                space_key: None,
                cursor: None,
                limit: Some(5),
                raw_query: false,
            })
            .await
            .unwrap();

        mock.assert();
        assert!(result.items.is_empty());
    }
}
