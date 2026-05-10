//! Jira API client implementation.
//!
//! Supports both Jira Cloud (API v3) and Jira Self-Hosted/Data Center (API v2).
//! Flavor is auto-detected from the URL: `*.atlassian.net` → Cloud, otherwise → SelfHosted.

/// Find the largest byte index <= `max_bytes` that is on a UTF-8 char boundary.
fn safe_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut i = max_bytes;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

use async_trait::async_trait;
use devboy_core::{
    AddStructureRowsInput, AssetCapabilities, AssetMeta, Comment, ContextCapabilities,
    CreateIssueInput, CreateStructureInput, Error, ForestModifyResult, GetForestOptions,
    GetStructureValuesInput, GetUsersOptions, Issue, IssueFilter, IssueLink, IssueProvider,
    IssueRelations, IssueStatus, ListProjectVersionsParams, MergeRequestProvider,
    MoveStructureRowsInput, PipelineProvider, ProjectVersion, Provider, ProviderResult, Result,
    SaveStructureViewInput, Structure, StructureColumnValue, StructureForest, StructureNode,
    StructureRowValues, StructureValues, StructureView, StructureViewColumn, UpdateIssueInput,
    UpsertProjectVersionInput, User,
};
use secrecy::{ExposeSecret, SecretString};
use tracing::{debug, warn};

use crate::types::{
    AddCommentPayload, CreateIssueFields, CreateIssueLinkPayload, CreateIssuePayload,
    CreateIssueResponse, CreateVersionPayload, IssueKeyRef, IssueLinkTypeName, IssueType,
    JiraAttachment, JiraCloudSearchResponse, JiraComment, JiraCommentsResponse,
    JiraForestModifyResponse, JiraForestResponse, JiraIssue, JiraIssueTypeStatuses, JiraPriority,
    JiraProjectStatus, JiraSearchResponse, JiraStatus, JiraStructure, JiraStructureListResponse,
    JiraStructureValuesResponse, JiraStructureView, JiraStructureViewListResponse, JiraTransition,
    JiraTransitionsResponse, JiraUser, JiraVersionDto, PriorityName, ProjectKey, TransitionId,
    TransitionPayload, UpdateIssueFields, UpdateIssuePayload, UpdateVersionPayload,
};

/// Jira deployment flavor.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JiraFlavor {
    /// Jira Cloud — API v3, ADF format, accountId-based users
    Cloud,
    /// Jira Self-Hosted / Data Center — API v2, plain text, username-based users
    SelfHosted,
}

pub struct JiraClient {
    base_url: String,
    /// Original Jira instance URL for generating browse links.
    /// When proxy is used, base_url points to proxy but instance_url points to real Jira.
    instance_url: String,
    project_key: String,
    email: String,
    token: SecretString,
    flavor: JiraFlavor,
    proxy_headers: Option<std::collections::HashMap<String, String>>,
    client: reqwest::Client,
}

impl JiraClient {
    /// Create a new Jira client. Flavor is auto-detected from the URL.
    pub fn new(
        url: impl Into<String>,
        project_key: impl Into<String>,
        email: impl Into<String>,
        token: SecretString,
    ) -> Self {
        let url = url.into();
        let flavor = detect_flavor(&url);
        let instance = url.trim_end_matches('/').to_string();
        let api_base = build_api_base(&url, flavor);
        Self {
            base_url: api_base,
            instance_url: instance,
            project_key: project_key.into(),
            email: email.into(),
            token,
            flavor,
            proxy_headers: None,
            client: reqwest::Client::builder()
                .user_agent("devboy-tools")
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Configure proxy mode with extra headers added to every request.
    /// When proxy is active, the provider's own auth headers are suppressed —
    /// the proxy handles authentication.
    /// Note: `instance_url` is preserved for generating browse links.
    pub fn with_proxy(mut self, headers: std::collections::HashMap<String, String>) -> Self {
        self.proxy_headers = Some(headers);
        self
    }

    /// Override the instance URL used for generating browse links.
    /// Useful when proxy URL differs from real Jira instance URL.
    pub fn with_instance_url(mut self, url: impl Into<String>) -> Self {
        self.instance_url = url.into().trim_end_matches('/').to_string();
        self
    }

    /// Override auto-detected flavor.
    /// Use when the URL doesn't reflect the actual Jira deployment
    /// (e.g. proxy URL instead of real Jira URL).
    pub fn with_flavor(mut self, flavor: JiraFlavor) -> Self {
        if self.flavor != flavor {
            // Rebuild API base URL with new flavor
            let instance_url = instance_url_from_base(&self.base_url);
            self.base_url = build_api_base(&instance_url, flavor);
            self.flavor = flavor;
        }
        self
    }

    /// Create a new Jira client with explicit base URL (for testing with httpmock).
    /// The base URL is used as-is (no `/rest/api/N` suffix appended).
    pub fn with_base_url(
        base_url: impl Into<String>,
        project_key: impl Into<String>,
        email: impl Into<String>,
        token: SecretString,
        flavor: bool, // true = Cloud, false = SelfHosted
    ) -> Self {
        let url = base_url.into().trim_end_matches('/').to_string();
        Self {
            instance_url: url.clone(),
            base_url: url,
            project_key: project_key.into(),
            email: email.into(),
            token,
            flavor: if flavor {
                JiraFlavor::Cloud
            } else {
                JiraFlavor::SelfHosted
            },
            proxy_headers: None,
            client: reqwest::Client::builder()
                .user_agent("devboy-tools")
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Build request with auth headers and JSON content type.
    ///
    /// When proxy is configured, provider's own auth is suppressed and
    /// proxy headers are added instead. The proxy handles authentication.
    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.request_raw(method, url)
            .header("Content-Type", "application/json")
    }

    /// Build request with auth headers but **no** Content-Type header.
    ///
    /// Use this for multipart uploads where reqwest must set its own
    /// `Content-Type: multipart/form-data; boundary=...` header.
    fn request_raw(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let mut builder = self.client.request(method, url);

        if let Some(headers) = &self.proxy_headers {
            for (key, value) in headers {
                builder = builder.header(key.as_str(), value.as_str());
            }
        } else {
            builder = match self.flavor {
                JiraFlavor::Cloud => {
                    let token_value = self.token.expose_secret();
                    let credentials = base64_encode(&format!("{}:{}", self.email, token_value));
                    builder.header("Authorization", format!("Basic {}", credentials))
                }
                JiraFlavor::SelfHosted => {
                    let token_value = self.token.expose_secret();
                    if token_value.contains(':') {
                        let credentials = base64_encode(token_value);
                        builder.header("Authorization", format!("Basic {}", credentials))
                    } else {
                        builder.header("Authorization", format!("Bearer {}", token_value))
                    }
                }
            };
        }
        builder
    }

    /// Make an authenticated GET request.
    async fn get<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        debug!(url = url, "Jira GET request");

        let response = self
            .request(reqwest::Method::GET, url)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        self.handle_response(response).await
    }

    /// Make an authenticated POST request.
    async fn post<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T> {
        debug!(url = url, "Jira POST request");

        let response = self
            .request(reqwest::Method::POST, url)
            .json(body)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        self.handle_response(response).await
    }

    /// Make an authenticated POST request that returns no body (201/204).
    async fn post_no_content<B: serde::Serialize>(&self, url: &str, body: &B) -> Result<()> {
        debug!(url = url, "Jira POST (no content) request");

        let response = self
            .request(reqwest::Method::POST, url)
            .json(body)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let status_code = status.as_u16();
            let message = response.text().await.unwrap_or_default();
            warn!(
                status = status_code,
                message = message,
                "Jira API error response"
            );
            return Err(Error::from_status(status_code, message));
        }

        Ok(())
    }

    /// Make an authenticated PUT request that parses a JSON response body.
    ///
    /// Jira's `PUT /version/{id}` (issue #238) — unlike `PUT /issue/{key}` —
    /// returns the updated entity, so we need a typed variant alongside the
    /// `put` helper that discards the body.
    async fn put_with_response<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T> {
        debug!(url = url, "Jira PUT request (typed response)");

        let response = self
            .request(reqwest::Method::PUT, url)
            .json(body)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        self.handle_response(response).await
    }

    /// Make an authenticated PUT request (Jira PUT returns 204 No Content).
    async fn put<B: serde::Serialize>(&self, url: &str, body: &B) -> Result<()> {
        debug!(url = url, "Jira PUT request");

        let response = self
            .request(reqwest::Method::PUT, url)
            .json(body)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let status_code = status.as_u16();
            let message = response.text().await.unwrap_or_default();
            warn!(
                status = status_code,
                message = message,
                "Jira API error response"
            );
            return Err(Error::from_status(status_code, message));
        }

        Ok(())
    }

    /// Handle response and map errors.
    async fn handle_response<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> Result<T> {
        let status = response.status();

        if !status.is_success() {
            let status_code = status.as_u16();
            let message = response.text().await.unwrap_or_default();
            warn!(
                status = status_code,
                message = message,
                "Jira API error response"
            );
            return Err(Error::from_status(status_code, message));
        }

        let body = response
            .text()
            .await
            .map_err(|e| Error::InvalidData(format!("Failed to read response body: {}", e)))?;

        serde_json::from_str::<T>(&body).map_err(|e| {
            // Use safe_char_boundary to avoid panic on multi-byte UTF-8
            let preview = if body.len() > 500 {
                let end = safe_char_boundary(&body, 500);
                format!("{}...(truncated, total {} bytes)", &body[..end], body.len())
            } else {
                body.clone()
            };
            warn!(
                error = %e,
                body_preview = preview,
                "Failed to parse Jira response"
            );
            let preview = if body.len() > 300 {
                let end = safe_char_boundary(&body, 300);
                format!("{}...(truncated)", &body[..end])
            } else {
                body.clone()
            };
            Error::InvalidData(format!(
                "Failed to parse response: {}. Response preview: {}",
                e, preview
            ))
        })
    }

    /// Transition an issue to a new status by finding matching transition.
    ///
    /// Matching order:
    /// 1. Exact match on transition `to.name` (case-insensitive)
    /// 2. Exact match on transition `name` (case-insensitive)
    /// 3. Resolve via project statuses: fetch `GET /project/{key}/statuses`,
    ///    find status matching `target_status` by name or category alias,
    ///    then match against available transitions.
    async fn transition_issue(&self, key: &str, target_status: &str) -> Result<()> {
        let url = format!("{}/issue/{}/transitions", self.base_url, key);
        let transitions: JiraTransitionsResponse = self.get(&url).await?;

        // 1. Exact match on to.name
        let transition = transitions
            .transitions
            .iter()
            .find(|t| t.to.name.eq_ignore_ascii_case(target_status))
            .or_else(|| {
                // 2. Exact match on transition name
                transitions
                    .transitions
                    .iter()
                    .find(|t| t.name.eq_ignore_ascii_case(target_status))
            });

        let transition = if let Some(t) = transition {
            t
        } else {
            // 3. Resolve via project statuses + category mapping
            self.find_transition_by_project_statuses(target_status, &transitions)
                .await?
                .ok_or_else(|| {
                    let available: Vec<String> = transitions
                        .transitions
                        .iter()
                        .map(|t| {
                            let cat =
                                t.to.status_category
                                    .as_ref()
                                    .map(|sc| sc.key.as_str())
                                    .unwrap_or("?");
                            format!("{} [{}]", t.to.name, cat)
                        })
                        .collect();
                    Error::InvalidData(format!(
                        "No transition to status '{}' found for issue {}. Available: {:?}",
                        target_status, key, available
                    ))
                })?
        };

        let payload = TransitionPayload {
            transition: TransitionId {
                id: transition.id.clone(),
            },
        };

        let post_url = format!("{}/issue/{}/transitions", self.base_url, key);
        debug!(
            issue = key,
            transition_id = transition.id,
            target = target_status,
            "Transitioning issue"
        );

        let response = self
            .request(reqwest::Method::POST, &post_url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let status_code = status.as_u16();
            let message = response.text().await.unwrap_or_default();
            return Err(Error::from_status(status_code, message));
        }

        Ok(())
    }

    /// Fetch project statuses and find a matching transition.
    ///
    /// Strategy:
    /// 1. Map user input to a category key (e.g., "cancelled" → "done")
    /// 2. Fetch all project statuses via `GET /project/{key}/statuses`
    /// 3. Find project statuses matching by name or category
    /// 4. Match those status names against available transitions
    async fn find_transition_by_project_statuses<'a>(
        &self,
        target_status: &str,
        transitions: &'a JiraTransitionsResponse,
    ) -> Result<Option<&'a JiraTransition>> {
        let project_statuses = self.get_project_statuses().await.unwrap_or_default();

        if project_statuses.is_empty() {
            // Fallback: match directly on transition category (no project statuses available)
            let category_key = generic_status_to_category(target_status);
            return Ok(category_key.and_then(|cat| {
                transitions.transitions.iter().find(|t| {
                    t.to.status_category
                        .as_ref()
                        .is_some_and(|sc| sc.key == cat)
                })
            }));
        }

        // 1. Try to find project status by exact name match
        let matching_status = project_statuses
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(target_status));

        if let Some(status) = matching_status {
            // Found exact status name in project — find transition to it
            if let Some(t) = transitions
                .transitions
                .iter()
                .find(|t| t.to.name.eq_ignore_ascii_case(&status.name))
            {
                return Ok(Some(t));
            }
        }

        // 2. Map generic alias to category, find project statuses in that category,
        //    then match against available transitions
        if let Some(category_key) = generic_status_to_category(target_status) {
            // Find all project statuses in this category
            let category_status_names: Vec<&str> = project_statuses
                .iter()
                .filter(|s| {
                    s.status_category
                        .as_ref()
                        .is_some_and(|sc| sc.key == category_key)
                })
                .map(|s| s.name.as_str())
                .collect();

            debug!(
                target = target_status,
                category = category_key,
                statuses = ?category_status_names,
                "Resolved category to project statuses"
            );

            // Find transition to any of these statuses
            for status_name in &category_status_names {
                if let Some(t) = transitions
                    .transitions
                    .iter()
                    .find(|t| t.to.name.eq_ignore_ascii_case(status_name))
                {
                    return Ok(Some(t));
                }
            }

            // Last resort: match transition by category key directly
            return Ok(transitions.transitions.iter().find(|t| {
                t.to.status_category
                    .as_ref()
                    .is_some_and(|sc| sc.key == category_key)
            }));
        }

        Ok(None)
    }

    /// Fetch all unique statuses for the project.
    ///
    /// Calls `GET /project/{key}/statuses` and flattens statuses
    /// from all issue types, deduplicating by name.
    async fn get_project_statuses(&self) -> Result<Vec<JiraProjectStatus>> {
        let url = format!("{}/project/{}/statuses", self.base_url, self.project_key);
        let issue_type_statuses: Vec<JiraIssueTypeStatuses> = self.get(&url).await?;

        let mut seen = std::collections::HashSet::new();
        let mut statuses = Vec::new();

        for its in &issue_type_statuses {
            for status in &its.statuses {
                let name_lower = status.name.to_lowercase();
                if seen.insert(name_lower) {
                    statuses.push(status.clone());
                }
            }
        }

        debug!(
            project = self.project_key,
            count = statuses.len(),
            "Fetched project statuses"
        );

        Ok(statuses)
    }

    // =========================================================================
    // Jira Structure Plugin API (/rest/structure/2.0/)
    // =========================================================================

    /// Build a URL for the Structure REST API.
    ///
    /// Uses the same root as the regular REST API (`base_url` with the
    /// `/rest/api/{2,3}` suffix stripped) so that Structure calls go
    /// through the configured proxy — `instance_url` is intentionally
    /// reserved for browse links and bypasses any proxy headers/auth
    /// installed via [`JiraClient::with_proxy`].
    fn structure_url(&self, endpoint: &str) -> String {
        let root = instance_url_from_base(&self.base_url);
        format!("{}/rest/structure/2.0{}", root, endpoint)
    }

    /// Make an authenticated GET request to the Structure API.
    async fn structure_get<T: serde::de::DeserializeOwned>(&self, endpoint: &str) -> Result<T> {
        let url = self.structure_url(endpoint);
        debug!(url = %url, "Jira Structure GET");
        let response = self
            .request(reqwest::Method::GET, &url)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;
        handle_structure_response(response).await
    }

    /// Make an authenticated POST request to the Structure API.
    async fn structure_post<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        endpoint: &str,
        body: &B,
    ) -> Result<T> {
        let url = self.structure_url(endpoint);
        debug!(url = %url, "Jira Structure POST");
        let response = self
            .request(reqwest::Method::POST, &url)
            .json(body)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;
        handle_structure_response(response).await
    }

    /// Make an authenticated PUT request to the Structure API (returns body).
    async fn structure_put<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        endpoint: &str,
        body: &B,
    ) -> Result<T> {
        let url = self.structure_url(endpoint);
        debug!(url = %url, "Jira Structure PUT");
        let response = self
            .request(reqwest::Method::PUT, &url)
            .json(body)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;
        handle_structure_response(response).await
    }

    /// Build a URL for the Jira Agile REST API (`/rest/agile/1.0/*`).
    /// Issue #198.
    fn agile_url(&self, endpoint: &str) -> String {
        let root = instance_url_from_base(&self.base_url);
        format!("{}/rest/agile/1.0{}", root, endpoint)
    }

    /// Authenticated GET against the Agile REST API.
    ///
    /// Uses generic `get` — Agile endpoints return JSON and we don't want
    /// the Structure-plugin-specific error hint (Copilot review on PR #205)
    /// leaking into Agile failures.
    async fn agile_get<T: serde::de::DeserializeOwned>(&self, endpoint: &str) -> Result<T> {
        let url = self.agile_url(endpoint);
        debug!(url = %url, "Jira Agile GET");
        self.get(&url).await
    }

    /// Authenticated POST against the Agile REST API with no response
    /// body (Agile's `/sprint/{id}/issue` returns 204). Uses
    /// `post_no_content` for the same reason as `agile_get`.
    async fn agile_post_void<B: serde::Serialize>(&self, endpoint: &str, body: &B) -> Result<()> {
        let url = self.agile_url(endpoint);
        debug!(url = %url, "Jira Agile POST");
        self.post_no_content(&url, body).await
    }

    /// Make an authenticated DELETE request to the Structure API.
    async fn structure_delete_request(&self, endpoint: &str) -> Result<()> {
        let url = self.structure_url(endpoint);
        debug!(url = %url, "Jira Structure DELETE");
        let response = self
            .request(reqwest::Method::DELETE, &url)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let (content_type, body) = read_structure_error_body(response).await;
            return Err(structure_error_from_status(
                status.as_u16(),
                &content_type,
                body,
            ));
        }
        Ok(())
    }

    /// Fetch a compact list of accessible Structures for metadata enrichment.
    ///
    /// Unlike [`Self::get_structures`], this is intended to be called from a
    /// metadata-assembly pipeline and **swallows the "Structure plugin not
    /// installed" error** (HTTP 404, which the Structure endpoint returns as
    /// a generic Jira "dead link" HTML page) into an empty [`Vec`]. The
    /// resulting `Ok(vec![])` is the "no structures are available here"
    /// signal that downstream enrichers key on to decide whether to touch
    /// the `structureId` parameter of Structure tools.
    ///
    /// Credential / permission failures (401 `Unauthorized`, 403
    /// `Forbidden`) still propagate — those indicate the caller's
    /// integration is misconfigured, not that the feature is absent, and
    /// the metadata build should surface the error rather than silently
    /// pretend no structures exist.
    pub async fn list_structures_for_metadata(
        &self,
    ) -> Result<Vec<crate::metadata::JiraStructureRef>> {
        match self
            .structure_get::<crate::types::JiraStructureListResponse>("/structure")
            .await
        {
            Ok(resp) => Ok(resp
                .structures
                .into_iter()
                .map(|s| crate::metadata::JiraStructureRef {
                    id: s.id,
                    name: s.name,
                    description: s.description,
                })
                .collect()),
            // Structure plugin not installed or endpoint removed — treat as
            // "no structures available" so metadata build keeps going.
            Err(Error::NotFound(_)) => Ok(vec![]),
            Err(other) => Err(other),
        }
    }
}

/// Install hint shown when the Structure plugin may not be detected on the
/// Jira host. Phrased as a possibility rather than a definitive diagnosis —
/// the same HTML/XML 404 pattern can also appear if the plugin is installed
/// but the client hit a wrong/changed endpoint.
const STRUCTURE_PLUGIN_HINT: &str = "The Jira Structure plugin may not be installed, not enabled, or the endpoint has moved. Install or upgrade it from the Atlassian Marketplace: https://marketplace.atlassian.com/apps/34717/structure-manage-work-your-way";

/// Detect markup response bodies (HTML and XML) — Jira returns a full
/// login/404 HTML page for missing endpoints and unauthenticated requests,
/// and the Structure plugin itself returns an XML 404 envelope for unknown
/// sub-paths. In both cases we refuse to dump the raw body into the MCP tool
/// response.
fn looks_like_html(content_type: &str, body: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("text/html") || ct.contains("application/xml") || ct.contains("text/xml") {
        return true;
    }
    let head = body.trim_start();
    head.starts_with("<!DOCTYPE")
        || head.starts_with("<!doctype")
        || head.starts_with("<html")
        || head.starts_with("<HTML")
        || head.starts_with("<?xml")
}

/// Read the response body + Content-Type for error diagnostics, tolerating
/// reqwest read failures.
async fn read_structure_error_body(response: reqwest::Response) -> (String, String) {
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = response.text().await.unwrap_or_default();
    (content_type, body)
}

/// Translate a failed Structure API HTTP response into a [`Result`] error.
///
/// Specifically:
///
/// - `404` with an HTML/XML body → soft hint that the endpoint was not found
///   and the Structure plugin may not be installed (without asserting it).
/// - Any other status with an HTML/XML body → short «status + hint» line
///   (we strip the markup to avoid dumping a whole login/error page into the
///   MCP tool response).
/// - JSON / plain-text bodies are forwarded as-is, trimmed to 500 chars so
///   the MCP tool output stays readable.
fn structure_error_from_status(status: u16, content_type: &str, body: String) -> Error {
    let html = looks_like_html(content_type, &body);

    if status == 404 && html {
        return Error::from_status(
            status,
            format!("Structure API endpoint not found (HTTP 404). {STRUCTURE_PLUGIN_HINT}"),
        );
    }

    if html {
        return Error::from_status(
            status,
            format!(
                "Jira returned a non-JSON (HTML/XML) response for a Structure API call (HTTP {status}). {STRUCTURE_PLUGIN_HINT}"
            ),
        );
    }

    let trimmed = if body.len() > 500 {
        let end = safe_char_boundary(&body, 500);
        format!("{}...(truncated, total {} bytes)", &body[..end], body.len())
    } else {
        body
    };
    Error::from_status(status, trimmed)
}

/// Build a redacted preview of a response body for parse-error diagnostics.
/// If the body is markup (HTML/XML), we never include any portion of it —
/// login pages can contain ~2 KB of boilerplate that would leak into the MCP
/// tool output. Otherwise, truncate to 300 chars on a UTF-8 boundary.
fn structure_parse_preview(content_type: &str, body: &str) -> String {
    if looks_like_html(content_type, body) {
        format!(
            "<{} bytes of HTML/XML redacted — non-JSON body indicates a non-Structure endpoint or missing plugin>",
            body.len()
        )
    } else if body.len() > 300 {
        let end = safe_char_boundary(body, 300);
        format!("{}...(truncated, total {} bytes)", &body[..end], body.len())
    } else {
        body.to_string()
    }
}

/// Unified response handler for Structure API helpers — mirrors
/// [`JiraClient::handle_response`] but routes both error bodies and
/// successful-but-unparseable bodies through [`looks_like_html`] so markup
/// never leaks into MCP tool output.
async fn handle_structure_response<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T> {
    let status = response.status();

    if !status.is_success() {
        let (content_type, body) = read_structure_error_body(response).await;
        warn!(
            status = status.as_u16(),
            content_type = %content_type,
            body_len = body.len(),
            "Jira Structure API error response"
        );
        return Err(structure_error_from_status(
            status.as_u16(),
            &content_type,
            body,
        ));
    }

    // Capture Content-Type before consuming the response into text — needed
    // for markup detection on the parse-error path when Jira returns e.g. an
    // SSO redirect HTML page with a 2xx status.
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let body = response.text().await.map_err(|e| {
        Error::InvalidData(format!("Failed to read Structure response body: {}", e))
    })?;

    serde_json::from_str::<T>(&body).map_err(|e| {
        let preview = structure_parse_preview(&content_type, &body);
        warn!(
            error = %e,
            body_preview = preview,
            content_type = %content_type,
            "Failed to parse Jira Structure response"
        );
        Error::InvalidData(format!(
            "Failed to parse Jira Structure response: {}. Body preview: {}",
            e, preview
        ))
    })
}

// =============================================================================
// Flavor detection and URL building
// =============================================================================

/// Detect Jira flavor from the instance URL.
fn detect_flavor(url: &str) -> JiraFlavor {
    if url.contains(".atlassian.net") {
        JiraFlavor::Cloud
    } else {
        JiraFlavor::SelfHosted
    }
}

/// Build the API base URL from the instance URL and flavor.
fn build_api_base(url: &str, flavor: JiraFlavor) -> String {
    let base = url.trim_end_matches('/');
    match flavor {
        JiraFlavor::Cloud => format!("{}/rest/api/3", base),
        JiraFlavor::SelfHosted => format!("{}/rest/api/2", base),
    }
}

/// Base64-encode a string (simple implementation without external crate).
fn base64_encode(input: &str) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut result = String::new();

    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };

        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(CHARSET[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARSET[((triple >> 12) & 0x3F) as usize] as char);

        if chunk.len() > 1 {
            result.push(CHARSET[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }

        if chunk.len() > 2 {
            result.push(CHARSET[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }

    result
}

// =============================================================================
// ADF (Atlassian Document Format) converters
// =============================================================================

/// Convert plain text to ADF document (for Jira Cloud API v3).
///
/// Splits on `\n\n` for paragraphs, uses `hardBreak` for single `\n`.
fn text_to_adf(text: &str) -> serde_json::Value {
    if text.is_empty() {
        return serde_json::json!({
            "version": 1,
            "type": "doc",
            "content": [{
                "type": "paragraph",
                "content": []
            }]
        });
    }

    let paragraphs: Vec<&str> = text.split("\n\n").collect();
    let content: Vec<serde_json::Value> = paragraphs
        .iter()
        .map(|para| {
            let lines: Vec<&str> = para.split('\n').collect();
            let mut inline_content: Vec<serde_json::Value> = Vec::new();

            for (i, line) in lines.iter().enumerate() {
                if i > 0 {
                    inline_content.push(serde_json::json!({ "type": "hardBreak" }));
                }
                if !line.is_empty() {
                    inline_content.push(serde_json::json!({
                        "type": "text",
                        "text": *line
                    }));
                }
            }

            serde_json::json!({
                "type": "paragraph",
                "content": inline_content
            })
        })
        .collect();

    serde_json::json!({
        "version": 1,
        "type": "doc",
        "content": content
    })
}

/// Extract plain text from an ADF document (for Jira Cloud API v3 responses).
///
/// Recursively walks the ADF tree extracting text nodes.
/// Falls back to returning the value as a string if it's not an ADF document.
fn adf_to_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(obj) => {
            let doc_type = obj.get("type").and_then(|t| t.as_str());

            // If it's a text node, return the text
            if doc_type == Some("text") {
                return obj
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
            }

            // If it's a hardBreak, return newline
            if doc_type == Some("hardBreak") {
                return "\n".to_string();
            }

            // Recurse into content array
            if let Some(content) = obj.get("content").and_then(|c| c.as_array()) {
                let texts: Vec<String> = content.iter().map(adf_to_text).collect();
                let joined = texts.join("");

                // Add paragraph separation for top-level paragraphs
                if doc_type == Some("paragraph") {
                    return joined;
                }
                if doc_type == Some("doc") {
                    // Join paragraphs with double newline
                    let para_texts: Vec<String> = content
                        .iter()
                        .map(adf_to_text)
                        .filter(|s| !s.is_empty())
                        .collect();
                    return para_texts.join("\n\n");
                }

                return joined;
            }

            String::new()
        }
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Read description from a Jira issue, handling both ADF and plain text.
fn read_description(value: &Option<serde_json::Value>, flavor: JiraFlavor) -> Option<String> {
    let value = value.as_ref()?;
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => {
            if s.is_empty() {
                None
            } else {
                Some(s.clone())
            }
        }
        _ => {
            if flavor == JiraFlavor::Cloud {
                let text = adf_to_text(value);
                if text.is_empty() { None } else { Some(text) }
            } else {
                // Self-hosted v2 shouldn't return ADF, but handle gracefully
                Some(value.to_string())
            }
        }
    }
}

/// Read comment body from a Jira comment, handling both ADF and plain text.
fn read_comment_body(value: &Option<serde_json::Value>, flavor: JiraFlavor) -> String {
    match value {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Null) | None => String::new(),
        Some(v) => {
            if flavor == JiraFlavor::Cloud {
                adf_to_text(v)
            } else {
                v.to_string()
            }
        }
    }
}

// =============================================================================
// Mapping functions: Jira types -> Unified types
// =============================================================================

fn map_user(jira_user: Option<&JiraUser>) -> Option<User> {
    jira_user.map(|u| {
        let id = u
            .account_id
            .clone()
            .or_else(|| u.name.clone())
            .unwrap_or_default();
        let username = u
            .name
            .clone()
            .or_else(|| u.account_id.clone())
            .unwrap_or_default();
        User {
            id,
            username,
            name: u.display_name.clone(),
            email: u.email_address.clone(),
            avatar_url: None,
        }
    })
}

fn map_priority(jira_priority: Option<&JiraPriority>) -> Option<String> {
    jira_priority.map(|p| match p.name.to_lowercase().as_str() {
        "highest" | "critical" | "blocker" => "urgent".to_string(),
        "high" => "high".to_string(),
        "medium" => "normal".to_string(),
        "low" => "low".to_string(),
        "lowest" | "trivial" => "low".to_string(),
        other => other.to_string(),
    })
}

fn map_state(status: Option<&JiraStatus>) -> String {
    status
        .map(|s| s.name.clone())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Parse issue key like "jira#WEB-1" to get the raw Jira key "WEB-1".
/// If the key doesn't have a "jira#" prefix, returns it as-is (for internal calls).
fn parse_jira_key(key: &str) -> &str {
    key.strip_prefix("jira#").unwrap_or(key)
}

fn map_issue(issue: &JiraIssue, flavor: JiraFlavor, instance_url: &str) -> Issue {
    Issue {
        key: format!("jira#{}", issue.key),
        title: issue.fields.summary.clone().unwrap_or_default(),
        description: read_description(&issue.fields.description, flavor),
        state: map_state(issue.fields.status.as_ref()),
        source: "jira".to_string(),
        priority: map_priority(issue.fields.priority.as_ref()),
        labels: issue.fields.labels.clone(),
        author: map_user(issue.fields.reporter.as_ref()),
        assignees: issue
            .fields
            .assignee
            .as_ref()
            .map(|a| vec![map_user(Some(a)).unwrap()])
            .unwrap_or_default(),
        url: Some(format!("{}/browse/{}", instance_url, issue.key)),
        created_at: issue.fields.created.clone(),
        updated_at: issue.fields.updated.clone(),
        attachments_count: if issue.fields.attachment.is_empty() {
            None
        } else {
            Some(issue.fields.attachment.len() as u32)
        },
        parent: None,
        subtasks: vec![],
    }
}

fn map_relations(issue: &JiraIssue, flavor: JiraFlavor, instance_url: &str) -> IssueRelations {
    let mut relations = IssueRelations::default();

    // Parent
    if let Some(parent) = &issue.fields.parent {
        relations.parent = Some(map_issue(parent, flavor, instance_url));
    }

    // Subtasks
    relations.subtasks = issue
        .fields
        .subtasks
        .iter()
        .map(|s| map_issue(s, flavor, instance_url))
        .collect();

    // Issue links
    for link in &issue.fields.issuelinks {
        let link_name = &link.link_type.name;

        let outward_lower = link.link_type.outward.as_deref().map(str::to_lowercase);
        let inward_lower = link.link_type.inward.as_deref().map(str::to_lowercase);

        if let Some(outward) = &link.outward_issue {
            let mapped = map_issue(outward, flavor, instance_url);
            let issue_link = IssueLink {
                issue: mapped,
                link_type: link_name.clone(),
            };

            match outward_lower.as_deref() {
                Some(s) if s.contains("block") => relations.blocks.push(issue_link),
                Some(s) if s.contains("duplicate") => relations.duplicates.push(issue_link),
                _ => relations.related_to.push(issue_link),
            }
        }

        if let Some(inward) = &link.inward_issue {
            let mapped = map_issue(inward, flavor, instance_url);
            let issue_link = IssueLink {
                issue: mapped,
                link_type: link_name.clone(),
            };

            match inward_lower.as_deref() {
                Some(s) if s.contains("block") => relations.blocked_by.push(issue_link),
                Some(s) if s.contains("duplicate") => relations.duplicates.push(issue_link),
                _ => relations.related_to.push(issue_link),
            }
        }
    }

    relations
}

fn map_comment(jira_comment: &JiraComment, flavor: JiraFlavor) -> Comment {
    Comment {
        id: jira_comment.id.clone(),
        body: read_comment_body(&jira_comment.body, flavor),
        author: map_user(jira_comment.author.as_ref()),
        created_at: jira_comment.created.clone(),
        updated_at: jira_comment.updated.clone(),
        position: None,
    }
}

/// Map a Jira attachment payload to the provider-agnostic [`AssetMeta`].
fn map_jira_attachment(raw: &JiraAttachment) -> AssetMeta {
    // Prefer the explicit `filename` from Jira. Don't fall back to
    // `filename_from_url(content)` because Jira content URLs typically
    // end with `/attachment/content/{id}`, producing useless filenames
    // like "42". Fall back to `attachment-{id}` instead.
    let filename = raw
        .filename
        .clone()
        .unwrap_or_else(|| format!("attachment-{}", raw.id));
    let author = raw
        .author
        .as_ref()
        .and_then(|u| map_user(Some(u)))
        .map(|u| u.name.unwrap_or(u.username));

    AssetMeta {
        id: raw.id.clone(),
        filename,
        mime_type: raw.mime_type.clone(),
        size: raw.size,
        url: raw.content.clone(),
        created_at: raw.created.clone(),
        author,
        cached: false,
        local_path: None,
        checksum_sha256: None,
        analysis: None,
    }
}

/// Map a unified priority string to a Jira priority name.
fn priority_to_jira(priority: &str) -> String {
    match priority {
        "urgent" => "Highest".to_string(),
        "high" => "High".to_string(),
        "normal" => "Medium".to_string(),
        "low" => "Low".to_string(),
        other => other.to_string(),
    }
}

/// Map generic/alias status names to Jira status category keys.
///
/// Jira has 4 status categories: `new`, `indeterminate`, `done`, `undefined`.
/// Escape special characters in a JQL string value.
///
/// JQL uses double quotes for string values. Backslashes and double quotes
/// inside the value must be escaped to prevent injection.
fn escape_jql(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Merge custom fields (Object format) into a serializable payload.
/// Only keys with `customfield_` prefix are merged to prevent overwriting
/// core Jira fields like `project`, `summary`, `issuetype`.
/// Returns the number of custom fields actually merged.
fn merge_custom_fields_into_payload<T: serde::Serialize>(
    payload: T,
    custom_fields: &Option<serde_json::Value>,
) -> Result<(serde_json::Value, usize)> {
    let mut value = serde_json::to_value(payload)
        .map_err(|e| Error::InvalidData(format!("failed to serialize issue payload: {e}")))?;
    let mut merged_count = 0;
    if let Some(serde_json::Value::Object(cf)) = custom_fields
        && let Some(fields) = value.get_mut("fields").and_then(|f| f.as_object_mut())
    {
        for (k, v) in cf {
            if k.starts_with("customfield_") {
                fields.insert(k.clone(), v.clone());
                merged_count += 1;
            } else {
                tracing::warn!(field = %k, "Skipping non-custom field in customFields (expected customfield_* prefix)");
            }
        }
    }
    Ok((value, merged_count))
}

/// Check whether a JQL string already contains a project filter clause.
/// Matches `project` as a JQL field name (word boundary) followed by an operator.
/// Skips occurrences inside quoted strings to avoid false positives.
fn has_project_clause(jql: &str) -> bool {
    let lower = jql.to_lowercase();
    let bytes = lower.as_bytes();
    let keyword = b"project";
    let mut in_quote = false;
    let mut i = 0;

    while i < bytes.len() {
        // Track quoted strings — skip content inside quotes
        if bytes[i] == b'\\' && in_quote && i + 1 < bytes.len() {
            i += 2; // skip escaped character
            continue;
        }
        if bytes[i] == b'"' {
            in_quote = !in_quote;
            i += 1;
            continue;
        }
        if in_quote {
            i += 1;
            continue;
        }

        // Check for "project" keyword at position i
        if i + keyword.len() <= bytes.len() && &bytes[i..i + keyword.len()] == keyword {
            // Word boundary before: not preceded by alphanumeric or underscore
            if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
                i += 1;
                continue;
            }
            // Check what follows — skip whitespace, then expect a JQL operator
            let after = &lower[i + keyword.len()..];
            let trimmed = after.trim_start();
            if trimmed.starts_with("!=")
                || trimmed.starts_with("not in ")
                || trimmed.starts_with("not in(")
                || trimmed.starts_with('=')
                || trimmed.starts_with('~')
                || trimmed.starts_with("in ")
                || trimmed.starts_with("in(")
            {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// This maps user-friendly aliases to the correct category key, used as fallback
/// when the exact status name is not found in available transitions.
fn generic_status_to_category(status: &str) -> Option<&'static str> {
    match status.to_lowercase().as_str() {
        "closed" | "done" | "resolved" | "canceled" | "cancelled" => Some("done"),
        "open" | "new" | "todo" | "to do" | "reopen" | "reopened" => Some("new"),
        "in_progress" | "in progress" | "in-progress" => Some("indeterminate"),
        _ => None,
    }
}

/// Check if a keyword appears outside quoted strings in JQL.
fn has_unquoted_keyword(jql: &str, keyword: &str) -> bool {
    let lower = jql.to_lowercase();
    let kw = keyword.to_lowercase();
    let kw_bytes = kw.as_bytes();
    let bytes = lower.as_bytes();
    let mut in_quote = false;
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'\\' && in_quote && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            in_quote = !in_quote;
            i += 1;
            continue;
        }
        if !in_quote
            && i + kw_bytes.len() <= bytes.len()
            && bytes[i..i + kw_bytes.len()] == *kw_bytes
        {
            return true;
        }
        i += 1;
    }
    false
}

/// Get the Jira instance URL from the API base URL.
fn instance_url_from_base(base_url: &str) -> String {
    base_url
        .trim_end_matches("/rest/api/3")
        .trim_end_matches("/rest/api/2")
        .to_string()
}

// =============================================================================
// Structure helpers
// =============================================================================

/// Transform compact `rows[] + depths[]` from Structure API into a
/// nested tree. Returns `InvalidData` if the two vectors are not the
/// same length — the Structure API contract guarantees alignment, and
/// a mismatch here would otherwise silently nest rows at depth 0 and
/// produce a subtly wrong tree.
fn build_forest_tree(
    rows: &[crate::types::JiraForestRow],
    depths: &[u32],
) -> Result<Vec<StructureNode>> {
    if rows.len() != depths.len() {
        return Err(Error::InvalidData(format!(
            "Structure forest response has {} rows but {} depths",
            rows.len(),
            depths.len()
        )));
    }
    let mut roots: Vec<StructureNode> = Vec::new();
    let mut stack: Vec<StructureNode> = Vec::new();

    for (row, depth) in rows.iter().zip(depths.iter()) {
        let depth = *depth as usize;
        let node = StructureNode {
            row_id: row.id,
            item_id: row.item_id.clone(),
            item_type: row.item_type.clone(),
            children: Vec::new(),
        };

        // Pop stack to find parent at depth - 1
        while stack.len() > depth {
            let child = stack.pop().expect("stack.len() > depth > 0");
            if let Some(parent) = stack.last_mut() {
                parent.children.push(child);
            } else {
                roots.push(child);
            }
        }

        stack.push(node);
    }

    // Flush remaining stack
    while let Some(child) = stack.pop() {
        if let Some(parent) = stack.last_mut() {
            parent.children.push(child);
        } else {
            roots.push(child);
        }
    }

    Ok(roots)
}

/// Map Jira Structure view to unified type.
fn map_structure_view(view: crate::types::JiraStructureView) -> StructureView {
    StructureView {
        id: view.id,
        name: view.name,
        structure_id: view.structure_id,
        columns: view
            .columns
            .into_iter()
            .map(|c| StructureViewColumn {
                id: c.id,
                field: c.field,
                formula: c.formula,
                width: c.width,
            })
            .collect(),
        group_by: view.group_by,
        sort_by: view.sort_by,
        filter: view.filter,
    }
}

// =============================================================================
// Trait implementations
// =============================================================================

#[async_trait]
impl IssueProvider for JiraClient {
    async fn get_issues(&self, filter: IssueFilter) -> Result<ProviderResult<Issue>> {
        let limit = filter.limit.unwrap_or(20);
        if limit == 0 {
            return Ok(vec![].into());
        }
        let offset = filter.offset.unwrap_or(0);

        // Resolve effective project key: filter override → self.project_key
        // Treat blank project_key as unset
        let effective_project = filter
            .project_key
            .as_deref()
            .filter(|k| !k.trim().is_empty())
            .unwrap_or(&self.project_key);

        // Build JQL query — native_query takes precedence over filter-based construction
        let escaped_project = escape_jql(effective_project);
        let jql = if let Some(native) = &filter.native_query
            && !native.trim().is_empty()
        {
            // If native query doesn't mention a project clause, prepend one
            // (Jira Cloud requires a project filter)
            if has_project_clause(native) {
                native.clone()
            } else if native.trim_start().to_lowercase().starts_with("order by") {
                format!("project = \"{}\" {}", escaped_project, native)
            } else {
                format!("project = \"{}\" AND {}", escaped_project, native)
            }
        } else {
            let mut jql_parts: Vec<String> = vec![format!("project = \"{}\"", escaped_project)];

            // State filter
            if let Some(state) = &filter.state {
                match state.as_str() {
                    "open" | "opened" => {
                        jql_parts.push("statusCategory != Done".to_string());
                    }
                    "closed" | "done" => {
                        jql_parts.push("statusCategory = Done".to_string());
                    }
                    "all" => {} // No filter
                    other => {
                        // Exact status name
                        jql_parts.push(format!("status = \"{}\"", escape_jql(other)));
                    }
                }
            }

            if let Some(search) = &filter.search {
                jql_parts.push(format!("summary ~ \"{}\"", escape_jql(search)));
            }

            if let Some(labels) = &filter.labels {
                for label in labels {
                    jql_parts.push(format!("labels = \"{}\"", escape_jql(label)));
                }
            }

            if let Some(assignee) = &filter.assignee {
                jql_parts.push(format!("assignee = \"{}\"", escape_jql(assignee)));
            }

            jql_parts.join(" AND ")
        };

        // Add ORDER BY — skip if native_query already contains one
        let order_by = match filter.sort_by.as_deref() {
            Some("created_at" | "created") => "created",
            Some("priority") => "priority",
            _ => "updated",
        };
        let order = match filter.sort_order.as_deref() {
            Some("asc") => "ASC",
            _ => "DESC",
        };
        let has_order_by = has_unquoted_keyword(&jql, "order by");
        let jql_with_order = if has_order_by {
            jql
        } else {
            format!("{} ORDER BY {} {}", jql, order_by, order)
        };

        let instance_url = &self.instance_url;

        match self.flavor {
            JiraFlavor::Cloud => {
                // Cloud: GET /search/jql?jql=...&maxResults=...&nextPageToken=...
                let url = format!("{}/search/jql", self.base_url);

                let mut all_issues: Vec<Issue> = Vec::new();
                let mut next_page_token: Option<String> = None;
                let total_needed = offset.saturating_add(limit);
                let mut fetched_count = 0u32;

                // Explicitly request required fields — without this, Jira Cloud
                // may return minimal responses (only `id`) for certain JQL queries
                // (e.g., label filters), causing deserialization failures.
                let fields = "summary,status,priority,assignee,reporter,labels,created,updated,parent,subtasks".to_string();

                loop {
                    let mut params: Vec<(&str, String)> = vec![
                        ("jql", jql_with_order.clone()),
                        ("maxResults", std::cmp::min(limit, 50).to_string()),
                        ("fields", fields.clone()),
                    ];

                    if let Some(token) = &next_page_token {
                        params.push(("nextPageToken", token.clone()));
                    }

                    let param_refs: Vec<(&str, &str)> =
                        params.iter().map(|(k, v)| (*k, v.as_str())).collect();

                    debug!(url = url, params = ?param_refs, "Jira Cloud search");

                    let response = self
                        .request(reqwest::Method::GET, &url)
                        .query(&param_refs)
                        .send()
                        .await
                        .map_err(|e| Error::Http(e.to_string()))?;

                    let search_resp: JiraCloudSearchResponse =
                        self.handle_response(response).await?;

                    let page_len = search_resp.issues.len() as u32;
                    for issue in &search_resp.issues {
                        if fetched_count >= offset && all_issues.len() < limit as usize {
                            all_issues.push(map_issue(issue, self.flavor, instance_url));
                        }
                        fetched_count += 1;
                    }

                    if all_issues.len() >= limit as usize {
                        break;
                    }

                    match search_resp.next_page_token {
                        Some(token) if page_len > 0 && fetched_count < total_needed => {
                            next_page_token = Some(token);
                        }
                        _ => break,
                    }
                }

                let mut result = ProviderResult::new(all_issues);
                result.pagination = Some(devboy_core::Pagination {
                    offset,
                    limit,
                    total: None, // Jira Cloud cursor-based, no total
                    has_more: next_page_token.is_some(),
                    next_cursor: next_page_token,
                });
                result.sort_info = Some(devboy_core::SortInfo {
                    sort_by: Some(order_by.into()),
                    sort_order: match order {
                        "ASC" => devboy_core::SortOrder::Asc,
                        _ => devboy_core::SortOrder::Desc,
                    },
                    available_sorts: vec!["created".into(), "updated".into(), "priority".into()],
                });
                Ok(result)
            }
            JiraFlavor::SelfHosted => {
                // Self-Hosted: GET /search?jql=...&startAt=...&maxResults=...
                let url = format!("{}/search", self.base_url);

                let params: Vec<(&str, String)> = vec![
                    ("jql", jql_with_order),
                    ("startAt", offset.to_string()),
                    ("maxResults", limit.to_string()),
                    ("fields", "summary,status,priority,assignee,reporter,labels,created,updated,parent,subtasks".to_string()),
                ];

                let param_refs: Vec<(&str, &str)> =
                    params.iter().map(|(k, v)| (*k, v.as_str())).collect();

                debug!(url = url, params = ?param_refs, "Jira Self-Hosted search");

                let response = self
                    .request(reqwest::Method::GET, &url)
                    .query(&param_refs)
                    .send()
                    .await
                    .map_err(|e| Error::Http(e.to_string()))?;

                let search_resp: JiraSearchResponse = self.handle_response(response).await?;

                let total = search_resp.total;
                let has_more = match (total, search_resp.start_at, search_resp.max_results) {
                    (Some(t), Some(s), Some(m)) => s + m < t,
                    _ => false,
                };

                let issues: Vec<Issue> = search_resp
                    .issues
                    .iter()
                    .map(|i| map_issue(i, self.flavor, instance_url))
                    .collect();

                let mut result = ProviderResult::new(issues);
                result.pagination = Some(devboy_core::Pagination {
                    offset,
                    limit,
                    total,
                    has_more,
                    next_cursor: None,
                });
                result.sort_info = Some(devboy_core::SortInfo {
                    sort_by: Some(order_by.into()),
                    sort_order: match order {
                        "ASC" => devboy_core::SortOrder::Asc,
                        _ => devboy_core::SortOrder::Desc,
                    },
                    available_sorts: vec!["created".into(), "updated".into(), "priority".into()],
                });
                Ok(result)
            }
        }
    }

    async fn get_issue(&self, key: &str) -> Result<Issue> {
        let jira_key = parse_jira_key(key);
        let url = format!("{}/issue/{}", self.base_url, jira_key);
        let issue: JiraIssue = self.get(&url).await?;
        Ok(map_issue(&issue, self.flavor, &self.instance_url))
    }

    async fn create_issue(&self, input: CreateIssueInput) -> Result<Issue> {
        let description = input.description.map(|d| {
            if self.flavor == JiraFlavor::Cloud {
                text_to_adf(&d)
            } else {
                serde_json::Value::String(d)
            }
        });

        let labels = if input.labels.is_empty() {
            None
        } else {
            Some(input.labels)
        };
        let has_labels = labels.is_some();

        let priority = input.priority.as_deref().map(|p| PriorityName {
            name: priority_to_jira(p),
        });

        let assignee = input.assignees.first().map(|a| {
            if self.flavor == JiraFlavor::Cloud {
                serde_json::json!({ "accountId": a })
            } else {
                serde_json::json!({ "name": a })
            }
        });

        let effective_project = input.project_id.unwrap_or_else(|| self.project_key.clone());
        let effective_issue_type = input.issue_type.unwrap_or_else(|| "Task".to_string());

        // Issue #197: pass through component IDs to the payload.
        let components = if input.components.is_empty() {
            None
        } else {
            Some(
                input
                    .components
                    .into_iter()
                    .map(|name| crate::types::ComponentRef { name })
                    .collect(),
            )
        };

        let fix_versions = if input.fix_versions.is_empty() {
            None
        } else {
            Some(
                input
                    .fix_versions
                    .into_iter()
                    .map(|name| crate::types::VersionRef { name })
                    .collect(),
            )
        };

        let payload = CreateIssuePayload {
            fields: CreateIssueFields {
                project: ProjectKey {
                    key: effective_project,
                },
                summary: input.title,
                issuetype: IssueType {
                    name: effective_issue_type,
                },
                description,
                labels,
                priority,
                assignee,
                components,
                fix_versions,
                parent: input.parent.map(|key| crate::types::IssueKeyRef { key }),
            },
        };

        let (mut payload, _) = merge_custom_fields_into_payload(payload, &input.custom_fields)?;

        let url = format!("{}/issue", self.base_url);
        let create_result: std::result::Result<CreateIssueResponse, Error> =
            self.post(&url, &payload).await;

        let create_resp = match create_result {
            Ok(resp) => resp,
            Err(e)
                if has_labels
                    && e.to_string().contains("labels")
                    && e.to_string().contains("not on the appropriate screen") =>
            {
                // Labels field is not on the Jira create screen
                // (common on Self-Hosted). Retry without labels and set them via
                // update afterwards.
                tracing::warn!("Create issue failed with labels, retrying without: {e}");
                let saved_labels = payload
                    .get_mut("fields")
                    .and_then(|f| f.as_object_mut())
                    .and_then(|f| f.remove("labels"));
                let resp: CreateIssueResponse = self.post(&url, &payload).await?;

                // Best-effort: try to set labels via PUT update
                if let Some(lbl_value) = saved_labels
                    && let Ok(lbl) = serde_json::from_value::<Vec<String>>(lbl_value)
                {
                    let update = UpdateIssueInput {
                        labels: Some(lbl),
                        ..Default::default()
                    };
                    if let Err(e) = self.update_issue(&resp.key, update).await {
                        tracing::warn!("Failed to set labels after create: {e}");
                    }
                }
                resp
            }
            Err(e) => return Err(e),
        };

        // Fetch the full issue to return
        self.get_issue(&create_resp.key).await
    }

    async fn update_issue(&self, key: &str, input: UpdateIssueInput) -> Result<Issue> {
        let jira_key = parse_jira_key(key);

        let description = input.description.map(|d| {
            if self.flavor == JiraFlavor::Cloud {
                text_to_adf(&d)
            } else {
                serde_json::Value::String(d)
            }
        });

        let priority = input.priority.as_deref().map(|p| PriorityName {
            name: priority_to_jira(p),
        });

        let assignee = input.assignees.as_ref().and_then(|a| {
            a.first().map(|username| {
                if self.flavor == JiraFlavor::Cloud {
                    serde_json::json!({ "accountId": username })
                } else {
                    serde_json::json!({ "name": username })
                }
            })
        });

        let labels = input.labels;

        // Issue #197: components. `None` → untouched, `Some([])` → clear.
        let components = input.components.map(|ids| {
            ids.into_iter()
                .map(|name| crate::types::ComponentRef { name })
                .collect()
        });
        let has_components = components.is_some();

        // Fix versions. `None` → untouched, `Some([])` → clear.
        let fix_versions = input.fix_versions.map(|names| {
            names
                .into_iter()
                .map(|name| crate::types::VersionRef { name })
                .collect()
        });
        let has_fix_versions = fix_versions.is_some();

        let fields = UpdateIssueFields {
            summary: input.title,
            description,
            labels,
            priority,
            assignee,
            components,
            fix_versions,
        };

        let has_custom_fields = input.custom_fields.as_ref().is_some_and(|v| {
            v.as_object()
                .is_some_and(|obj| obj.keys().any(|k| k.starts_with("customfield_")))
        });

        // Only call PUT if there are field updates
        let has_field_updates = fields.summary.is_some()
            || fields.description.is_some()
            || fields.labels.is_some()
            || fields.priority.is_some()
            || fields.assignee.is_some()
            || has_components
            || has_fix_versions
            || has_custom_fields;

        if has_field_updates {
            let url = format!("{}/issue/{}", self.base_url, jira_key);
            let payload = UpdateIssuePayload { fields };
            let (payload, _) = merge_custom_fields_into_payload(payload, &input.custom_fields)?;
            self.put(&url, &payload).await?;
        }

        // Handle status change via transitions
        if let Some(state) = &input.state {
            self.transition_issue(jira_key, state).await?;
        }

        // Fetch updated issue
        self.get_issue(jira_key).await
    }

    async fn get_comments(&self, issue_key: &str) -> Result<ProviderResult<Comment>> {
        let jira_key = parse_jira_key(issue_key);
        let url = format!("{}/issue/{}/comment", self.base_url, jira_key);
        let response: JiraCommentsResponse = self.get(&url).await?;
        Ok(response
            .comments
            .iter()
            .map(|c| map_comment(c, self.flavor))
            .collect::<Vec<_>>()
            .into())
    }

    async fn add_comment(&self, issue_key: &str, body: &str) -> Result<Comment> {
        let jira_key = parse_jira_key(issue_key);
        let comment_body = if self.flavor == JiraFlavor::Cloud {
            text_to_adf(body)
        } else {
            serde_json::Value::String(body.to_string())
        };

        let payload = AddCommentPayload { body: comment_body };

        let url = format!("{}/issue/{}/comment", self.base_url, jira_key);
        let jira_comment: JiraComment = self.post(&url, &payload).await?;
        Ok(map_comment(&jira_comment, self.flavor))
    }

    async fn get_statuses(&self) -> Result<ProviderResult<IssueStatus>> {
        let project_statuses = self.get_project_statuses().await?;

        let statuses: Vec<IssueStatus> = project_statuses
            .iter()
            .enumerate()
            .map(|(idx, s)| {
                let category = s
                    .status_category
                    .as_ref()
                    .map(|sc| match sc.key.as_str() {
                        "new" => "open".to_string(),
                        "indeterminate" => "in_progress".to_string(),
                        "done" => "done".to_string(),
                        other => other.to_string(),
                    })
                    .unwrap_or_else(|| "custom".to_string());

                IssueStatus {
                    id: s.id.clone().unwrap_or_else(|| s.name.clone()),
                    name: s.name.clone(),
                    category,
                    color: None,
                    order: Some(idx as u32),
                }
            })
            .collect();

        Ok(statuses.into())
    }

    async fn get_users(&self, options: GetUsersOptions) -> Result<ProviderResult<User>> {
        let start_at = options.start_at.unwrap_or(0);
        let max_results = options.max_results.unwrap_or(50);

        // Use assignable search if project_key is provided, otherwise generic user search
        let url = if let Some(ref project_key) = options.project_key {
            format!(
                "{}/user/assignable/search?project={}&startAt={}&maxResults={}",
                self.base_url, project_key, start_at, max_results
            )
        } else {
            let query = options.search.as_deref().unwrap_or("");
            match self.flavor {
                JiraFlavor::Cloud => format!(
                    "{}/user/search?query={}&startAt={}&maxResults={}",
                    self.base_url, query, start_at, max_results
                ),
                JiraFlavor::SelfHosted => format!(
                    "{}/user/search?username={}&startAt={}&maxResults={}",
                    self.base_url,
                    if query.is_empty() { "." } else { query },
                    start_at,
                    max_results
                ),
            }
        };

        let jira_users: Vec<JiraUser> = self.get(&url).await?;

        let users: Vec<User> = jira_users
            .iter()
            .map(|u| map_user(Some(u)).unwrap_or_default())
            .collect();

        Ok(users.into())
    }

    async fn link_issues(&self, source_key: &str, target_key: &str, link_type: &str) -> Result<()> {
        let source_jira_key = parse_jira_key(source_key).to_string();
        let target_jira_key = parse_jira_key(target_key).to_string();

        let link_type_name = match link_type {
            "blocks" => "Blocks",
            "blocked_by" => "Blocks", // reversed direction
            "relates_to" => "Relates",
            "duplicates" => "Duplicate",
            "clones" => "Cloners",
            other => other,
        };

        // For "blocked_by", swap source and target so the direction is correct
        let (outward_key, inward_key) = if link_type == "blocked_by" {
            (target_jira_key, source_jira_key)
        } else {
            (source_jira_key, target_jira_key)
        };

        let payload = CreateIssueLinkPayload {
            link_type: IssueLinkTypeName {
                name: link_type_name.to_string(),
            },
            outward_issue: IssueKeyRef { key: outward_key },
            inward_issue: IssueKeyRef { key: inward_key },
        };

        let url = format!("{}/issueLink", self.base_url);
        self.post_no_content(&url, &payload).await?;

        Ok(())
    }

    async fn get_issue_relations(&self, issue_key: &str) -> Result<IssueRelations> {
        let jira_key = parse_jira_key(issue_key);
        let url = format!(
            "{}/issue/{}?fields=parent,subtasks,issuelinks,summary,status,priority",
            self.base_url, jira_key
        );
        let issue: JiraIssue = self.get(&url).await?;
        Ok(map_relations(&issue, self.flavor, &self.instance_url))
    }

    async fn upload_attachment(
        &self,
        issue_key: &str,
        filename: &str,
        data: &[u8],
    ) -> Result<String> {
        let jira_key = parse_jira_key(issue_key);
        let url = format!("{}/issue/{}/attachments", self.base_url, jira_key);

        let part = reqwest::multipart::Part::bytes(data.to_vec())
            .file_name(filename.to_string())
            .mime_str("application/octet-stream")
            .map_err(|e| Error::Http(format!("failed to build multipart: {e}")))?;
        let form = reqwest::multipart::Form::new().part("file", part);

        // Use request_raw (no Content-Type) so reqwest can set its own
        // multipart/form-data boundary header. self.request() sets
        // Content-Type: application/json which conflicts with multipart.
        let response = self
            .request_raw(reqwest::Method::POST, &url)
            // Jira requires the X-Atlassian-Token header to bypass its XSRF check
            // on file uploads: https://developer.atlassian.com/cloud/jira/platform/rest/v3/api-group-issue-attachments/
            .header("X-Atlassian-Token", "no-check")
            .multipart(form)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(Error::from_status(status.as_u16(), message));
        }

        // Jira returns an array of attachment descriptors; we take the first.
        let attachments: Vec<JiraAttachment> = response
            .json()
            .await
            .map_err(|e| Error::InvalidData(format!("failed to parse attachment response: {e}")))?;
        let url = attachments
            .into_iter()
            .next()
            .and_then(|a| a.content)
            .filter(|u| !u.is_empty())
            .ok_or_else(|| {
                Error::InvalidData(
                    "Jira upload returned no attachment with a content URL".to_string(),
                )
            })?;
        Ok(url)
    }

    async fn get_issue_attachments(&self, issue_key: &str) -> Result<Vec<AssetMeta>> {
        let jira_key = parse_jira_key(issue_key);
        let url = format!("{}/issue/{}?fields=attachment", self.base_url, jira_key);
        let issue: JiraIssue = self.get(&url).await?;
        Ok(issue
            .fields
            .attachment
            .iter()
            .map(map_jira_attachment)
            .collect())
    }

    async fn download_attachment(&self, _issue_key: &str, asset_id: &str) -> Result<Vec<u8>> {
        // Cloud: GET /rest/api/3/attachment/content/{id}
        // Self-Hosted: the Cloud endpoint doesn't exist; fetch attachment
        // metadata first and download from its `content` URL.
        let url = match self.flavor {
            JiraFlavor::Cloud => {
                format!("{}/attachment/content/{}", self.base_url, asset_id)
            }
            JiraFlavor::SelfHosted => {
                let meta_url = format!("{}/attachment/{}", self.base_url, asset_id);
                let meta: serde_json::Value = self.get(&meta_url).await?;
                meta.get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        Error::InvalidData(format!(
                            "attachment {asset_id} metadata has no content URL"
                        ))
                    })?
                    .to_string()
            }
        };
        let response = self
            .request(reqwest::Method::GET, &url)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(Error::from_status(status.as_u16(), message));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| Error::Http(format!("failed to read attachment bytes: {e}")))?;
        Ok(bytes.to_vec())
    }

    async fn delete_attachment(&self, _issue_key: &str, asset_id: &str) -> Result<()> {
        // DELETE /rest/api/{v}/attachment/{id} — 204 on success.
        let url = format!("{}/attachment/{}", self.base_url, asset_id);
        let response = self
            .request(reqwest::Method::DELETE, &url)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(Error::from_status(status.as_u16(), message));
        }
        Ok(())
    }

    fn asset_capabilities(&self) -> AssetCapabilities {
        // Jira exposes a full CRUD REST API for attachments on issues.
        AssetCapabilities {
            issue: ContextCapabilities {
                upload: true,
                download: true,
                delete: true,
                list: true,
                max_file_size: None,
                allowed_types: Vec::new(),
            },
            ..Default::default()
        }
    }

    // --- Jira Structure plugin ---

    async fn get_structures(&self) -> Result<ProviderResult<Structure>> {
        let resp: JiraStructureListResponse = self.structure_get("/structure").await?;
        let items: Vec<Structure> = resp
            .structures
            .into_iter()
            .map(|s| Structure {
                id: s.id,
                name: s.name,
                description: s.description,
            })
            .collect();
        Ok(items.into())
    }

    async fn get_structure_forest(
        &self,
        structure_id: u64,
        options: GetForestOptions,
    ) -> Result<StructureForest> {
        let mut spec = serde_json::Map::new();
        if let Some(offset) = options.offset {
            spec.insert("offset".into(), serde_json::json!(offset));
        }
        if let Some(limit) = options.limit {
            spec.insert("limit".into(), serde_json::json!(limit));
        }

        let resp: JiraForestResponse = self
            .structure_post(
                &format!("/forest/{}/spec", structure_id),
                &serde_json::Value::Object(spec),
            )
            .await?;

        let tree = build_forest_tree(&resp.rows, &resp.depths)?;

        Ok(StructureForest {
            version: resp.version,
            structure_id,
            tree,
            total_count: resp.total_count,
        })
    }

    async fn add_structure_rows(
        &self,
        structure_id: u64,
        input: AddStructureRowsInput,
    ) -> Result<ForestModifyResult> {
        let mut payload = serde_json::json!({
            "rows": input.items.iter().map(|i| {
                let mut row = serde_json::json!({"itemId": i.item_id});
                if let Some(ref t) = i.item_type {
                    row["itemType"] = serde_json::json!(t);
                }
                row
            }).collect::<Vec<_>>()
        });
        if let Some(under) = input.under {
            payload["under"] = serde_json::json!(under);
        }
        if let Some(after) = input.after {
            payload["after"] = serde_json::json!(after);
        }
        if let Some(version) = input.forest_version {
            payload["forestVersion"] = serde_json::json!(version);
        }

        let resp: JiraForestModifyResponse = self
            .structure_put(&format!("/forest/{}/item", structure_id), &payload)
            .await
            .map_err(|e| {
                if matches!(&e, Error::Api { status, .. } if *status == 409) {
                    Error::Api {
                        status: 409,
                        message: "Forest version conflict. The structure was modified concurrently. Retry with the latest version.".to_string(),
                    }
                } else {
                    e
                }
            })?;

        Ok(ForestModifyResult {
            version: resp.version,
            affected_count: input.items.len(),
        })
    }

    async fn move_structure_rows(
        &self,
        structure_id: u64,
        input: MoveStructureRowsInput,
    ) -> Result<ForestModifyResult> {
        let mut payload = serde_json::json!({
            "rowIds": input.row_ids
        });
        if let Some(under) = input.under {
            payload["under"] = serde_json::json!(under);
        }
        if let Some(after) = input.after {
            payload["after"] = serde_json::json!(after);
        }
        if let Some(version) = input.forest_version {
            payload["forestVersion"] = serde_json::json!(version);
        }

        let resp: JiraForestModifyResponse = self
            .structure_post(&format!("/forest/{}/move", structure_id), &payload)
            .await
            .map_err(|e| {
                if matches!(&e, Error::Api { status, .. } if *status == 409) {
                    Error::Api {
                        status: 409,
                        message: "Forest version conflict. Retry with the latest version."
                            .to_string(),
                    }
                } else {
                    e
                }
            })?;

        Ok(ForestModifyResult {
            version: resp.version,
            affected_count: input.row_ids.len(),
        })
    }

    async fn remove_structure_row(&self, structure_id: u64, row_id: u64) -> Result<()> {
        self.structure_delete_request(&format!("/forest/{}/item/{}", structure_id, row_id))
            .await
    }

    async fn get_structure_values(
        &self,
        input: GetStructureValuesInput,
    ) -> Result<StructureValues> {
        let columns: Vec<serde_json::Value> = input
            .columns
            .iter()
            .map(|c| {
                let mut col = serde_json::Map::new();
                if let Some(ref id) = c.id {
                    col.insert("id".into(), serde_json::json!(id));
                }
                if let Some(ref field) = c.field {
                    col.insert("field".into(), serde_json::json!(field));
                }
                if let Some(ref formula) = c.formula {
                    col.insert("formula".into(), serde_json::json!(formula));
                }
                serde_json::Value::Object(col)
            })
            .collect();

        let payload = serde_json::json!({
            "structureId": input.structure_id,
            "rows": input.rows,
            "columns": columns,
        });

        let resp: JiraStructureValuesResponse = self.structure_post("/value", &payload).await?;

        // Group values by row_id. A missing `columnId` is treated as
        // an error rather than defaulted to `""` — silently bucketing
        // unknown columns under the empty-string key would merge
        // values from different columns and destroy user data.
        let mut row_map: std::collections::BTreeMap<u64, Vec<StructureColumnValue>> =
            std::collections::BTreeMap::new();
        for entry in resp.values {
            let column = entry.column_id.ok_or_else(|| {
                Error::InvalidData(format!(
                    "Structure value for row {} is missing `columnId`",
                    entry.row_id
                ))
            })?;
            row_map
                .entry(entry.row_id)
                .or_default()
                .push(StructureColumnValue {
                    column,
                    value: entry.value,
                });
        }

        let values = row_map
            .into_iter()
            .map(|(row_id, columns)| StructureRowValues { row_id, columns })
            .collect();

        Ok(StructureValues {
            structure_id: input.structure_id,
            values,
        })
    }

    async fn get_structure_views(
        &self,
        structure_id: u64,
        view_id: Option<u64>,
    ) -> Result<Vec<StructureView>> {
        if let Some(id) = view_id {
            let view: JiraStructureView = self.structure_get(&format!("/view/{}", id)).await?;
            // Validate that the returned view actually belongs to the
            // requested structure — the Structure API's `/view/{id}`
            // endpoint ignores the structure id in the request, so a
            // caller who mixes up ids would otherwise silently see a
            // view from a different structure.
            if view.structure_id != structure_id {
                return Err(Error::InvalidData(format!(
                    "view {id} belongs to structure {} but {structure_id} was requested",
                    view.structure_id
                )));
            }
            Ok(vec![map_structure_view(view)])
        } else {
            let resp: JiraStructureViewListResponse = self
                .structure_get(&format!("/view?structureId={}", structure_id))
                .await?;
            Ok(resp.views.into_iter().map(map_structure_view).collect())
        }
    }

    async fn save_structure_view(&self, input: SaveStructureViewInput) -> Result<StructureView> {
        let columns: Option<Vec<serde_json::Value>> = input.columns.as_ref().map(|cols| {
            cols.iter()
                .map(|c| {
                    let mut col = serde_json::Map::new();
                    if let Some(ref field) = c.field {
                        col.insert("field".into(), serde_json::json!(field));
                    }
                    if let Some(ref formula) = c.formula {
                        col.insert("formula".into(), serde_json::json!(formula));
                    }
                    if let Some(width) = c.width {
                        col.insert("width".into(), serde_json::json!(width));
                    }
                    serde_json::Value::Object(col)
                })
                .collect()
        });

        let mut payload = serde_json::json!({
            "structureId": input.structure_id,
            "name": input.name,
        });
        if let Some(cols) = columns {
            payload["columns"] = serde_json::json!(cols);
        }
        if let Some(ref g) = input.group_by {
            payload["groupBy"] = serde_json::json!(g);
        }
        if let Some(ref s) = input.sort_by {
            payload["sortBy"] = serde_json::json!(s);
        }
        if let Some(ref f) = input.filter {
            payload["filter"] = serde_json::json!(f);
        }

        let view: JiraStructureView = if let Some(id) = input.id {
            self.structure_put(&format!("/view/{}", id), &payload)
                .await?
        } else {
            self.structure_post("/view", &payload).await?
        };

        Ok(map_structure_view(view))
    }

    async fn create_structure(&self, input: CreateStructureInput) -> Result<Structure> {
        let mut payload = serde_json::json!({"name": input.name});
        if let Some(ref desc) = input.description {
            payload["description"] = serde_json::json!(desc);
        }
        let s: JiraStructure = self.structure_post("/structure", &payload).await?;
        Ok(Structure {
            id: s.id,
            name: s.name,
            description: s.description,
        })
    }

    // --- Structure generators (issue #179) -----------------------------

    async fn get_structure_generators(
        &self,
        structure_id: u64,
    ) -> Result<ProviderResult<devboy_core::StructureGenerator>> {
        #[derive(serde::Deserialize)]
        struct Resp {
            #[serde(default)]
            generators: Vec<RawGenerator>,
        }
        #[derive(serde::Deserialize)]
        struct RawGenerator {
            id: String,
            #[serde(rename = "type")]
            generator_type: String,
            #[serde(default)]
            spec: serde_json::Value,
        }
        let resp: Resp = self
            .structure_get(&format!("/structure/{}/generator", structure_id))
            .await?;
        let items: Vec<devboy_core::StructureGenerator> = resp
            .generators
            .into_iter()
            .map(|g| devboy_core::StructureGenerator {
                id: g.id,
                generator_type: g.generator_type,
                spec: g.spec,
            })
            .collect();
        Ok(items.into())
    }

    async fn add_structure_generator(
        &self,
        input: devboy_core::AddStructureGeneratorInput,
    ) -> Result<devboy_core::StructureGenerator> {
        // Typed response so missing `id`/`type` surface as a deserialise
        // error instead of silent empty strings (Copilot review on PR #205).
        #[derive(serde::Deserialize)]
        struct Resp {
            id: String,
            #[serde(rename = "type")]
            generator_type: String,
            #[serde(default)]
            spec: serde_json::Value,
        }
        let body = serde_json::json!({
            "type": input.generator_type,
            "spec": input.spec,
        });
        let resp: Resp = self
            .structure_post(
                &format!("/structure/{}/generator", input.structure_id),
                &body,
            )
            .await?;
        Ok(devboy_core::StructureGenerator {
            id: resp.id,
            generator_type: resp.generator_type,
            spec: resp.spec,
        })
    }

    async fn sync_structure_generator(
        &self,
        input: devboy_core::SyncStructureGeneratorInput,
    ) -> Result<()> {
        let body = serde_json::json!({});
        let _: serde_json::Value = self
            .structure_post(
                &format!(
                    "/structure/{}/generator/{}/sync",
                    input.structure_id, input.generator_id
                ),
                &body,
            )
            .await?;
        Ok(())
    }

    // --- Structure delete + automation (issue #180) --------------------

    async fn delete_structure(&self, structure_id: u64) -> Result<()> {
        self.structure_delete_request(&format!("/structure/{}", structure_id))
            .await
    }

    async fn update_structure_automation(
        &self,
        input: devboy_core::UpdateStructureAutomationInput,
    ) -> Result<()> {
        // `automation_id = Some(id)` → rule-scoped PUT; `None` → replace
        // the whole automation collection (Copilot review on PR #205).
        let endpoint = match input.automation_id.as_deref() {
            Some(aid) => format!("/structure/{}/automation/{}", input.structure_id, aid),
            None => format!("/structure/{}/automation", input.structure_id),
        };
        let _: serde_json::Value = self.structure_put(&endpoint, &input.config).await?;
        Ok(())
    }

    async fn trigger_structure_automation(&self, structure_id: u64) -> Result<()> {
        let body = serde_json::json!({});
        let _: serde_json::Value = self
            .structure_post(
                &format!("/structure/{}/automation/run", structure_id),
                &body,
            )
            .await?;
        Ok(())
    }

    // --- Agile / Sprint (issue #198) -----------------------------------

    async fn get_board_sprints(
        &self,
        board_id: u64,
        state: devboy_core::SprintState,
    ) -> Result<ProviderResult<devboy_core::Sprint>> {
        // Walk Jira Agile pagination (`startAt` + `isLast`) so callers on
        // boards with many closed/future sprints get the full list
        // (Codex review on PR #205).
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Resp {
            #[serde(default)]
            is_last: bool,
            #[serde(default)]
            values: Vec<devboy_core::Sprint>,
        }
        // Cap at 5k sprints — plenty for any realistic board, prevents
        // an infinite loop if `isLast` is ever misreported.
        const MAX_SPRINTS: usize = 5_000;
        const PAGE_SIZE: u32 = 50;

        let state_param = state
            .as_query_value()
            .map(|s| format!("&state={}", s))
            .unwrap_or_default();

        let mut sprints: Vec<devboy_core::Sprint> = Vec::new();
        let mut start_at: u32 = 0;
        loop {
            let endpoint = format!(
                "/board/{}/sprint?startAt={}&maxResults={}{}",
                board_id, start_at, PAGE_SIZE, state_param
            );
            let resp: Resp = self.agile_get(&endpoint).await?;
            let fetched = resp.values.len() as u32;
            sprints.extend(resp.values);
            if resp.is_last || fetched == 0 || sprints.len() >= MAX_SPRINTS {
                break;
            }
            start_at += fetched;
        }
        Ok(sprints.into())
    }

    async fn assign_to_sprint(&self, input: devboy_core::AssignToSprintInput) -> Result<()> {
        // Jira accepts issue keys in the form `["PROJ-1", ...]`. Our
        // provider-normalised keys may carry a `jira#` prefix — strip it.
        let issues: Vec<String> = input
            .issue_keys
            .into_iter()
            .map(|k| parse_jira_key(&k).to_string())
            .collect();
        let body = serde_json::json!({ "issues": issues });
        self.agile_post_void(&format!("/sprint/{}/issue", input.sprint_id), &body)
            .await
    }

    // --- Project versions / fixVersion (issue #238) --------------------

    async fn list_project_versions(
        &self,
        params: ListProjectVersionsParams,
    ) -> Result<ProviderResult<ProjectVersion>> {
        let project_key = if params.project.is_empty() {
            self.project_key.clone()
        } else {
            params.project
        };

        // Both Cloud v3 and Server/DC v2 expose
        // `GET /project/{key}/versions` as an unpaginated list of all
        // versions. The paginated `/version/page` endpoint exists on
        // Cloud only and isn't worth the flavor split — projects with
        // O(10²) versions still fit in one round-trip; we trim the
        // response in-memory below to honour Paper 1's 8k-token cap.
        //
        // `?expand=issuesstatus` is a Cloud-only payload extension —
        // Server/DC ignores it but we still skip the param there so we
        // don't bake hidden flavor-quirk dependencies into the URL.
        let mut url = format!("{}/project/{}/versions", self.base_url, project_key);
        if params.include_issue_count && self.flavor == JiraFlavor::Cloud {
            url.push_str("?expand=issuesstatus");
        }

        let dtos: Vec<JiraVersionDto> = self.get(&url).await?;

        let mut versions: Vec<ProjectVersion> = dtos
            .into_iter()
            .map(|dto| jira_version_to_project_version(dto, &project_key))
            .collect();

        if let Some(want_released) = params.released {
            versions.retain(|v| v.released == want_released);
        }
        if let Some(want_archived) = params.archived {
            versions.retain(|v| v.archived == want_archived);
        }

        // Order (Paper 1 — keep the *current* release at the top, not the
        // most recently shipped one):
        //   1. unreleased before released — work-in-flight beats history;
        //   2. release_date placement depends on the group:
        //      - unreleased: undated *first* (undated == "planned, no
        //        date yet" → still in flight), then dated desc;
        //      - released: dated desc, undated *last* ("released without
        //        a date" usually means unspecified history);
        //   3. semver-numeric tiebreak on name so "10.0.0" beats "9.10.0".
        versions.sort_by(|a, b| {
            use std::cmp::Ordering;
            let group = a.released.cmp(&b.released);
            if group != Ordering::Equal {
                return group;
            }
            // Both `a` and `b` are in the same released/unreleased group,
            // so checking one is enough.
            let undated_first = !a.released;
            let date = match (&a.release_date, &b.release_date) {
                (Some(a_d), Some(b_d)) => b_d.cmp(a_d),
                (None, None) => Ordering::Equal,
                (None, Some(_)) if undated_first => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) if undated_first => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
            };
            date.then_with(|| compare_version_names(&b.name, &a.name))
        });

        let total_after_filter = versions.len() as u32;
        let limit_applied = params.limit.unwrap_or(total_after_filter);
        if (limit_applied as usize) < versions.len() {
            versions.truncate(limit_applied as usize);
        }

        // Pagination carries total + has_more so the formatter can render
        // a "[+N more …]" hint when truncation hid items (Paper 1 §Chunk
        // Index). We start at offset 0 — the list endpoint is unpaginated
        // server-side, all chunking is client-side trimming.
        let pagination = devboy_core::Pagination {
            offset: 0,
            limit: limit_applied,
            total: Some(total_after_filter),
            has_more: (versions.len() as u32) < total_after_filter,
            next_cursor: None,
        };

        Ok(ProviderResult::new(versions).with_pagination(pagination))
    }

    async fn upsert_project_version(
        &self,
        input: UpsertProjectVersionInput,
    ) -> Result<ProjectVersion> {
        let trimmed_name = input.name.trim().to_string();
        if trimmed_name.is_empty() {
            return Err(Error::InvalidData(
                "upsert_project_version: name must not be empty".into(),
            ));
        }
        // Jira limits version names to 255 characters; rejecting client-side
        // gives a clearer error than a late 400 from the server.
        if trimmed_name.chars().count() > 255 {
            return Err(Error::InvalidData(
                "upsert_project_version: name must be ≤ 255 characters".into(),
            ));
        }
        let project_key = if input.project.is_empty() {
            self.project_key.clone()
        } else {
            input.project.clone()
        };

        let update_payload = UpdateVersionPayload {
            name: None,
            description: input.description.clone(),
            start_date: input.start_date.clone(),
            release_date: input.release_date.clone(),
            released: input.released,
            archived: input.archived,
        };
        let create_payload = CreateVersionPayload {
            name: trimmed_name.clone(),
            project: Some(project_key.clone()),
            project_id: None,
            description: input.description,
            start_date: input.start_date,
            release_date: input.release_date,
            released: input.released,
            archived: input.archived,
        };

        // Resolve `(project, name)` → existing id. We list all versions
        // in the project rather than filtering server-side — Jira has no
        // exact-name lookup, and a single project rarely has more than a
        // few hundred versions.
        let list_url = format!("{}/project/{}/versions", self.base_url, project_key);
        let dtos: Vec<JiraVersionDto> = self.get(&list_url).await?;
        let existing = dtos.into_iter().find(|d| d.name == trimmed_name);

        let dto: JiraVersionDto = match existing {
            Some(existing) => {
                self.put_with_response(
                    &format!("{}/version/{}", self.base_url, existing.id),
                    &update_payload,
                )
                .await?
            }
            None => {
                // Create path. Two callers can race here: both miss the
                // version on the initial list and both POST. Jira rejects
                // the loser with a 400 + "already exists" message; we
                // re-list, find the winner's id, and apply the update so
                // the loser still observes a consistent post-condition.
                match self
                    .post::<JiraVersionDto, _>(
                        &format!("{}/version", self.base_url),
                        &create_payload,
                    )
                    .await
                {
                    Ok(dto) => dto,
                    Err(e) if is_duplicate_version_error(&e) => {
                        let dtos: Vec<JiraVersionDto> = self.get(&list_url).await?;
                        let recovered = dtos
                            .into_iter()
                            .find(|d| d.name == trimmed_name)
                            .ok_or_else(|| {
                                Error::InvalidData(format!(
                                    "upsert_project_version: create rejected as duplicate but version '{trimmed_name}' is not in the project list"
                                ))
                            })?;
                        self.put_with_response(
                            &format!("{}/version/{}", self.base_url, recovered.id),
                            &update_payload,
                        )
                        .await?
                    }
                    Err(e) => return Err(e),
                }
            }
        };

        Ok(jira_version_to_project_version(dto, &project_key))
    }

    fn provider_name(&self) -> &'static str {
        "jira"
    }
}

/// Map the raw Jira version DTO to the provider-agnostic [`ProjectVersion`].
///
/// `project_fallback` covers DTOs returned by `POST /version` on some
/// Server/DC builds where the `project` key is omitted — we fall back to
/// the key the caller addressed.
/// True when the error returned by `POST /version` indicates Jira
/// rejected the create because a version with that name already exists
/// in the project. Both Cloud v3 and Server/DC v2 surface this as a 400
/// with the phrase "already exists" in the response body.
fn is_duplicate_version_error(e: &Error) -> bool {
    let lowered = e.to_string().to_lowercase();
    lowered.contains("already exists") || lowered.contains("already used")
}

/// Compare two Jira version *names* with semver-aware ordering.
///
/// Splits each name into runs of digits and runs of non-digits and
/// compares them piecewise — digit runs numerically (so `10` > `9`),
/// non-digit runs lexicographically (so `1.0.0-rc1` < `1.0.0`). Falls
/// back to plain string compare when either side has no digits, which
/// keeps non-semver release names (e.g. `"Sprint 42 cleanup"`) stable.
fn compare_version_names(a: &str, b: &str) -> std::cmp::Ordering {
    fn tokens(s: &str) -> Vec<(bool, &str)> {
        let mut out = Vec::new();
        let mut start = 0;
        let mut last_digit: Option<bool> = None;
        for (i, ch) in s.char_indices() {
            let is_digit = ch.is_ascii_digit();
            match last_digit {
                Some(prev) if prev != is_digit => {
                    out.push((prev, &s[start..i]));
                    start = i;
                }
                _ => {}
            }
            last_digit = Some(is_digit);
        }
        if let Some(prev) = last_digit {
            out.push((prev, &s[start..]));
        }
        out
    }

    let a_toks = tokens(a);
    let b_toks = tokens(b);
    for (ax, bx) in a_toks.iter().zip(b_toks.iter()) {
        let cmp = match (ax, bx) {
            ((true, ad), (true, bd)) => {
                // Numeric token compare — strip leading zeros, then by
                // length, then lexicographically as a tiebreak.
                let an = ad.trim_start_matches('0');
                let bn = bd.trim_start_matches('0');
                an.len().cmp(&bn.len()).then_with(|| an.cmp(bn))
            }
            ((false, at), (false, bt)) => at.cmp(bt),
            // Numeric runs sort *after* alpha runs at the same position
            // — this matches semver's rule that `1.0.0-rc1 < 1.0.0`.
            ((true, _), (false, _)) => std::cmp::Ordering::Greater,
            ((false, _), (true, _)) => std::cmp::Ordering::Less,
        };
        if cmp != std::cmp::Ordering::Equal {
            return cmp;
        }
    }
    // Equal token-by-token up to the shorter side. SemVer treats a
    // pre-release suffix as *lower* than the bare version, so when one
    // side has more tokens *and* the next token starts with `-` (or
    // `+` build metadata), the longer side is considered smaller.
    match a_toks.len().cmp(&b_toks.len()) {
        std::cmp::Ordering::Equal => std::cmp::Ordering::Equal,
        std::cmp::Ordering::Greater => {
            let next = a_toks[b_toks.len()].1;
            if next.starts_with('-') || next.starts_with('+') {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            }
        }
        std::cmp::Ordering::Less => {
            let next = b_toks[a_toks.len()].1;
            if next.starts_with('-') || next.starts_with('+') {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Less
            }
        }
    }
}

fn jira_version_to_project_version(dto: JiraVersionDto, project_fallback: &str) -> ProjectVersion {
    // Cloud `?expand=issuesstatus` returns a per-category breakdown we
    // sum into a true `issue_count` total. Server/DC inlines
    // `issuesUnresolvedCount` on the base payload but *not* a total, so
    // we route that into `unresolved_issue_count` and leave
    // `issue_count` unset there — conflating the two would let callers
    // compare a Cloud total against a Server unresolved count and not
    // notice the categorical mismatch.
    let issue_count = dto
        .issues_status_for_fix_version
        .as_ref()
        .map(|c| c.total());
    let unresolved_issue_count = dto.issues_unresolved_count;

    ProjectVersion {
        id: dto.id,
        project: dto.project.unwrap_or_else(|| project_fallback.to_string()),
        name: dto.name,
        description: dto.description.filter(|d| !d.is_empty()),
        start_date: dto.start_date.filter(|d| !d.is_empty()),
        release_date: dto.release_date.filter(|d| !d.is_empty()),
        released: dto.released,
        archived: dto.archived,
        overdue: dto.overdue,
        issue_count,
        unresolved_issue_count,
        source: "jira".to_string(),
    }
}

#[async_trait]
impl MergeRequestProvider for JiraClient {
    fn provider_name(&self) -> &'static str {
        "jira"
    }
}

#[async_trait]
impl PipelineProvider for JiraClient {
    fn provider_name(&self) -> &'static str {
        "jira"
    }
}

#[async_trait]
impl Provider for JiraClient {
    async fn get_current_user(&self) -> Result<User> {
        let url = format!("{}/myself", self.base_url);
        let jira_user: JiraUser = self.get(&url).await?;
        Ok(map_user(Some(&jira_user)).unwrap_or_default())
    }
}

// Issue #177 — UserProvider. Jira exposes user lookup via /user?accountId
// (Cloud) or /user?username (Self-Hosted); email lookup uses /user/search.
#[async_trait]
impl devboy_core::UserProvider for JiraClient {
    fn provider_name(&self) -> &'static str {
        "jira"
    }

    async fn get_user_profile(&self, user_id: &str) -> Result<User> {
        let url = match self.flavor {
            JiraFlavor::Cloud => format!("{}/user?accountId={}", self.base_url, user_id),
            JiraFlavor::SelfHosted => format!("{}/user?username={}", self.base_url, user_id),
        };
        let jira_user: JiraUser = self.get(&url).await?;
        map_user(Some(&jira_user))
            .ok_or_else(|| Error::InvalidData("Jira /user returned no user".to_string()))
    }

    async fn lookup_user_by_email(&self, email: &str) -> Result<Option<User>> {
        // /user/search accepts `query=` on Cloud (searches display name /
        // email) and `username=` on Self-Hosted. Email is the more useful
        // parameter for cross-provider correlation.
        let url = match self.flavor {
            JiraFlavor::Cloud => format!("{}/user/search?query={}", self.base_url, email),
            JiraFlavor::SelfHosted => {
                format!("{}/user/search?username={}", self.base_url, email)
            }
        };
        let users: Vec<JiraUser> = self.get(&url).await?;
        Ok(users.into_iter().find_map(|u| map_user(Some(&u))))
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use devboy_core::{CreateCommentInput, MrFilter};

    fn token(s: &str) -> SecretString {
        SecretString::from(s.to_string())
    }

    // =========================================================================
    // Structure error mapping tests
    // =========================================================================

    #[test]
    fn structure_install_hint_is_single_well_spaced_line() {
        // Guard against the previous `"... \` with-indent multi-line literal
        // which baked spurious inner whitespace into the error text.
        assert!(
            !STRUCTURE_PLUGIN_HINT.contains("  "),
            "hint contains consecutive spaces: {STRUCTURE_PLUGIN_HINT:?}"
        );
        assert!(!STRUCTURE_PLUGIN_HINT.contains('\n'));
        assert!(STRUCTURE_PLUGIN_HINT.contains("marketplace.atlassian.com"));
    }

    #[test]
    fn structure_404_with_html_returns_soft_endpoint_hint() {
        let html = "<!DOCTYPE html><html><body>Oops, you&#39;ve found a dead link.</body></html>";
        let err = structure_error_from_status(404, "text/html;charset=UTF-8", html.into());
        let msg = err.to_string();
        assert!(!msg.contains("<!DOCTYPE"), "HTML leaked into error: {msg}");
        // Wording must be soft — the same 404+markup signal can fire if the
        // plugin IS installed but the endpoint path was renamed/removed.
        assert!(
            msg.contains("endpoint not found"),
            "expected soft 'endpoint not found' wording: {msg}"
        );
        assert!(
            msg.contains("may not be installed"),
            "expected soft install-hint wording: {msg}"
        );
        assert!(
            msg.contains("marketplace.atlassian.com"),
            "missing marketplace link: {msg}"
        );
    }

    #[test]
    fn structure_500_with_html_strips_body() {
        let html = "<html><body>".to_string() + &"x".repeat(20_000) + "</body></html>";
        let err = structure_error_from_status(500, "text/html", html);
        let msg = err.to_string();
        assert!(
            !msg.contains("xxxx"),
            "raw HTML body leaked: {}",
            &msg[..msg.len().min(400)]
        );
        assert!(
            msg.contains("non-JSON"),
            "missing short status message: {msg}"
        );
    }

    #[test]
    fn structure_json_error_is_forwarded_verbatim() {
        let body = r#"{"errorMessages":["Invalid forestVersion"],"errors":{}}"#;
        let err = structure_error_from_status(409, "application/json", body.into());
        let msg = err.to_string();
        assert!(
            msg.contains("Invalid forestVersion"),
            "JSON body dropped: {msg}"
        );
    }

    #[test]
    fn structure_long_text_body_is_truncated() {
        let body = "plain text ".repeat(200); // > 500 chars
        let err = structure_error_from_status(400, "text/plain", body);
        let msg = err.to_string();
        assert!(
            msg.contains("truncated"),
            "truncation marker missing: {msg}"
        );
    }

    #[test]
    fn structure_html_detected_by_body_when_content_type_missing() {
        assert!(looks_like_html("", "<!DOCTYPE html><html>..."));
        assert!(looks_like_html("", "<html lang=\"en\">"));
        assert!(!looks_like_html("", "   {\"ok\":true}"));
        assert!(!looks_like_html("application/json", "{\"ok\":true}"));
    }

    #[test]
    fn structure_html_detected_by_content_type_only() {
        assert!(looks_like_html("text/html; charset=UTF-8", ""));
        assert!(looks_like_html("Text/HTML", ""));
    }

    #[test]
    fn structure_xml_body_treated_as_non_json() {
        // Structure plugin returns XML 404 for unknown subpaths
        let xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><status><status-code>404</status-code><message>null for uri: /rest/structure/2.0/view?structureId=1</message></status>"#;
        assert!(looks_like_html("application/xml", xml));
        assert!(looks_like_html("", xml));
        let err = structure_error_from_status(404, "application/xml", xml.into());
        let msg = err.to_string();
        assert!(!msg.contains("<?xml"), "XML leaked into error: {msg}");
        assert!(
            msg.contains("endpoint not found"),
            "expected soft wording: {msg}"
        );
    }

    #[test]
    fn structure_parse_preview_redacts_html_body() {
        let html = r#"<!DOCTYPE html><html><head><title>Login</title></head><body><form>…</form></body></html>"#;
        let preview = structure_parse_preview("text/html; charset=UTF-8", html);
        assert!(
            !preview.contains("<!DOCTYPE"),
            "HTML leaked into parse preview: {preview}"
        );
        assert!(
            !preview.contains("<html"),
            "HTML leaked into parse preview: {preview}"
        );
        assert!(
            preview.contains("redacted"),
            "expected redaction marker: {preview}"
        );
        assert!(
            preview.contains(&format!("{}", html.len())),
            "expected byte count in preview: {preview}"
        );
    }

    #[test]
    fn structure_parse_preview_redacts_xml_body() {
        let xml = r#"<?xml version="1.0"?><status><code>200</code></status>"#;
        let preview = structure_parse_preview("application/xml", xml);
        assert!(!preview.contains("<?xml"), "XML leaked: {preview}");
        assert!(preview.contains("redacted"));
    }

    #[test]
    fn structure_parse_preview_keeps_short_json_body_verbatim() {
        let body = r#"{"broken":"response"#; // missing closing brace
        let preview = structure_parse_preview("application/json", body);
        assert_eq!(preview, body);
    }

    #[test]
    fn structure_parse_preview_truncates_long_non_markup_body() {
        let body = "a".repeat(2000);
        let preview = structure_parse_preview("text/plain", &body);
        assert!(preview.contains("truncated"));
        assert!(preview.len() < body.len());
    }

    // =========================================================================
    // Flavor detection tests
    // =========================================================================

    #[test]
    fn test_flavor_detection_cloud() {
        assert_eq!(
            detect_flavor("https://company.atlassian.net"),
            JiraFlavor::Cloud
        );
        assert_eq!(
            detect_flavor("https://myorg.atlassian.net/"),
            JiraFlavor::Cloud
        );
    }

    #[test]
    fn test_flavor_detection_self_hosted() {
        assert_eq!(
            detect_flavor("https://jira.company.com"),
            JiraFlavor::SelfHosted
        );
        assert_eq!(
            detect_flavor("https://jira.corp.internal"),
            JiraFlavor::SelfHosted
        );
        assert_eq!(
            detect_flavor("http://localhost:8080"),
            JiraFlavor::SelfHosted
        );
    }

    // =========================================================================
    // API URL tests
    // =========================================================================

    #[test]
    fn test_api_url_cloud() {
        assert_eq!(
            build_api_base("https://company.atlassian.net", JiraFlavor::Cloud),
            "https://company.atlassian.net/rest/api/3"
        );
    }

    #[test]
    fn test_api_url_self_hosted() {
        assert_eq!(
            build_api_base("https://jira.company.com", JiraFlavor::SelfHosted),
            "https://jira.company.com/rest/api/2"
        );
    }

    #[test]
    fn test_api_url_strips_trailing_slash() {
        assert_eq!(
            build_api_base("https://company.atlassian.net/", JiraFlavor::Cloud),
            "https://company.atlassian.net/rest/api/3"
        );
    }

    // =========================================================================
    // Auth header tests
    // =========================================================================

    #[test]
    fn test_auth_header_cloud() {
        let client = JiraClient::with_base_url(
            "http://localhost",
            "PROJ",
            "user@example.com",
            token("api-token-123"),
            true,
        );
        // Cloud uses Basic auth with email:token
        let expected = base64_encode("user@example.com:api-token-123");
        let req = client.request(reqwest::Method::GET, "http://localhost/test");
        let built = req.build().unwrap();
        let auth = built
            .headers()
            .get("Authorization")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(auth, format!("Basic {}", expected));
    }

    #[test]
    fn test_auth_header_self_hosted_bearer() {
        let client = JiraClient::with_base_url(
            "http://localhost",
            "PROJ",
            "user@example.com",
            token("personal-access-token"),
            false,
        );
        let req = client.request(reqwest::Method::GET, "http://localhost/test");
        let built = req.build().unwrap();
        let auth = built
            .headers()
            .get("Authorization")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(auth, "Bearer personal-access-token");
    }

    #[test]
    fn test_auth_header_self_hosted_basic() {
        let client = JiraClient::with_base_url(
            "http://localhost",
            "PROJ",
            "user@example.com",
            token("user:password"),
            false,
        );
        let expected = base64_encode("user:password");
        let req = client.request(reqwest::Method::GET, "http://localhost/test");
        let built = req.build().unwrap();
        let auth = built
            .headers()
            .get("Authorization")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(auth, format!("Basic {}", expected));
    }

    // =========================================================================
    // Base64 encoding tests
    // =========================================================================

    #[test]
    fn test_base64_encode() {
        assert_eq!(base64_encode("hello"), "aGVsbG8=");
        assert_eq!(base64_encode("user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64_encode(""), "");
        assert_eq!(base64_encode("a"), "YQ==");
        assert_eq!(base64_encode("ab"), "YWI=");
        assert_eq!(base64_encode("abc"), "YWJj");
    }

    // =========================================================================
    // ADF conversion tests
    // =========================================================================

    #[test]
    fn test_text_to_adf_simple() {
        let adf = text_to_adf("Hello world");
        assert_eq!(adf["type"], "doc");
        assert_eq!(adf["version"], 1);
        let content = adf["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "paragraph");
        let inline = content[0]["content"].as_array().unwrap();
        assert_eq!(inline.len(), 1);
        assert_eq!(inline[0]["text"], "Hello world");
    }

    #[test]
    fn test_text_to_adf_multi_paragraph() {
        let adf = text_to_adf("First paragraph\n\nSecond paragraph");
        let content = adf["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["content"][0]["text"], "First paragraph");
        assert_eq!(content[1]["content"][0]["text"], "Second paragraph");
    }

    #[test]
    fn test_text_to_adf_with_line_breaks() {
        let adf = text_to_adf("Line 1\nLine 2\nLine 3");
        let content = adf["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        let inline = content[0]["content"].as_array().unwrap();
        // text, hardBreak, text, hardBreak, text = 5 nodes
        assert_eq!(inline.len(), 5);
        assert_eq!(inline[0]["text"], "Line 1");
        assert_eq!(inline[1]["type"], "hardBreak");
        assert_eq!(inline[2]["text"], "Line 2");
        assert_eq!(inline[3]["type"], "hardBreak");
        assert_eq!(inline[4]["text"], "Line 3");
    }

    #[test]
    fn test_text_to_adf_empty() {
        let adf = text_to_adf("");
        assert_eq!(adf["type"], "doc");
        let content = adf["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "paragraph");
        assert!(content[0]["content"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_adf_to_text_simple() {
        let adf = serde_json::json!({
            "version": 1,
            "type": "doc",
            "content": [{
                "type": "paragraph",
                "content": [{
                    "type": "text",
                    "text": "Hello world"
                }]
            }]
        });
        assert_eq!(adf_to_text(&adf), "Hello world");
    }

    #[test]
    fn test_adf_to_text_multi() {
        let adf = serde_json::json!({
            "version": 1,
            "type": "doc",
            "content": [
                {
                    "type": "paragraph",
                    "content": [{
                        "type": "text",
                        "text": "First"
                    }]
                },
                {
                    "type": "paragraph",
                    "content": [{
                        "type": "text",
                        "text": "Second"
                    }]
                }
            ]
        });
        assert_eq!(adf_to_text(&adf), "First\n\nSecond");
    }

    #[test]
    fn test_adf_to_text_with_hardbreak() {
        let adf = serde_json::json!({
            "version": 1,
            "type": "doc",
            "content": [{
                "type": "paragraph",
                "content": [
                    {"type": "text", "text": "Line 1"},
                    {"type": "hardBreak"},
                    {"type": "text", "text": "Line 2"}
                ]
            }]
        });
        assert_eq!(adf_to_text(&adf), "Line 1\nLine 2");
    }

    #[test]
    fn test_adf_to_text_empty() {
        let adf = serde_json::json!({
            "version": 1,
            "type": "doc",
            "content": []
        });
        assert_eq!(adf_to_text(&adf), "");
    }

    #[test]
    fn test_adf_to_text_non_adf_string() {
        let value = serde_json::Value::String("plain text".to_string());
        assert_eq!(adf_to_text(&value), "plain text");
    }

    #[test]
    fn test_adf_to_text_null() {
        assert_eq!(adf_to_text(&serde_json::Value::Null), "");
    }

    // =========================================================================
    // Mapping tests
    // =========================================================================

    fn sample_jira_user_cloud() -> JiraUser {
        JiraUser {
            account_id: Some("5b10a2844c20165700ede21g".to_string()),
            name: None,
            display_name: Some("John Doe".to_string()),
            email_address: Some("john@example.com".to_string()),
        }
    }

    fn sample_jira_user_self_hosted() -> JiraUser {
        JiraUser {
            account_id: None,
            name: Some("jdoe".to_string()),
            display_name: Some("John Doe".to_string()),
            email_address: Some("john@example.com".to_string()),
        }
    }

    #[test]
    fn test_map_user_cloud() {
        let user = map_user(Some(&sample_jira_user_cloud())).unwrap();
        assert_eq!(user.id, "5b10a2844c20165700ede21g");
        assert_eq!(user.username, "5b10a2844c20165700ede21g");
        assert_eq!(user.name, Some("John Doe".to_string()));
        assert_eq!(user.email, Some("john@example.com".to_string()));
    }

    #[test]
    fn test_map_user_self_hosted() {
        let user = map_user(Some(&sample_jira_user_self_hosted())).unwrap();
        assert_eq!(user.id, "jdoe");
        assert_eq!(user.username, "jdoe");
        assert_eq!(user.name, Some("John Doe".to_string()));
    }

    #[test]
    fn test_map_user_none() {
        assert!(map_user(None).is_none());
    }

    #[test]
    fn test_map_priority() {
        let make_priority = |name: &str| JiraPriority {
            name: name.to_string(),
        };

        assert_eq!(
            map_priority(Some(&make_priority("Highest"))),
            Some("urgent".to_string())
        );
        assert_eq!(
            map_priority(Some(&make_priority("High"))),
            Some("high".to_string())
        );
        assert_eq!(
            map_priority(Some(&make_priority("Medium"))),
            Some("normal".to_string())
        );
        assert_eq!(
            map_priority(Some(&make_priority("Low"))),
            Some("low".to_string())
        );
        assert_eq!(
            map_priority(Some(&make_priority("Lowest"))),
            Some("low".to_string())
        );
        assert_eq!(
            map_priority(Some(&make_priority("Blocker"))),
            Some("urgent".to_string())
        );
        assert_eq!(map_priority(None), None);
    }

    #[test]
    fn test_map_issue() {
        let issue = JiraIssue {
            id: "10001".to_string(),
            key: "PROJ-123".to_string(),
            fields: JiraIssueFields {
                summary: Some("Fix login bug".to_string()),
                description: Some(serde_json::Value::String(
                    "Login fails on mobile".to_string(),
                )),
                status: Some(JiraStatus {
                    name: "In Progress".to_string(),
                    status_category: None,
                }),
                priority: Some(JiraPriority {
                    name: "High".to_string(),
                }),
                assignee: Some(sample_jira_user_self_hosted()),
                reporter: Some(JiraUser {
                    account_id: None,
                    name: Some("reporter".to_string()),
                    display_name: Some("Reporter".to_string()),
                    email_address: None,
                }),
                labels: vec!["bug".to_string(), "mobile".to_string()],
                created: Some("2024-01-01T10:00:00.000+0000".to_string()),
                updated: Some("2024-01-02T15:30:00.000+0000".to_string()),
                parent: None,
                subtasks: vec![],
                issuelinks: vec![],
                attachment: vec![],
            },
        };

        let mapped = map_issue(&issue, JiraFlavor::SelfHosted, "https://jira.example.com");
        assert_eq!(mapped.key, "jira#PROJ-123");
        assert_eq!(mapped.title, "Fix login bug");
        assert_eq!(
            mapped.description,
            Some("Login fails on mobile".to_string())
        );
        assert_eq!(mapped.state, "In Progress");
        assert_eq!(mapped.source, "jira");
        assert_eq!(mapped.priority, Some("high".to_string()));
        assert_eq!(mapped.labels, vec!["bug", "mobile"]);
        assert_eq!(mapped.assignees.len(), 1);
        assert_eq!(mapped.assignees[0].username, "jdoe");
        assert!(mapped.author.is_some());
        assert_eq!(mapped.author.unwrap().username, "reporter");
        assert_eq!(
            mapped.url,
            Some("https://jira.example.com/browse/PROJ-123".to_string())
        );
        assert_eq!(
            mapped.created_at,
            Some("2024-01-01T10:00:00.000+0000".to_string())
        );
    }

    #[test]
    fn test_map_issue_cloud_adf_description() {
        let adf_desc = serde_json::json!({
            "version": 1,
            "type": "doc",
            "content": [{
                "type": "paragraph",
                "content": [{
                    "type": "text",
                    "text": "ADF description"
                }]
            }]
        });

        let issue = JiraIssue {
            id: "10001".to_string(),
            key: "PROJ-1".to_string(),
            fields: JiraIssueFields {
                summary: Some("Test".to_string()),
                description: Some(adf_desc),
                status: None,
                priority: None,
                assignee: None,
                reporter: None,
                labels: vec![],
                created: None,
                updated: None,
                parent: None,
                subtasks: vec![],
                issuelinks: vec![],
                attachment: vec![],
            },
        };

        let mapped = map_issue(&issue, JiraFlavor::Cloud, "https://test.atlassian.net");
        assert_eq!(mapped.description, Some("ADF description".to_string()));
    }

    #[test]
    fn test_map_issue_self_hosted_plain_description() {
        let issue = JiraIssue {
            id: "10001".to_string(),
            key: "PROJ-1".to_string(),
            fields: JiraIssueFields {
                summary: Some("Test".to_string()),
                description: Some(serde_json::Value::String("Plain text desc".to_string())),
                status: None,
                priority: None,
                assignee: None,
                reporter: None,
                labels: vec![],
                created: None,
                updated: None,
                parent: None,
                subtasks: vec![],
                issuelinks: vec![],
                attachment: vec![],
            },
        };

        let mapped = map_issue(&issue, JiraFlavor::SelfHosted, "https://jira.example.com");
        assert_eq!(mapped.description, Some("Plain text desc".to_string()));
    }

    #[test]
    fn test_map_comment() {
        let comment = JiraComment {
            id: "100".to_string(),
            body: Some(serde_json::Value::String("Nice work!".to_string())),
            author: Some(sample_jira_user_self_hosted()),
            created: Some("2024-01-01T10:00:00.000+0000".to_string()),
            updated: Some("2024-01-01T11:00:00.000+0000".to_string()),
        };

        let mapped = map_comment(&comment, JiraFlavor::SelfHosted);
        assert_eq!(mapped.id, "100");
        assert_eq!(mapped.body, "Nice work!");
        assert!(mapped.author.is_some());
        assert_eq!(mapped.author.unwrap().username, "jdoe");
    }

    #[test]
    fn test_map_comment_cloud_adf() {
        let adf_body = serde_json::json!({
            "version": 1,
            "type": "doc",
            "content": [{
                "type": "paragraph",
                "content": [{
                    "type": "text",
                    "text": "ADF comment"
                }]
            }]
        });

        let comment = JiraComment {
            id: "200".to_string(),
            body: Some(adf_body),
            author: None,
            created: None,
            updated: None,
        };

        let mapped = map_comment(&comment, JiraFlavor::Cloud);
        assert_eq!(mapped.body, "ADF comment");
    }

    // =========================================================================
    // Provider name test
    // =========================================================================

    #[test]
    fn test_provider_name() {
        let client = JiraClient::with_base_url(
            "http://localhost",
            "PROJ",
            "user@example.com",
            token("token"),
            false,
        );
        assert_eq!(IssueProvider::provider_name(&client), "jira");
        assert_eq!(MergeRequestProvider::provider_name(&client), "jira");
    }

    // =========================================================================
    // Priority mapping tests
    // =========================================================================

    #[test]
    fn test_generic_status_to_category() {
        // done category
        assert_eq!(generic_status_to_category("closed"), Some("done"));
        assert_eq!(generic_status_to_category("done"), Some("done"));
        assert_eq!(generic_status_to_category("resolved"), Some("done"));
        assert_eq!(generic_status_to_category("canceled"), Some("done"));
        assert_eq!(generic_status_to_category("cancelled"), Some("done"));
        assert_eq!(generic_status_to_category("CLOSED"), Some("done"));

        // new category
        assert_eq!(generic_status_to_category("open"), Some("new"));
        assert_eq!(generic_status_to_category("new"), Some("new"));
        assert_eq!(generic_status_to_category("todo"), Some("new"));
        assert_eq!(generic_status_to_category("to do"), Some("new"));
        assert_eq!(generic_status_to_category("reopen"), Some("new"));
        assert_eq!(generic_status_to_category("reopened"), Some("new"));

        // indeterminate category
        assert_eq!(
            generic_status_to_category("in_progress"),
            Some("indeterminate")
        );
        assert_eq!(
            generic_status_to_category("in progress"),
            Some("indeterminate")
        );
        assert_eq!(
            generic_status_to_category("in-progress"),
            Some("indeterminate")
        );

        // unknown
        assert_eq!(generic_status_to_category("custom status"), None);
        assert_eq!(generic_status_to_category("review"), None);
    }

    #[test]
    fn test_priority_to_jira() {
        assert_eq!(priority_to_jira("urgent"), "Highest");
        assert_eq!(priority_to_jira("high"), "High");
        assert_eq!(priority_to_jira("normal"), "Medium");
        assert_eq!(priority_to_jira("low"), "Low");
        assert_eq!(priority_to_jira("custom"), "custom");
    }

    // =========================================================================
    // Instance URL extraction test
    // =========================================================================

    #[test]
    fn test_instance_url_from_base() {
        assert_eq!(
            instance_url_from_base("https://company.atlassian.net/rest/api/3"),
            "https://company.atlassian.net"
        );
        assert_eq!(
            instance_url_from_base("https://jira.corp.com/rest/api/2"),
            "https://jira.corp.com"
        );
        assert_eq!(
            instance_url_from_base("http://localhost:8080"),
            "http://localhost:8080"
        );
    }

    // =========================================================================
    // Integration tests with httpmock
    // =========================================================================

    mod integration {
        use super::*;
        use httpmock::prelude::*;

        fn token(s: &str) -> SecretString {
            SecretString::from(s.to_string())
        }

        fn create_self_hosted_client(server: &MockServer) -> JiraClient {
            JiraClient::with_base_url(
                server.base_url(),
                "PROJ",
                "user@example.com",
                token("pat-token"),
                false,
            )
        }

        fn create_cloud_client(server: &MockServer) -> JiraClient {
            JiraClient::with_base_url(
                server.base_url(),
                "PROJ",
                "user@example.com",
                token("api-token"),
                true,
            )
        }

        fn sample_issue_json() -> serde_json::Value {
            serde_json::json!({
                "id": "10001",
                "key": "PROJ-1",
                "fields": {
                    "summary": "Fix login bug",
                    "description": "Login fails on mobile",
                    "status": {"name": "Open"},
                    "priority": {"name": "High"},
                    "assignee": {
                        "name": "jdoe",
                        "displayName": "John Doe",
                        "emailAddress": "john@example.com"
                    },
                    "reporter": {
                        "name": "reporter",
                        "displayName": "Reporter"
                    },
                    "labels": ["bug"],
                    "created": "2024-01-01T10:00:00.000+0000",
                    "updated": "2024-01-02T15:30:00.000+0000"
                }
            })
        }

        fn sample_cloud_issue_json() -> serde_json::Value {
            serde_json::json!({
                "id": "10001",
                "key": "PROJ-1",
                "fields": {
                    "summary": "Fix login bug",
                    "description": {
                        "version": 1,
                        "type": "doc",
                        "content": [{
                            "type": "paragraph",
                            "content": [{
                                "type": "text",
                                "text": "Login fails on mobile"
                            }]
                        }]
                    },
                    "status": {"name": "Open"},
                    "priority": {"name": "High"},
                    "assignee": {
                        "accountId": "5b10a2844c20165700ede21g",
                        "displayName": "John Doe",
                        "emailAddress": "john@example.com"
                    },
                    "reporter": {
                        "accountId": "5b10a284reporter",
                        "displayName": "Reporter"
                    },
                    "labels": ["bug"],
                    "created": "2024-01-01T10:00:00.000+0000",
                    "updated": "2024-01-02T15:30:00.000+0000"
                }
            })
        }

        // =================================================================
        // Self-Hosted (API v2) tests
        // =================================================================

        #[tokio::test]
        async fn test_get_issues() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/search").query_param_exists("jql");
                then.status(200).json_body(serde_json::json!({
                    "issues": [sample_issue_json()],
                    "startAt": 0,
                    "maxResults": 20,
                    "total": 1
                }));
            });

            let client = create_self_hosted_client(&server);
            let issues = client
                .get_issues(IssueFilter::default())
                .await
                .unwrap()
                .items;

            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].key, "jira#PROJ-1");
            assert_eq!(issues[0].title, "Fix login bug");
            assert_eq!(issues[0].source, "jira");
            assert_eq!(issues[0].priority, Some("high".to_string()));
            assert_eq!(
                issues[0].description,
                Some("Login fails on mobile".to_string())
            );
        }

        #[tokio::test]
        async fn test_get_issues_with_filters() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/search")
                    .query_param_includes("jql", "labels = \"bug\"")
                    .query_param_includes("jql", "assignee = \"jdoe\"");
                then.status(200).json_body(serde_json::json!({
                    "issues": [sample_issue_json()],
                    "startAt": 0,
                    "maxResults": 20,
                    "total": 1
                }));
            });

            let client = create_self_hosted_client(&server);
            let issues = client
                .get_issues(IssueFilter {
                    labels: Some(vec!["bug".to_string()]),
                    assignee: Some("jdoe".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;

            assert_eq!(issues.len(), 1);
        }

        #[tokio::test]
        async fn test_get_issues_pagination() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/search")
                    .query_param("startAt", "5")
                    .query_param("maxResults", "10");
                then.status(200).json_body(serde_json::json!({
                    "issues": [sample_issue_json()],
                    "startAt": 5,
                    "maxResults": 10,
                    "total": 20
                }));
            });

            let client = create_self_hosted_client(&server);
            let issues = client
                .get_issues(IssueFilter {
                    offset: Some(5),
                    limit: Some(10),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;

            assert_eq!(issues.len(), 1);
        }

        #[tokio::test]
        async fn test_get_issues_project_key_override() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/search")
                    .query_param_includes("jql", "project = \"OTHER\"");
                then.status(200).json_body(serde_json::json!({
                    "issues": [sample_issue_json()],
                    "startAt": 0,
                    "maxResults": 20,
                    "total": 1
                }));
            });

            let client = create_self_hosted_client(&server);
            let issues = client
                .get_issues(IssueFilter {
                    project_key: Some("OTHER".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;

            assert_eq!(issues.len(), 1);
        }

        #[tokio::test]
        async fn test_get_issues_native_query_passthrough() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/search")
                    .query_param_includes("jql", "project = \"CUSTOM\" AND fixVersion = \"1.0\"");
                then.status(200).json_body(serde_json::json!({
                    "issues": [sample_issue_json()],
                    "startAt": 0,
                    "maxResults": 20,
                    "total": 1
                }));
            });

            let client = create_self_hosted_client(&server);
            let issues = client
                .get_issues(IssueFilter {
                    native_query: Some("project = \"CUSTOM\" AND fixVersion = \"1.0\"".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;

            assert_eq!(issues.len(), 1);
        }

        #[tokio::test]
        async fn test_get_issues_native_query_auto_injects_project() {
            let server = MockServer::start();

            // Client is configured with project_key = "PROJ", native_query has no project clause
            // → should auto-prepend project = "PROJ"
            server.mock(|when, then| {
                when.method(GET)
                    .path("/search")
                    .query_param_includes("jql", "project = \"PROJ\" AND fixVersion = \"2.0\"");
                then.status(200).json_body(serde_json::json!({
                    "issues": [sample_issue_json()],
                    "startAt": 0,
                    "maxResults": 20,
                    "total": 1
                }));
            });

            let client = create_self_hosted_client(&server);
            let issues = client
                .get_issues(IssueFilter {
                    native_query: Some("fixVersion = \"2.0\"".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;

            assert_eq!(issues.len(), 1);
        }

        #[tokio::test]
        async fn test_get_issues_native_query_with_project_in() {
            let server = MockServer::start();

            // Native query already has "project IN (...)" — should NOT prepend another project clause
            server.mock(|when, then| {
                when.method(GET)
                    .path("/search")
                    .query_param_includes("jql", "project IN (\"A\", \"B\") AND status = \"Open\"");
                then.status(200).json_body(serde_json::json!({
                    "issues": [sample_issue_json()],
                    "startAt": 0,
                    "maxResults": 20,
                    "total": 1
                }));
            });

            let client = create_self_hosted_client(&server);
            let issues = client
                .get_issues(IssueFilter {
                    native_query: Some(
                        "project IN (\"A\", \"B\") AND status = \"Open\"".to_string(),
                    ),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;

            assert_eq!(issues.len(), 1);
        }

        #[tokio::test]
        async fn test_get_issues_project_key_with_native_query() {
            let server = MockServer::start();

            // project_key override + native_query without project clause
            // → should inject the overridden project key, not the default one
            server.mock(|when, then| {
                when.method(GET)
                    .path("/search")
                    .query_param_includes("jql", "project = \"OVERRIDE\" AND sprint = 42");
                then.status(200).json_body(serde_json::json!({
                    "issues": [sample_issue_json()],
                    "startAt": 0,
                    "maxResults": 20,
                    "total": 1
                }));
            });

            let client = create_self_hosted_client(&server); // default project = "PROJ"
            let issues = client
                .get_issues(IssueFilter {
                    project_key: Some("OVERRIDE".to_string()),
                    native_query: Some("sprint = 42".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;

            assert_eq!(issues.len(), 1);
        }

        #[tokio::test]
        async fn test_get_issues_empty_native_query_falls_back() {
            let server = MockServer::start();

            // Empty native_query should fall back to normal filter-based JQL
            server.mock(|when, then| {
                when.method(GET)
                    .path("/search")
                    .query_param_includes("jql", "project = \"PROJ\"");
                then.status(200).json_body(serde_json::json!({
                    "issues": [sample_issue_json()],
                    "startAt": 0,
                    "maxResults": 20,
                    "total": 1
                }));
            });

            let client = create_self_hosted_client(&server);
            let issues = client
                .get_issues(IssueFilter {
                    native_query: Some("".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;

            assert_eq!(issues.len(), 1);
        }

        #[tokio::test]
        async fn test_get_issues_native_query_order_by_only() {
            let server = MockServer::start();

            // native_query = "ORDER BY created ASC" without filters
            // → should produce "project = "PROJ" ORDER BY created ASC" (no AND)
            server.mock(|when, then| {
                when.method(GET)
                    .path("/search")
                    .query_param_includes("jql", "project = \"PROJ\" ORDER BY created ASC");
                then.status(200).json_body(serde_json::json!({
                    "issues": [sample_issue_json()],
                    "startAt": 0,
                    "maxResults": 20,
                    "total": 1
                }));
            });

            let client = create_self_hosted_client(&server);
            let issues = client
                .get_issues(IssueFilter {
                    native_query: Some("ORDER BY created ASC".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;

            assert_eq!(issues.len(), 1);
        }

        #[tokio::test]
        async fn test_get_issue() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1");
                then.status(200).json_body(sample_issue_json());
            });

            let client = create_self_hosted_client(&server);
            let issue = client.get_issue("jira#PROJ-1").await.unwrap();

            assert_eq!(issue.key, "jira#PROJ-1");
            assert_eq!(issue.title, "Fix login bug");
        }

        #[tokio::test]
        async fn test_create_issue() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST)
                    .path("/issue")
                    .body_includes("\"summary\":\"New task\"");
                then.status(201).json_body(serde_json::json!({
                    "id": "10002",
                    "key": "PROJ-2"
                }));
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-2");
                then.status(200).json_body(serde_json::json!({
                    "id": "10002",
                    "key": "PROJ-2",
                    "fields": {
                        "summary": "New task",
                        "status": {"name": "Open"},
                        "labels": [],
                        "created": "2024-01-03T10:00:00.000+0000"
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let issue = client
                .create_issue(CreateIssueInput {
                    title: "New task".to_string(),
                    description: Some("Task description".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap();

            assert_eq!(issue.key, "jira#PROJ-2");
            assert_eq!(issue.title, "New task");
        }

        #[tokio::test]
        async fn test_create_issue_with_project_id_override() {
            let server = MockServer::start();

            // Verify the payload uses the overridden project key
            server.mock(|when, then| {
                when.method(POST)
                    .path("/issue")
                    .body_includes("\"key\":\"OTHER\"");
                then.status(201).json_body(serde_json::json!({
                    "id": "10003",
                    "key": "OTHER-1"
                }));
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/OTHER-1");
                then.status(200).json_body(serde_json::json!({
                    "id": "10003",
                    "key": "OTHER-1",
                    "fields": {
                        "summary": "Task in other project",
                        "status": {"name": "Open"},
                        "labels": [],
                        "created": "2024-01-03T10:00:00.000+0000"
                    }
                }));
            });

            let client = create_self_hosted_client(&server); // default project = "PROJ"
            let issue = client
                .create_issue(CreateIssueInput {
                    title: "Task in other project".to_string(),
                    project_id: Some("OTHER".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap();

            assert_eq!(issue.key, "jira#OTHER-1");
        }

        #[tokio::test]
        async fn test_create_issue_with_issue_type() {
            let server = MockServer::start();

            // Verify the payload uses the specified issue type, not hardcoded "Task"
            server.mock(|when, then| {
                when.method(POST)
                    .path("/issue")
                    .body_includes("\"name\":\"Bug\"");
                then.status(201).json_body(serde_json::json!({
                    "id": "10004",
                    "key": "PROJ-3"
                }));
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-3");
                then.status(200).json_body(serde_json::json!({
                    "id": "10004",
                    "key": "PROJ-3",
                    "fields": {
                        "summary": "Bug report",
                        "status": {"name": "Open"},
                        "labels": [],
                        "created": "2024-01-03T10:00:00.000+0000"
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let issue = client
                .create_issue(CreateIssueInput {
                    title: "Bug report".to_string(),
                    issue_type: Some("Bug".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap();

            assert_eq!(issue.key, "jira#PROJ-3");
        }

        #[tokio::test]
        async fn test_create_issue_with_custom_fields() {
            let server = MockServer::start();

            // Verify custom fields are merged into the payload
            server.mock(|when, then| {
                when.method(POST)
                    .path("/issue")
                    .body_includes("\"customfield_10001\":8")
                    .body_includes("\"customfield_10002\":\"goal-a\"");
                then.status(201).json_body(serde_json::json!({
                    "id": "10005",
                    "key": "PROJ-5"
                }));
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-5");
                then.status(200).json_body(serde_json::json!({
                    "id": "10005",
                    "key": "PROJ-5",
                    "fields": {
                        "summary": "With custom fields",
                        "status": {"name": "Open"},
                        "labels": [],
                        "created": "2024-01-03T10:00:00.000+0000"
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let issue = client
                .create_issue(CreateIssueInput {
                    title: "With custom fields".to_string(),
                    custom_fields: Some(serde_json::json!({
                        "customfield_10001": 8,
                        "customfield_10002": "goal-a"
                    })),
                    ..Default::default()
                })
                .await
                .unwrap();

            assert_eq!(issue.key, "jira#PROJ-5");
        }

        #[tokio::test]
        async fn test_update_issue_with_custom_fields() {
            let server = MockServer::start();

            // Verify custom fields are merged into the update payload
            server.mock(|when, then| {
                when.method(PUT)
                    .path("/issue/PROJ-1")
                    .body_includes("\"customfield_10001\":5");
                then.status(204);
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1");
                then.status(200).json_body(serde_json::json!({
                    "id": "10001",
                    "key": "PROJ-1",
                    "fields": {
                        "summary": "Fix login bug",
                        "status": {"name": "Open"},
                        "labels": [],
                        "created": "2024-01-01T10:00:00.000+0000"
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let issue = client
                .update_issue(
                    "PROJ-1",
                    UpdateIssueInput {
                        custom_fields: Some(serde_json::json!({
                            "customfield_10001": 5
                        })),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();

            assert_eq!(issue.key, "jira#PROJ-1");
        }

        /// Issue #197 — components pass-through on create.
        #[tokio::test]
        async fn test_create_issue_with_components() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST).path("/issue").body_includes(
                    "\"components\":[{\"name\":\"Backend\"},{\"name\":\"Frontend\"}]",
                );
                then.status(201).json_body(serde_json::json!({
                    "id": "10010",
                    "key": "PROJ-10"
                }));
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-10");
                then.status(200).json_body(serde_json::json!({
                    "id": "10010",
                    "key": "PROJ-10",
                    "fields": {
                        "summary": "With components",
                        "status": {"name": "Open"},
                        "labels": [],
                        "created": "2024-01-05T10:00:00.000+0000"
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let issue = client
                .create_issue(CreateIssueInput {
                    title: "With components".to_string(),
                    components: vec!["Backend".to_string(), "Frontend".to_string()],
                    ..Default::default()
                })
                .await
                .unwrap();

            assert_eq!(issue.key, "jira#PROJ-10");
        }

        /// Issue #197 — empty components list on create must not emit a
        /// `"components": []` into the payload (confusing to server).
        #[tokio::test]
        async fn test_create_issue_without_components_omits_field() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST).path("/issue").is_true(|req| {
                    let body = String::from_utf8_lossy(req.body().as_ref());
                    !body.contains("\"components\"")
                });
                then.status(201).json_body(serde_json::json!({
                    "id": "10011",
                    "key": "PROJ-11"
                }));
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-11");
                then.status(200).json_body(serde_json::json!({
                    "id": "10011",
                    "key": "PROJ-11",
                    "fields": {
                        "summary": "No components",
                        "status": {"name": "Open"},
                        "labels": [],
                        "created": "2024-01-05T10:00:00.000+0000"
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let issue = client
                .create_issue(CreateIssueInput {
                    title: "No components".to_string(),
                    components: vec![],
                    ..Default::default()
                })
                .await
                .unwrap();

            assert_eq!(issue.key, "jira#PROJ-11");
        }

        /// fix_versions pass-through on create — names serialise as
        /// `[{"name": "..."}, ...]` into `fields.fixVersions`.
        #[tokio::test]
        async fn test_create_issue_with_fix_versions() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST)
                    .path("/issue")
                    .body_includes("\"fixVersions\":[{\"name\":\"3.18.0\"},{\"name\":\"3.19.0\"}]");
                then.status(201).json_body(serde_json::json!({
                    "id": "10012",
                    "key": "PROJ-12"
                }));
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-12");
                then.status(200).json_body(serde_json::json!({
                    "id": "10012",
                    "key": "PROJ-12",
                    "fields": {
                        "summary": "With fix versions",
                        "status": {"name": "Open"},
                        "labels": [],
                        "created": "2024-01-05T10:00:00.000+0000"
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let issue = client
                .create_issue(CreateIssueInput {
                    title: "With fix versions".to_string(),
                    fix_versions: vec!["3.18.0".to_string(), "3.19.0".to_string()],
                    ..Default::default()
                })
                .await
                .unwrap();

            assert_eq!(issue.key, "jira#PROJ-12");
        }

        /// Empty `fix_versions` must not emit a `"fixVersions": []` into
        /// the payload — Jira treats absent and empty differently for
        /// validators on the create screen.
        #[tokio::test]
        async fn test_create_issue_without_fix_versions_omits_field() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST).path("/issue").is_true(|req| {
                    let body = String::from_utf8_lossy(req.body().as_ref());
                    !body.contains("\"fixVersions\"")
                });
                then.status(201).json_body(serde_json::json!({
                    "id": "10013",
                    "key": "PROJ-13"
                }));
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-13");
                then.status(200).json_body(serde_json::json!({
                    "id": "10013",
                    "key": "PROJ-13",
                    "fields": {
                        "summary": "No fix versions",
                        "status": {"name": "Open"},
                        "labels": [],
                        "created": "2024-01-05T10:00:00.000+0000"
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let issue = client
                .create_issue(CreateIssueInput {
                    title: "No fix versions".to_string(),
                    fix_versions: vec![],
                    ..Default::default()
                })
                .await
                .unwrap();

            assert_eq!(issue.key, "jira#PROJ-13");
        }

        /// Issue #214 — sub-task creation requires `fields.parent.key`
        /// in the payload; the API returns 400 otherwise. The
        /// `CreateIssueInput.parent` field must round-trip into the body.
        #[tokio::test]
        async fn test_create_issue_subtask_includes_parent_in_payload() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST).path("/issue").is_true(|req| {
                    let body = String::from_utf8_lossy(req.body().as_ref());
                    body.contains("\"parent\":{\"key\":\"PROJ-1\"}")
                        && body.contains("\"name\":\"Sub-task\"")
                });
                then.status(201).json_body(serde_json::json!({
                    "id": "10010",
                    "key": "PROJ-10"
                }));
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-10");
                then.status(200).json_body(serde_json::json!({
                    "id": "10010",
                    "key": "PROJ-10",
                    "fields": {
                        "summary": "Sub task work",
                        "status": {"name": "Open"},
                        "labels": [],
                        "created": "2024-01-06T10:00:00.000+0000"
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let issue = client
                .create_issue(CreateIssueInput {
                    title: "Sub task work".to_string(),
                    issue_type: Some("Sub-task".to_string()),
                    parent: Some("PROJ-1".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap();

            assert_eq!(issue.key, "jira#PROJ-10");
        }

        /// Without a parent, the body must not include `"parent"` — Jira
        /// rejects empty `parent` objects, and we don't want to emit a
        /// dangling field for non-sub-task issue types.
        #[tokio::test]
        async fn test_create_issue_without_parent_omits_field() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST).path("/issue").is_true(|req| {
                    let body = String::from_utf8_lossy(req.body().as_ref());
                    !body.contains("\"parent\"")
                });
                then.status(201).json_body(serde_json::json!({
                    "id": "10011",
                    "key": "PROJ-11"
                }));
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-11");
                then.status(200).json_body(serde_json::json!({
                    "id": "10011",
                    "key": "PROJ-11",
                    "fields": {
                        "summary": "Plain task",
                        "status": {"name": "Open"},
                        "labels": [],
                        "created": "2024-01-06T10:00:00.000+0000"
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let issue = client
                .create_issue(CreateIssueInput {
                    title: "Plain task".to_string(),
                    parent: None,
                    ..Default::default()
                })
                .await
                .unwrap();

            assert_eq!(issue.key, "jira#PROJ-11");
        }

        /// Issue #197 — update_issue with components replaces them.
        /// `Some(vec![])` clears; `None` does not touch (handled upstream).
        #[tokio::test]
        async fn test_update_issue_replaces_components() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(PUT)
                    .path("/issue/PROJ-1")
                    .body_includes("\"components\":[{\"name\":\"Backend\"}]");
                then.status(204);
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1");
                then.status(200).json_body(serde_json::json!({
                    "id": "10001",
                    "key": "PROJ-1",
                    "fields": {
                        "summary": "Updated",
                        "status": {"name": "Open"},
                        "labels": [],
                        "created": "2024-01-01T10:00:00.000+0000"
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let issue = client
                .update_issue(
                    "PROJ-1",
                    UpdateIssueInput {
                        components: Some(vec!["Backend".to_string()]),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();

            assert_eq!(issue.key, "jira#PROJ-1");
        }

        /// `Some(["3.18.0"])` replaces fix versions with one entry;
        /// `Some(vec![])` clears; `None` does not touch (handled upstream).
        #[tokio::test]
        async fn test_update_issue_replaces_fix_versions() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(PUT)
                    .path("/issue/PROJ-1")
                    .body_includes("\"fixVersions\":[{\"name\":\"3.18.0\"}]");
                then.status(204);
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1");
                then.status(200).json_body(serde_json::json!({
                    "id": "10001",
                    "key": "PROJ-1",
                    "fields": {
                        "summary": "Updated",
                        "status": {"name": "Open"},
                        "labels": [],
                        "created": "2024-01-01T10:00:00.000+0000"
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let issue = client
                .update_issue(
                    "PROJ-1",
                    UpdateIssueInput {
                        fix_versions: Some(vec!["3.18.0".to_string()]),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();

            assert_eq!(issue.key, "jira#PROJ-1");
        }

        #[tokio::test]
        async fn test_update_issue() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(PUT)
                    .path("/issue/PROJ-1")
                    .body_includes("\"summary\":\"Updated title\"");
                then.status(204);
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1");
                then.status(200).json_body(serde_json::json!({
                    "id": "10001",
                    "key": "PROJ-1",
                    "fields": {
                        "summary": "Updated title",
                        "status": {"name": "Open"},
                        "labels": [],
                        "created": "2024-01-01T10:00:00.000+0000"
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let issue = client
                .update_issue(
                    "PROJ-1",
                    UpdateIssueInput {
                        title: Some("Updated title".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();

            assert_eq!(issue.title, "Updated title");
        }

        #[tokio::test]
        async fn test_update_issue_with_status_transition() {
            let server = MockServer::start();

            // GET transitions
            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1/transitions");
                then.status(200).json_body(serde_json::json!({
                    "transitions": [
                        {
                            "id": "21",
                            "name": "Start Progress",
                            "to": {"name": "In Progress"}
                        },
                        {
                            "id": "31",
                            "name": "Done",
                            "to": {"name": "Done"}
                        }
                    ]
                }));
            });

            // POST transition
            server.mock(|when, then| {
                when.method(POST)
                    .path("/issue/PROJ-1/transitions")
                    .body_includes("\"id\":\"31\"");
                then.status(204);
            });

            // GET issue after transition
            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1");
                then.status(200).json_body(serde_json::json!({
                    "id": "10001",
                    "key": "PROJ-1",
                    "fields": {
                        "summary": "Test",
                        "status": {"name": "Done"},
                        "labels": []
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let issue = client
                .update_issue(
                    "PROJ-1",
                    UpdateIssueInput {
                        state: Some("Done".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();

            assert_eq!(issue.state, "Done");
        }

        /// Helper: mock project statuses response with custom statuses.
        fn mock_project_statuses(server: &MockServer, statuses: serde_json::Value) {
            server.mock(|when, then| {
                when.method(GET).path("/project/PROJ/statuses");
                then.status(200).json_body(statuses);
            });
        }

        /// Helper: standard project statuses with localized names.
        fn sample_project_statuses_json() -> serde_json::Value {
            serde_json::json!([{
                "name": "Task",
                "statuses": [
                    {"name": "Offen", "id": "1", "statusCategory": {"key": "new"}},
                    {"name": "In Bearbeitung", "id": "2", "statusCategory": {"key": "indeterminate"}},
                    {"name": "Erledigt", "id": "3", "statusCategory": {"key": "done"}},
                    {"name": "Abgebrochen", "id": "4", "statusCategory": {"key": "done"}}
                ]
            }])
        }

        #[tokio::test]
        async fn test_update_issue_generic_closed_maps_to_done_category() {
            let server = MockServer::start();

            // GET transitions — include statusCategory
            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1/transitions");
                then.status(200).json_body(serde_json::json!({
                    "transitions": [
                        {
                            "id": "21",
                            "name": "Start Progress",
                            "to": {
                                "name": "In Bearbeitung",
                                "statusCategory": {"key": "indeterminate"}
                            }
                        },
                        {
                            "id": "31",
                            "name": "Erledigt",
                            "to": {
                                "name": "Erledigt",
                                "statusCategory": {"key": "done"}
                            }
                        }
                    ]
                }));
            });

            // Project statuses — used for category resolution
            mock_project_statuses(&server, sample_project_statuses_json());

            // POST transition — should pick id "31" (done category)
            server.mock(|when, then| {
                when.method(POST)
                    .path("/issue/PROJ-1/transitions")
                    .body_includes("\"id\":\"31\"");
                then.status(204);
            });

            // GET issue after transition
            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1");
                then.status(200).json_body(serde_json::json!({
                    "id": "10001",
                    "key": "PROJ-1",
                    "fields": {
                        "summary": "Test",
                        "status": {"name": "Erledigt"},
                        "labels": []
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let issue = client
                .update_issue(
                    "PROJ-1",
                    UpdateIssueInput {
                        state: Some("closed".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();

            assert_eq!(issue.state, "Erledigt");
        }

        #[tokio::test]
        async fn test_update_issue_generic_open_maps_to_new_category() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1/transitions");
                then.status(200).json_body(serde_json::json!({
                    "transitions": [
                        {
                            "id": "11",
                            "name": "Offen",
                            "to": {
                                "name": "Offen",
                                "statusCategory": {"key": "new"}
                            }
                        },
                        {
                            "id": "21",
                            "name": "In Bearbeitung",
                            "to": {
                                "name": "In Bearbeitung",
                                "statusCategory": {"key": "indeterminate"}
                            }
                        }
                    ]
                }));
            });

            mock_project_statuses(&server, sample_project_statuses_json());

            server.mock(|when, then| {
                when.method(POST)
                    .path("/issue/PROJ-1/transitions")
                    .body_includes("\"id\":\"11\"");
                then.status(204);
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1");
                then.status(200).json_body(serde_json::json!({
                    "id": "10001",
                    "key": "PROJ-1",
                    "fields": {
                        "summary": "Test",
                        "status": {"name": "Offen"},
                        "labels": []
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let issue = client
                .update_issue(
                    "PROJ-1",
                    UpdateIssueInput {
                        state: Some("open".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();

            assert_eq!(issue.state, "Offen");
        }

        #[tokio::test]
        async fn test_update_issue_canceled_resolves_via_project_statuses() {
            let server = MockServer::start();

            // Only "Abgebrochen" transition is available (done category)
            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1/transitions");
                then.status(200).json_body(serde_json::json!({
                    "transitions": [
                        {
                            "id": "21",
                            "name": "Start Progress",
                            "to": {
                                "name": "In Bearbeitung",
                                "statusCategory": {"key": "indeterminate"}
                            }
                        },
                        {
                            "id": "41",
                            "name": "Cancel",
                            "to": {
                                "name": "Abgebrochen",
                                "statusCategory": {"key": "done"}
                            }
                        }
                    ]
                }));
            });

            // Project statuses: "Abgebrochen" is in done category
            mock_project_statuses(&server, sample_project_statuses_json());

            // POST transition — should pick "41" (resolved via project statuses + category)
            server.mock(|when, then| {
                when.method(POST)
                    .path("/issue/PROJ-1/transitions")
                    .body_includes("\"id\":\"41\"");
                then.status(204);
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1");
                then.status(200).json_body(serde_json::json!({
                    "id": "10001",
                    "key": "PROJ-1",
                    "fields": {
                        "summary": "Test",
                        "status": {"name": "Abgebrochen"},
                        "labels": []
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let issue = client
                .update_issue(
                    "PROJ-1",
                    UpdateIssueInput {
                        state: Some("canceled".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();

            assert_eq!(issue.state, "Abgebrochen");
        }

        #[tokio::test]
        async fn test_update_issue_exact_project_status_name_match() {
            let server = MockServer::start();

            // User passes exact project status name "Abgebrochen"
            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1/transitions");
                then.status(200).json_body(serde_json::json!({
                    "transitions": [
                        {
                            "id": "41",
                            "name": "Cancel",
                            "to": {"name": "Abgebrochen", "statusCategory": {"key": "done"}}
                        },
                        {
                            "id": "31",
                            "name": "Done",
                            "to": {"name": "Erledigt", "statusCategory": {"key": "done"}}
                        }
                    ]
                }));
            });

            mock_project_statuses(&server, sample_project_statuses_json());

            // Should pick transition to "Abgebrochen" by exact project status name
            server.mock(|when, then| {
                when.method(POST)
                    .path("/issue/PROJ-1/transitions")
                    .body_includes("\"id\":\"41\"");
                then.status(204);
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1");
                then.status(200).json_body(serde_json::json!({
                    "id": "10001",
                    "key": "PROJ-1",
                    "fields": {
                        "summary": "Test",
                        "status": {"name": "Abgebrochen"},
                        "labels": []
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let issue = client
                .update_issue(
                    "PROJ-1",
                    UpdateIssueInput {
                        state: Some("Abgebrochen".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();

            assert_eq!(issue.state, "Abgebrochen");
        }

        #[tokio::test]
        async fn test_update_issue_fallback_when_project_statuses_unavailable() {
            let server = MockServer::start();

            // Transitions with category info
            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1/transitions");
                then.status(200).json_body(serde_json::json!({
                    "transitions": [{
                        "id": "31",
                        "name": "Done",
                        "to": {"name": "Done", "statusCategory": {"key": "done"}}
                    }]
                }));
            });

            // Project statuses endpoint returns 403 (no permission)
            server.mock(|when, then| {
                when.method(GET).path("/project/PROJ/statuses");
                then.status(403).body("Forbidden");
            });

            server.mock(|when, then| {
                when.method(POST)
                    .path("/issue/PROJ-1/transitions")
                    .body_includes("\"id\":\"31\"");
                then.status(204);
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1");
                then.status(200).json_body(serde_json::json!({
                    "id": "10001",
                    "key": "PROJ-1",
                    "fields": {
                        "summary": "Test",
                        "status": {"name": "Done"},
                        "labels": []
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            // "closed" → category "done" → should still work via fallback
            let issue = client
                .update_issue(
                    "PROJ-1",
                    UpdateIssueInput {
                        state: Some("closed".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();

            assert_eq!(issue.state, "Done");
        }

        #[tokio::test]
        async fn test_get_comments() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1/comment");
                then.status(200).json_body(serde_json::json!({
                    "comments": [{
                        "id": "100",
                        "body": "Great work!",
                        "author": {
                            "name": "reviewer",
                            "displayName": "Reviewer"
                        },
                        "created": "2024-01-01T12:00:00.000+0000",
                        "updated": "2024-01-01T12:00:00.000+0000"
                    }]
                }));
            });

            let client = create_self_hosted_client(&server);
            let comments = client.get_comments("PROJ-1").await.unwrap().items;

            assert_eq!(comments.len(), 1);
            assert_eq!(comments[0].id, "100");
            assert_eq!(comments[0].body, "Great work!");
            assert_eq!(comments[0].author.as_ref().unwrap().username, "reviewer");
        }

        #[tokio::test]
        async fn test_add_comment() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST)
                    .path("/issue/PROJ-1/comment")
                    .body_includes("\"body\":\"My comment\"");
                then.status(201).json_body(serde_json::json!({
                    "id": "101",
                    "body": "My comment",
                    "author": {
                        "name": "user",
                        "displayName": "User"
                    },
                    "created": "2024-01-01T13:00:00.000+0000"
                }));
            });

            let client = create_self_hosted_client(&server);
            let comment = IssueProvider::add_comment(&client, "PROJ-1", "My comment")
                .await
                .unwrap();

            assert_eq!(comment.id, "101");
            assert_eq!(comment.body, "My comment");
        }

        // =================================================================
        // Cloud (API v3) tests
        // =================================================================

        #[tokio::test]
        async fn test_cloud_get_issues() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/search/jql")
                    .query_param_exists("jql");
                then.status(200).json_body(serde_json::json!({
                    "issues": [sample_cloud_issue_json()]
                }));
            });

            let client = create_cloud_client(&server);
            let issues = client
                .get_issues(IssueFilter::default())
                .await
                .unwrap()
                .items;

            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].key, "jira#PROJ-1");
            assert_eq!(
                issues[0].description,
                Some("Login fails on mobile".to_string())
            );
        }

        #[tokio::test]
        async fn test_cloud_create_issue_adf() {
            let server = MockServer::start();

            // Verify ADF in request body
            server.mock(|when, then| {
                when.method(POST)
                    .path("/issue")
                    .body_includes("\"type\":\"doc\"")
                    .body_includes("\"version\":1");
                then.status(201).json_body(serde_json::json!({
                    "id": "10003",
                    "key": "PROJ-3"
                }));
            });

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-3");
                then.status(200).json_body(serde_json::json!({
                    "id": "10003",
                    "key": "PROJ-3",
                    "fields": {
                        "summary": "Cloud task",
                        "description": {
                            "version": 1,
                            "type": "doc",
                            "content": [{
                                "type": "paragraph",
                                "content": [{"type": "text", "text": "Cloud description"}]
                            }]
                        },
                        "status": {"name": "To Do"},
                        "labels": []
                    }
                }));
            });

            let client = create_cloud_client(&server);
            let issue = client
                .create_issue(CreateIssueInput {
                    title: "Cloud task".to_string(),
                    description: Some("Cloud description".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap();

            assert_eq!(issue.key, "jira#PROJ-3");
            assert_eq!(issue.description, Some("Cloud description".to_string()));
        }

        #[tokio::test]
        async fn test_cloud_add_comment_adf() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST)
                    .path("/issue/PROJ-1/comment")
                    .body_includes("\"type\":\"doc\"");
                then.status(201).json_body(serde_json::json!({
                    "id": "201",
                    "body": {
                        "version": 1,
                        "type": "doc",
                        "content": [{
                            "type": "paragraph",
                            "content": [{"type": "text", "text": "ADF comment body"}]
                        }]
                    },
                    "author": {
                        "accountId": "abc123",
                        "displayName": "Commenter"
                    },
                    "created": "2024-01-02T10:00:00.000+0000"
                }));
            });

            let client = create_cloud_client(&server);
            let comment = IssueProvider::add_comment(&client, "PROJ-1", "ADF comment body")
                .await
                .unwrap();

            assert_eq!(comment.id, "201");
            assert_eq!(comment.body, "ADF comment body");
        }

        #[tokio::test]
        async fn test_cloud_get_issue_adf_description() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1");
                then.status(200).json_body(sample_cloud_issue_json());
            });

            let client = create_cloud_client(&server);
            let issue = client.get_issue("PROJ-1").await.unwrap();

            assert_eq!(issue.description, Some("Login fails on mobile".to_string()));
        }

        // =================================================================
        // Error handling tests
        // =================================================================

        #[tokio::test]
        async fn test_handle_401() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1");
                then.status(401).body("Unauthorized");
            });

            let client = create_self_hosted_client(&server);
            let result = client.get_issue("PROJ-1").await;

            assert!(result.is_err());
            assert!(matches!(result.unwrap_err(), Error::Unauthorized(_)));
        }

        #[tokio::test]
        async fn test_handle_404() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-999");
                then.status(404).body("Issue not found");
            });

            let client = create_self_hosted_client(&server);
            let result = client.get_issue("PROJ-999").await;

            assert!(result.is_err());
            assert!(matches!(result.unwrap_err(), Error::NotFound(_)));
        }

        #[tokio::test]
        async fn test_handle_500() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/search");
                then.status(500).body("Internal Server Error");
            });

            let client = create_self_hosted_client(&server);
            let result = client.get_issues(IssueFilter::default()).await;

            assert!(result.is_err());
            assert!(matches!(result.unwrap_err(), Error::ServerError { .. }));
        }

        // =================================================================
        // MR methods unsupported test
        // =================================================================

        #[tokio::test]
        async fn test_mr_methods_unsupported() {
            let client = JiraClient::with_base_url(
                "http://localhost",
                "PROJ",
                "user@example.com",
                token("token"),
                false,
            );

            let result = client.get_merge_requests(MrFilter::default()).await;
            assert!(matches!(
                result.unwrap_err(),
                Error::ProviderUnsupported { .. }
            ));

            let result = client.get_merge_request("mr#1").await;
            assert!(matches!(
                result.unwrap_err(),
                Error::ProviderUnsupported { .. }
            ));

            let result = client.get_discussions("mr#1").await;
            assert!(matches!(
                result.unwrap_err(),
                Error::ProviderUnsupported { .. }
            ));

            let result = client.get_diffs("mr#1").await;
            assert!(matches!(
                result.unwrap_err(),
                Error::ProviderUnsupported { .. }
            ));

            let result = MergeRequestProvider::add_comment(
                &client,
                "mr#1",
                CreateCommentInput {
                    body: "test".to_string(),
                    position: None,
                    discussion_id: None,
                },
            )
            .await;
            assert!(matches!(
                result.unwrap_err(),
                Error::ProviderUnsupported { .. }
            ));
        }

        // =================================================================
        // Current user tests
        // =================================================================

        #[tokio::test]
        async fn test_get_current_user() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/myself");
                then.status(200).json_body(serde_json::json!({
                    "name": "jdoe",
                    "displayName": "John Doe",
                    "emailAddress": "john@example.com"
                }));
            });

            let client = create_self_hosted_client(&server);
            let user = client.get_current_user().await.unwrap();

            assert_eq!(user.username, "jdoe");
            assert_eq!(user.name, Some("John Doe".to_string()));
            assert_eq!(user.email, Some("john@example.com".to_string()));
        }

        #[tokio::test]
        async fn test_get_current_user_auth_failure() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/myself");
                then.status(401).body("Unauthorized");
            });

            let client = create_self_hosted_client(&server);
            let result = client.get_current_user().await;

            assert!(result.is_err());
            assert!(matches!(result.unwrap_err(), Error::Unauthorized(_)));
        }

        #[tokio::test]
        async fn test_transition_not_found_error_lists_available() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/issue/PROJ-1/transitions");
                then.status(200).json_body(serde_json::json!({
                    "transitions": [
                        {
                            "id": "21",
                            "name": "Start Progress",
                            "to": {
                                "name": "In Bearbeitung",
                                "statusCategory": {"key": "indeterminate"}
                            }
                        }
                    ]
                }));
            });

            // Project statuses — no matching category for "nonexistent"
            mock_project_statuses(&server, sample_project_statuses_json());

            let client = create_self_hosted_client(&server);
            let result = client
                .update_issue(
                    "PROJ-1",
                    UpdateIssueInput {
                        state: Some("nonexistent".to_string()),
                        ..Default::default()
                    },
                )
                .await;

            assert!(result.is_err());
            let err = result.unwrap_err().to_string();
            assert!(err.contains("No transition to status"), "got: {}", err);
            assert!(
                err.contains("In Bearbeitung"),
                "should list available: {}",
                err
            );
        }

        #[tokio::test]
        async fn test_cloud_get_issues_pagination_next_page_token() {
            let server = MockServer::start();

            // Page 2 mock must be registered first — httpmock matches most specific.
            // Page 2: has nextPageToken param, returns 1 issue, no more pages
            server.mock(|when, then| {
                when.method(GET)
                    .path("/search/jql")
                    .query_param("nextPageToken", "page2token");
                then.status(200).json_body(serde_json::json!({
                    "issues": [
                        {
                            "id": "10003",
                            "key": "PROJ-3",
                            "fields": {
                                "summary": "Issue 3",
                                "status": {"name": "Done"},
                                "labels": [],
                                "created": "2024-01-03T10:00:00.000+0000"
                            }
                        }
                    ]
                }));
            });

            // Page 1: no nextPageToken param, returns 2 issues + nextPageToken
            server.mock(|when, then| {
                when.method(GET)
                    .path("/search/jql")
                    .query_param_exists("jql");
                then.status(200).json_body(serde_json::json!({
                    "issues": [
                        {
                            "id": "10001",
                            "key": "PROJ-1",
                            "fields": {
                                "summary": "Issue 1",
                                "status": {"name": "Open"},
                                "labels": [],
                                "created": "2024-01-01T10:00:00.000+0000"
                            }
                        },
                        {
                            "id": "10002",
                            "key": "PROJ-2",
                            "fields": {
                                "summary": "Issue 2",
                                "status": {"name": "Open"},
                                "labels": [],
                                "created": "2024-01-02T10:00:00.000+0000"
                            }
                        }
                    ],
                    "nextPageToken": "page2token"
                }));
            });

            let client = create_cloud_client(&server);
            let issues = client
                .get_issues(IssueFilter {
                    limit: Some(3),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;

            assert_eq!(issues.len(), 3);
            assert_eq!(issues[0].key, "jira#PROJ-1");
            assert_eq!(issues[1].key, "jira#PROJ-2");
            assert_eq!(issues[2].key, "jira#PROJ-3");
        }

        #[test]
        fn test_escape_jql() {
            assert_eq!(escape_jql("simple"), "simple");
            assert_eq!(escape_jql(r#"has "quotes""#), r#"has \"quotes\""#);
            assert_eq!(escape_jql(r"back\slash"), r"back\\slash");
            assert_eq!(
                escape_jql(r#"both "and" \ here"#),
                r#"both \"and\" \\ here"#
            );
        }

        #[test]
        fn test_has_project_clause() {
            // Positive cases — standard operators
            assert!(has_project_clause("project = \"PROJ\""));
            assert!(has_project_clause("project = PROJ AND status = Open"));
            assert!(has_project_clause("project IN (\"A\", \"B\")"));
            assert!(has_project_clause("project in(A, B)"));
            assert!(has_project_clause("PROJECT = KEY")); // case-insensitive
            assert!(has_project_clause("status = Open AND project = X"));
            assert!(has_project_clause("project ~ KEY")); // contains operator
            // Positive cases — negation operators
            assert!(has_project_clause("project != \"PROJ\""));
            assert!(has_project_clause("project NOT IN (\"A\", \"B\")"));
            assert!(has_project_clause("project not in(A)"));
            // Negative cases — no project clause
            assert!(!has_project_clause("fixVersion = \"1.0\""));
            assert!(!has_project_clause("status = Done"));
            // Negative cases — "project" inside quoted strings
            assert!(!has_project_clause("summary ~ \"project plan\""));
            assert!(!has_project_clause("summary ~ \"project information\""));
            assert!(!has_project_clause("summary ~ \"project = foo\""));
            // Negative cases — underscore word boundary
            assert!(!has_project_clause("my_project = X"));
        }

        // =================================================================
        // merge_custom_fields unit tests
        // =================================================================

        #[test]
        fn test_merge_custom_fields_into_payload() {
            use crate::types::*;
            let payload = CreateIssuePayload {
                fields: CreateIssueFields {
                    project: ProjectKey { key: "PROJ".into() },
                    summary: "Test".into(),
                    issuetype: IssueType {
                        name: "Task".into(),
                    },
                    description: None,
                    labels: None,
                    priority: None,
                    assignee: None,
                    components: None,
                    fix_versions: None,
                    parent: None,
                },
            };

            let cf = Some(serde_json::json!({"customfield_10001": 8, "customfield_10002": "x"}));
            let (merged, count) = merge_custom_fields_into_payload(payload, &cf).unwrap();

            let fields = merged.get("fields").unwrap();
            assert_eq!(fields["customfield_10001"], 8);
            assert_eq!(fields["customfield_10002"], "x");
            assert_eq!(count, 2);
            assert_eq!(fields["summary"], "Test");
            assert_eq!(fields["project"]["key"], "PROJ");
        }

        #[test]
        fn test_merge_custom_fields_none_is_noop() {
            use crate::types::*;
            let payload = CreateIssuePayload {
                fields: CreateIssueFields {
                    project: ProjectKey { key: "PROJ".into() },
                    summary: "Test".into(),
                    issuetype: IssueType {
                        name: "Task".into(),
                    },
                    description: None,
                    labels: None,
                    priority: None,
                    assignee: None,
                    components: None,
                    fix_versions: None,
                    parent: None,
                },
            };

            let (merged, count) = merge_custom_fields_into_payload(payload, &None).unwrap();
            assert_eq!(count, 0);
            let fields = merged.get("fields").unwrap();
            assert_eq!(fields["summary"], "Test");
            assert!(fields.get("customfield_10001").is_none());
        }

        #[test]
        fn test_merge_custom_fields_rejects_non_custom_keys() {
            use crate::types::*;
            let payload = CreateIssuePayload {
                fields: CreateIssueFields {
                    project: ProjectKey { key: "PROJ".into() },
                    summary: "Test".into(),
                    issuetype: IssueType {
                        name: "Task".into(),
                    },
                    description: None,
                    labels: None,
                    priority: None,
                    assignee: None,
                    components: None,
                    fix_versions: None,
                    parent: None,
                },
            };

            // "summary" should be rejected, "customfield_10001" should pass
            let cf = Some(serde_json::json!({"summary": "HACKED", "customfield_10001": 5}));
            let (merged, count) = merge_custom_fields_into_payload(payload, &cf).unwrap();

            let fields = merged.get("fields").unwrap();
            assert_eq!(fields["summary"], "Test"); // NOT overwritten
            assert_eq!(fields["customfield_10001"], 5); // custom field applied
            assert_eq!(count, 1); // only customfield_10001 counted
        }

        // =================================================================
        // get_issue_relations integration test
        // =================================================================

        #[tokio::test]
        async fn test_get_issue_relations() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/issue/PROJ-1")
                    .query_param_includes("fields", "parent");
                then.status(200).json_body(serde_json::json!({
                    "id": "10001",
                    "key": "PROJ-1",
                    "fields": {
                        "summary": "Main issue",
                        "status": {"name": "Open"},
                        "labels": [],
                        "parent": {
                            "id": "10000",
                            "key": "PROJ-0",
                            "fields": {
                                "summary": "Parent issue",
                                "status": {"name": "Open"},
                                "labels": []
                            }
                        },
                        "subtasks": [
                            {
                                "id": "10002",
                                "key": "PROJ-2",
                                "fields": {
                                    "summary": "Subtask 1",
                                    "status": {"name": "In Progress"},
                                    "labels": []
                                }
                            }
                        ],
                        "issuelinks": [
                            {
                                "type": {
                                    "name": "Blocks",
                                    "outward": "blocks",
                                    "inward": "is blocked by"
                                },
                                "outwardIssue": {
                                    "id": "10003",
                                    "key": "PROJ-3",
                                    "fields": {
                                        "summary": "Blocked issue",
                                        "status": {"name": "Open"},
                                        "labels": []
                                    }
                                }
                            }
                        ]
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let relations = client.get_issue_relations("jira#PROJ-1").await.unwrap();

            assert!(relations.parent.is_some());
            assert_eq!(relations.parent.unwrap().key, "jira#PROJ-0");
            assert_eq!(relations.subtasks.len(), 1);
            assert_eq!(relations.subtasks[0].key, "jira#PROJ-2");
            assert_eq!(relations.blocks.len(), 1);
            assert_eq!(relations.blocks[0].issue.key, "jira#PROJ-3");
        }

        // =================================================================
        // Attachment tests (Phase 2)
        // =================================================================

        #[tokio::test]
        async fn test_get_issue_attachments_maps_fields() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/issue/PROJ-1")
                    .query_param("fields", "attachment");
                then.status(200).json_body(serde_json::json!({
                    "id": "10001",
                    "key": "PROJ-1",
                    "fields": {
                        "attachment": [
                            {
                                "id": "42",
                                "filename": "crash.log",
                                "content": "https://example/rest/api/2/attachment/content/42",
                                "size": 2048,
                                "mimeType": "text/plain",
                                "created": "2024-01-01T00:00:00.000+0000",
                                "author": {
                                    "name": "uploader",
                                    "displayName": "Upload User"
                                }
                            }
                        ]
                    }
                }));
            });

            let client = create_self_hosted_client(&server);
            let assets = client.get_issue_attachments("jira#PROJ-1").await.unwrap();
            assert_eq!(assets.len(), 1);
            let a = &assets[0];
            assert_eq!(a.id, "42");
            assert_eq!(a.filename, "crash.log");
            assert_eq!(a.mime_type.as_deref(), Some("text/plain"));
            assert_eq!(a.size, Some(2048));
            assert_eq!(a.author.as_deref(), Some("Upload User"));
        }

        #[tokio::test]
        async fn test_download_attachment_returns_bytes() {
            let server = MockServer::start();

            // Self-Hosted: first fetches metadata, then downloads from content URL.
            let content_url = server.url("/secure/attachment/42/trace.log");
            server.mock(|when, then| {
                when.method(GET).path("/attachment/42");
                then.status(200).json_body(serde_json::json!({
                    "self": "http://localhost/rest/api/2/attachment/42",
                    "id": "42",
                    "filename": "trace.log",
                    "content": content_url,
                }));
            });
            server.mock(|when, then| {
                when.method(GET).path("/secure/attachment/42/trace.log");
                then.status(200).body("stack trace here");
            });

            let client = create_self_hosted_client(&server);
            let bytes = client
                .download_attachment("jira#PROJ-1", "42")
                .await
                .unwrap();
            assert_eq!(bytes, b"stack trace here");
        }

        #[tokio::test]
        async fn test_delete_attachment_ok() {
            let server = MockServer::start();

            let mock = server.mock(|when, then| {
                when.method(DELETE).path("/attachment/42");
                then.status(204);
            });

            let client = create_self_hosted_client(&server);
            client.delete_attachment("jira#PROJ-1", "42").await.unwrap();
            mock.assert();
        }

        #[tokio::test]
        async fn test_upload_attachment_returns_content_url() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST)
                    .path("/issue/PROJ-1/attachments")
                    .header("X-Atlassian-Token", "no-check");
                then.status(200).json_body(serde_json::json!([
                    {
                        "id": "99",
                        "filename": "report.txt",
                        "content": "https://example/rest/api/2/attachment/content/99",
                        "size": 10
                    }
                ]));
            });

            let client = create_self_hosted_client(&server);
            let url = client
                .upload_attachment("jira#PROJ-1", "report.txt", b"0123456789")
                .await
                .unwrap();
            assert_eq!(url, "https://example/rest/api/2/attachment/content/99");
        }

        #[tokio::test]
        async fn test_jira_asset_capabilities() {
            let server = MockServer::start();
            let client = create_self_hosted_client(&server);
            let caps = client.asset_capabilities();
            assert!(caps.issue.upload);
            assert!(caps.issue.download);
            assert!(caps.issue.delete);
            assert!(caps.issue.list);
        }
    }

    // =========================================================================
    // map_relations unit tests
    // =========================================================================

    #[test]
    fn test_map_relations_empty() {
        let issue = JiraIssue {
            id: "10001".to_string(),
            key: "PROJ-1".to_string(),
            fields: JiraIssueFields {
                summary: Some("Test".to_string()),
                description: None,
                status: None,
                priority: None,
                assignee: None,
                reporter: None,
                labels: vec![],
                created: None,
                updated: None,
                parent: None,
                subtasks: vec![],
                issuelinks: vec![],
                attachment: vec![],
            },
        };

        let relations = map_relations(&issue, JiraFlavor::Cloud, "https://test.atlassian.net");

        assert!(relations.parent.is_none());
        assert!(relations.subtasks.is_empty());
        assert!(relations.blocks.is_empty());
        assert!(relations.blocked_by.is_empty());
        assert!(relations.related_to.is_empty());
        assert!(relations.duplicates.is_empty());
    }

    #[test]
    fn test_map_relations_with_parent() {
        let parent = Box::new(JiraIssue {
            id: "10000".to_string(),
            key: "PROJ-0".to_string(),
            fields: JiraIssueFields {
                summary: Some("Parent Issue".to_string()),
                description: None,
                status: Some(JiraStatus {
                    name: "Open".to_string(),
                    status_category: None,
                }),
                priority: None,
                assignee: None,
                reporter: None,
                labels: vec![],
                created: None,
                updated: None,
                parent: None,
                subtasks: vec![],
                issuelinks: vec![],
                attachment: vec![],
            },
        });

        let issue = JiraIssue {
            id: "10001".to_string(),
            key: "PROJ-1".to_string(),
            fields: JiraIssueFields {
                summary: Some("Child Issue".to_string()),
                description: None,
                status: None,
                priority: None,
                assignee: None,
                reporter: None,
                labels: vec![],
                created: None,
                updated: None,
                parent: Some(parent),
                subtasks: vec![],
                issuelinks: vec![],
                attachment: vec![],
            },
        };

        let relations = map_relations(&issue, JiraFlavor::SelfHosted, "https://jira.example.com");

        assert!(relations.parent.is_some());
        let parent_issue = relations.parent.unwrap();
        assert_eq!(parent_issue.key, "jira#PROJ-0");
        assert_eq!(parent_issue.title, "Parent Issue");
    }

    #[test]
    fn test_map_relations_with_subtasks() {
        let issue = JiraIssue {
            id: "10001".to_string(),
            key: "PROJ-1".to_string(),
            fields: JiraIssueFields {
                summary: Some("Epic".to_string()),
                description: None,
                status: None,
                priority: None,
                assignee: None,
                reporter: None,
                labels: vec![],
                created: None,
                updated: None,
                parent: None,
                subtasks: vec![
                    JiraIssue {
                        id: "10002".to_string(),
                        key: "PROJ-2".to_string(),
                        fields: JiraIssueFields {
                            summary: Some("Subtask 1".to_string()),
                            description: None,
                            status: Some(JiraStatus {
                                name: "In Progress".to_string(),
                                status_category: None,
                            }),
                            priority: None,
                            assignee: None,
                            reporter: None,
                            labels: vec![],
                            created: None,
                            updated: None,
                            parent: None,
                            subtasks: vec![],
                            issuelinks: vec![],
                            attachment: vec![],
                        },
                    },
                    JiraIssue {
                        id: "10003".to_string(),
                        key: "PROJ-3".to_string(),
                        fields: JiraIssueFields {
                            summary: Some("Subtask 2".to_string()),
                            description: None,
                            status: None,
                            priority: None,
                            assignee: None,
                            reporter: None,
                            labels: vec![],
                            created: None,
                            updated: None,
                            parent: None,
                            subtasks: vec![],
                            issuelinks: vec![],
                            attachment: vec![],
                        },
                    },
                ],
                issuelinks: vec![],
                attachment: vec![],
            },
        };

        let relations = map_relations(&issue, JiraFlavor::Cloud, "https://test.atlassian.net");

        assert_eq!(relations.subtasks.len(), 2);
        assert_eq!(relations.subtasks[0].key, "jira#PROJ-2");
        assert_eq!(relations.subtasks[0].title, "Subtask 1");
        assert_eq!(relations.subtasks[1].key, "jira#PROJ-3");
        assert_eq!(relations.subtasks[1].title, "Subtask 2");
    }

    #[test]
    fn test_map_relations_with_issuelinks_blocks() {
        let issue = JiraIssue {
            id: "10001".to_string(),
            key: "PROJ-1".to_string(),
            fields: JiraIssueFields {
                summary: Some("Test".to_string()),
                description: None,
                status: None,
                priority: None,
                assignee: None,
                reporter: None,
                labels: vec![],
                created: None,
                updated: None,
                parent: None,
                subtasks: vec![],
                issuelinks: vec![
                    // Outward "blocks" link
                    JiraIssueLink {
                        id: Some("1".to_string()),
                        link_type: JiraIssueLinkType {
                            name: "Blocks".to_string(),
                            outward: Some("blocks".to_string()),
                            inward: Some("is blocked by".to_string()),
                        },
                        outward_issue: Some(Box::new(JiraIssue {
                            id: "10002".to_string(),
                            key: "PROJ-2".to_string(),
                            fields: JiraIssueFields {
                                summary: Some("Blocked".to_string()),
                                description: None,
                                status: None,
                                priority: None,
                                assignee: None,
                                reporter: None,
                                labels: vec![],
                                created: None,
                                updated: None,
                                parent: None,
                                subtasks: vec![],
                                issuelinks: vec![],
                                attachment: vec![],
                            },
                        })),
                        inward_issue: None,
                    },
                    // Inward "is blocked by" link
                    JiraIssueLink {
                        id: Some("2".to_string()),
                        link_type: JiraIssueLinkType {
                            name: "Blocks".to_string(),
                            outward: Some("blocks".to_string()),
                            inward: Some("is blocked by".to_string()),
                        },
                        outward_issue: None,
                        inward_issue: Some(Box::new(JiraIssue {
                            id: "10003".to_string(),
                            key: "PROJ-3".to_string(),
                            fields: JiraIssueFields {
                                summary: Some("Blocker".to_string()),
                                description: None,
                                status: None,
                                priority: None,
                                assignee: None,
                                reporter: None,
                                labels: vec![],
                                created: None,
                                updated: None,
                                parent: None,
                                subtasks: vec![],
                                issuelinks: vec![],
                                attachment: vec![],
                            },
                        })),
                    },
                ],
                attachment: vec![],
            },
        };

        let relations = map_relations(&issue, JiraFlavor::Cloud, "https://test.atlassian.net");

        assert_eq!(relations.blocks.len(), 1);
        assert_eq!(relations.blocks[0].issue.key, "jira#PROJ-2");
        assert_eq!(relations.blocks[0].link_type, "Blocks");
        assert_eq!(relations.blocked_by.len(), 1);
        assert_eq!(relations.blocked_by[0].issue.key, "jira#PROJ-3");
    }

    #[test]
    fn test_map_relations_with_issuelinks_duplicates() {
        let issue = JiraIssue {
            id: "10001".to_string(),
            key: "PROJ-1".to_string(),
            fields: JiraIssueFields {
                summary: Some("Test".to_string()),
                description: None,
                status: None,
                priority: None,
                assignee: None,
                reporter: None,
                labels: vec![],
                created: None,
                updated: None,
                parent: None,
                subtasks: vec![],
                issuelinks: vec![
                    // Outward "duplicate" link
                    JiraIssueLink {
                        id: Some("1".to_string()),
                        link_type: JiraIssueLinkType {
                            name: "Duplicate".to_string(),
                            outward: Some("duplicates".to_string()),
                            inward: Some("is duplicated by".to_string()),
                        },
                        outward_issue: Some(Box::new(JiraIssue {
                            id: "10002".to_string(),
                            key: "PROJ-2".to_string(),
                            fields: JiraIssueFields {
                                summary: Some("Dup outward".to_string()),
                                description: None,
                                status: None,
                                priority: None,
                                assignee: None,
                                reporter: None,
                                labels: vec![],
                                created: None,
                                updated: None,
                                parent: None,
                                subtasks: vec![],
                                issuelinks: vec![],
                                attachment: vec![],
                            },
                        })),
                        inward_issue: None,
                    },
                    // Inward "duplicate" link
                    JiraIssueLink {
                        id: Some("2".to_string()),
                        link_type: JiraIssueLinkType {
                            name: "Duplicate".to_string(),
                            outward: Some("duplicates".to_string()),
                            inward: Some("is duplicated by".to_string()),
                        },
                        outward_issue: None,
                        inward_issue: Some(Box::new(JiraIssue {
                            id: "10003".to_string(),
                            key: "PROJ-3".to_string(),
                            fields: JiraIssueFields {
                                summary: Some("Dup inward".to_string()),
                                description: None,
                                status: None,
                                priority: None,
                                assignee: None,
                                reporter: None,
                                labels: vec![],
                                created: None,
                                updated: None,
                                parent: None,
                                subtasks: vec![],
                                issuelinks: vec![],
                                attachment: vec![],
                            },
                        })),
                    },
                ],
                attachment: vec![],
            },
        };

        let relations = map_relations(&issue, JiraFlavor::Cloud, "https://test.atlassian.net");

        // Both outward and inward duplicates go to `duplicates`
        assert_eq!(relations.duplicates.len(), 2);
        assert_eq!(relations.duplicates[0].issue.key, "jira#PROJ-2");
        assert_eq!(relations.duplicates[1].issue.key, "jira#PROJ-3");
    }

    #[test]
    fn test_map_relations_with_issuelinks_relates() {
        let issue = JiraIssue {
            id: "10001".to_string(),
            key: "PROJ-1".to_string(),
            fields: JiraIssueFields {
                summary: Some("Test".to_string()),
                description: None,
                status: None,
                priority: None,
                assignee: None,
                reporter: None,
                labels: vec![],
                created: None,
                updated: None,
                parent: None,
                subtasks: vec![],
                issuelinks: vec![JiraIssueLink {
                    id: Some("1".to_string()),
                    link_type: JiraIssueLinkType {
                        name: "Relates".to_string(),
                        outward: Some("relates to".to_string()),
                        inward: Some("relates to".to_string()),
                    },
                    outward_issue: Some(Box::new(JiraIssue {
                        id: "10002".to_string(),
                        key: "PROJ-2".to_string(),
                        fields: JiraIssueFields {
                            summary: Some("Related".to_string()),
                            description: None,
                            status: None,
                            priority: None,
                            assignee: None,
                            reporter: None,
                            labels: vec![],
                            created: None,
                            updated: None,
                            parent: None,
                            subtasks: vec![],
                            issuelinks: vec![],
                            attachment: vec![],
                        },
                    })),
                    inward_issue: None,
                }],
                attachment: vec![],
            },
        };

        let relations = map_relations(&issue, JiraFlavor::Cloud, "https://test.atlassian.net");

        assert_eq!(relations.related_to.len(), 1);
        assert_eq!(relations.related_to[0].issue.key, "jira#PROJ-2");
        assert_eq!(relations.related_to[0].link_type, "Relates");
    }

    #[test]
    fn test_map_relations_mixed() {
        let issue = JiraIssue {
            id: "10001".to_string(),
            key: "PROJ-1".to_string(),
            fields: JiraIssueFields {
                summary: Some("Main".to_string()),
                description: None,
                status: None,
                priority: None,
                assignee: None,
                reporter: None,
                labels: vec![],
                created: None,
                updated: None,
                parent: Some(Box::new(JiraIssue {
                    id: "10000".to_string(),
                    key: "PROJ-0".to_string(),
                    fields: JiraIssueFields {
                        summary: Some("Parent".to_string()),
                        description: None,
                        status: None,
                        priority: None,
                        assignee: None,
                        reporter: None,
                        labels: vec![],
                        created: None,
                        updated: None,
                        parent: None,
                        subtasks: vec![],
                        issuelinks: vec![],
                        attachment: vec![],
                    },
                })),
                subtasks: vec![JiraIssue {
                    id: "10002".to_string(),
                    key: "PROJ-2".to_string(),
                    fields: JiraIssueFields {
                        summary: Some("Sub".to_string()),
                        description: None,
                        status: None,
                        priority: None,
                        assignee: None,
                        reporter: None,
                        labels: vec![],
                        created: None,
                        updated: None,
                        parent: None,
                        subtasks: vec![],
                        issuelinks: vec![],
                        attachment: vec![],
                    },
                }],
                issuelinks: vec![JiraIssueLink {
                    id: Some("1".to_string()),
                    link_type: JiraIssueLinkType {
                        name: "Blocks".to_string(),
                        outward: Some("blocks".to_string()),
                        inward: Some("is blocked by".to_string()),
                    },
                    outward_issue: Some(Box::new(JiraIssue {
                        id: "10003".to_string(),
                        key: "PROJ-3".to_string(),
                        fields: JiraIssueFields {
                            summary: Some("Blocked".to_string()),
                            description: None,
                            status: None,
                            priority: None,
                            assignee: None,
                            reporter: None,
                            labels: vec![],
                            created: None,
                            updated: None,
                            parent: None,
                            subtasks: vec![],
                            issuelinks: vec![],
                            attachment: vec![],
                        },
                    })),
                    inward_issue: None,
                }],
                attachment: vec![],
            },
        };

        let relations = map_relations(&issue, JiraFlavor::Cloud, "https://test.atlassian.net");

        assert!(relations.parent.is_some());
        assert_eq!(relations.parent.unwrap().key, "jira#PROJ-0");
        assert_eq!(relations.subtasks.len(), 1);
        assert_eq!(relations.subtasks[0].key, "jira#PROJ-2");
        assert_eq!(relations.blocks.len(), 1);
        assert_eq!(relations.blocks[0].issue.key, "jira#PROJ-3");
        assert!(relations.blocked_by.is_empty());
        assert!(relations.related_to.is_empty());
        assert!(relations.duplicates.is_empty());
    }

    // =========================================================================
    // Structure: build_forest_tree tests
    // =========================================================================

    #[test]
    fn test_build_forest_tree_empty() {
        let tree = build_forest_tree(&[], &[]).unwrap();
        assert!(tree.is_empty());
    }

    #[test]
    fn test_build_forest_tree_flat() {
        let rows = vec![
            JiraForestRow {
                id: 1,
                item_id: Some("PROJ-1".into()),
                item_type: Some("issue".into()),
            },
            JiraForestRow {
                id: 2,
                item_id: Some("PROJ-2".into()),
                item_type: Some("issue".into()),
            },
        ];
        let depths = vec![0, 0];
        let tree = build_forest_tree(&rows, &depths).unwrap();
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].row_id, 1);
        assert_eq!(tree[1].row_id, 2);
        assert!(tree[0].children.is_empty());
        assert!(tree[1].children.is_empty());
    }

    #[test]
    fn test_build_forest_tree_rejects_mismatched_lengths() {
        let rows = vec![JiraForestRow {
            id: 1,
            item_id: Some("PROJ-1".into()),
            item_type: None,
        }];
        let depths = vec![0, 1];
        let err = build_forest_tree(&rows, &depths).expect_err("mismatch must be rejected");
        assert!(
            matches!(err, Error::InvalidData(ref msg) if msg.contains("1 rows but 2 depths")),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn test_build_forest_tree_nested() {
        // PROJ-1
        //   PROJ-2
        //     PROJ-3
        //   PROJ-4
        let rows = vec![
            JiraForestRow {
                id: 1,
                item_id: Some("PROJ-1".into()),
                item_type: None,
            },
            JiraForestRow {
                id: 2,
                item_id: Some("PROJ-2".into()),
                item_type: None,
            },
            JiraForestRow {
                id: 3,
                item_id: Some("PROJ-3".into()),
                item_type: None,
            },
            JiraForestRow {
                id: 4,
                item_id: Some("PROJ-4".into()),
                item_type: None,
            },
        ];
        let depths = vec![0, 1, 2, 1];
        let tree = build_forest_tree(&rows, &depths).unwrap();

        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].row_id, 1);
        assert_eq!(tree[0].children.len(), 2);
        assert_eq!(tree[0].children[0].row_id, 2);
        assert_eq!(tree[0].children[0].children.len(), 1);
        assert_eq!(tree[0].children[0].children[0].row_id, 3);
        assert_eq!(tree[0].children[1].row_id, 4);
        assert!(tree[0].children[1].children.is_empty());
    }

    #[test]
    fn test_build_forest_tree_multiple_roots() {
        let rows = vec![
            JiraForestRow {
                id: 1,
                item_id: Some("PROJ-1".into()),
                item_type: None,
            },
            JiraForestRow {
                id: 2,
                item_id: Some("PROJ-2".into()),
                item_type: None,
            },
            JiraForestRow {
                id: 3,
                item_id: Some("PROJ-3".into()),
                item_type: None,
            },
            JiraForestRow {
                id: 4,
                item_id: Some("PROJ-4".into()),
                item_type: None,
            },
        ];
        let depths = vec![0, 1, 0, 1];
        let tree = build_forest_tree(&rows, &depths).unwrap();

        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[1].children.len(), 1);
    }

    // =========================================================================
    // Structure: httpmock integration tests
    // =========================================================================

    mod structure_integration {
        use super::*;
        use devboy_core::StructureRowItem;
        use httpmock::prelude::*;

        fn token(s: &str) -> SecretString {
            SecretString::from(s.to_string())
        }

        fn create_client(server: &MockServer) -> JiraClient {
            // with_base_url sets base_url WITHOUT /rest/api/N,
            // but Structure uses instance_url. Adjust:

            // instance_url is set to base_url by with_base_url
            JiraClient::with_base_url(
                server.base_url(),
                "PROJ",
                "user@example.com",
                token("token"),
                false,
            )
        }

        #[tokio::test]
        async fn test_get_structures() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/rest/structure/2.0/structure");
                then.status(200).json_body(serde_json::json!({
                    "structures": [
                        {"id": 1, "name": "Q1 Planning", "description": "Quarter 1"},
                        {"id": 2, "name": "Sprint Board"}
                    ]
                }));
            });

            let client = create_client(&server);
            let result = client.get_structures().await.unwrap();
            assert_eq!(result.items.len(), 2);
            assert_eq!(result.items[0].name, "Q1 Planning");
            assert_eq!(result.items[1].id, 2);
        }

        #[tokio::test]
        async fn test_get_structure_forest() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST).path("/rest/structure/2.0/forest/1/spec");
                then.status(200).json_body(serde_json::json!({
                    "version": 42,
                    "rows": [
                        {"id": 100, "itemId": "PROJ-1", "itemType": "issue"},
                        {"id": 101, "itemId": "PROJ-2", "itemType": "issue"},
                        {"id": 102, "itemId": "PROJ-3", "itemType": "issue"}
                    ],
                    "depths": [0, 1, 1],
                    "totalCount": 3
                }));
            });

            let client = create_client(&server);
            let forest = client
                .get_structure_forest(
                    1,
                    GetForestOptions {
                        offset: None,
                        limit: Some(200),
                    },
                )
                .await
                .unwrap();

            assert_eq!(forest.version, 42);
            assert_eq!(forest.structure_id, 1);
            assert_eq!(forest.total_count, Some(3));
            assert_eq!(forest.tree.len(), 1); // one root
            assert_eq!(forest.tree[0].item_id, Some("PROJ-1".into()));
            assert_eq!(forest.tree[0].children.len(), 2);
        }

        #[tokio::test]
        async fn test_create_structure() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST).path("/rest/structure/2.0/structure");
                then.status(200).json_body(serde_json::json!({
                    "id": 99,
                    "name": "New Structure",
                    "description": "Test"
                }));
            });

            let client = create_client(&server);
            let result = client
                .create_structure(CreateStructureInput {
                    name: "New Structure".into(),
                    description: Some("Test".into()),
                })
                .await
                .unwrap();

            assert_eq!(result.id, 99);
            assert_eq!(result.name, "New Structure");
        }

        #[tokio::test]
        async fn test_remove_structure_row() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(DELETE)
                    .path("/rest/structure/2.0/forest/1/item/100");
                then.status(204);
            });

            let client = create_client(&server);
            client.remove_structure_row(1, 100).await.unwrap();
        }

        #[tokio::test]
        async fn test_get_structure_views() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/rest/structure/2.0/view")
                    .query_param("structureId", "1");
                then.status(200).json_body(serde_json::json!({
                    "views": [
                        {"id": 10, "name": "Default View", "structureId": 1, "columns": []},
                        {"id": 11, "name": "Sprint View", "structureId": 1, "columns": [
                            {"field": "summary"},
                            {"field": "status"},
                            {"formula": "SUM(\"Story Points\")"}
                        ]}
                    ]
                }));
            });

            let client = create_client(&server);
            let views = client.get_structure_views(1, None).await.unwrap();
            assert_eq!(views.len(), 2);
            assert_eq!(views[1].columns.len(), 3);
        }

        #[tokio::test]
        async fn test_get_structure_views_by_id_accepts_matching_structure() {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/rest/structure/2.0/view/10");
                then.status(200).json_body(serde_json::json!({
                    "id": 10,
                    "name": "Default View",
                    "structureId": 1,
                    "columns": []
                }));
            });

            let client = create_client(&server);
            let views = client.get_structure_views(1, Some(10)).await.unwrap();
            assert_eq!(views.len(), 1);
            assert_eq!(views[0].id, 10);
        }

        #[tokio::test]
        async fn test_get_structure_views_by_id_rejects_cross_structure_view() {
            // View 99 actually lives in structure 7 — a caller who asked
            // for `structureId=1` must see InvalidData, not a surprise
            // view from a different structure.
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/rest/structure/2.0/view/99");
                then.status(200).json_body(serde_json::json!({
                    "id": 99,
                    "name": "Sibling view",
                    "structureId": 7,
                    "columns": []
                }));
            });

            let client = create_client(&server);
            let err = client
                .get_structure_views(1, Some(99))
                .await
                .expect_err("mismatched structure must error");
            match err {
                Error::InvalidData(msg) => {
                    assert!(msg.contains("belongs to structure 7"), "got: {msg}");
                    assert!(msg.contains("but 1 was requested"), "got: {msg}");
                }
                other => panic!("expected InvalidData, got {other:?}"),
            }
        }

        // -----------------------------------------------------------------
        // End-to-end error-body sanitisation through handle_structure_response
        // -----------------------------------------------------------------

        #[tokio::test]
        async fn test_structure_api_404_html_is_sanitised_end_to_end() {
            // Jira returns its generic 404 HTML page when the Structure
            // plugin endpoint is missing. Full flow must come back as
            // NotFound with soft wording + no HTML leak.
            let server = MockServer::start();
            let jira_404_html =
                "<!DOCTYPE html><html><head><title>Oops, you've found a dead link.</title>"
                    .to_string()
                    + &"<script>var a=1;</script>".repeat(100)
                    + "</head><body>404</body></html>";
            server.mock(|when, then| {
                when.method(GET).path("/rest/structure/2.0/structure");
                then.status(404)
                    .header("content-type", "text/html;charset=UTF-8")
                    .body(jira_404_html.clone());
            });

            let client = create_client(&server);
            let err = client
                .get_structures()
                .await
                .expect_err("404 must error out");
            let msg = err.to_string();
            assert!(
                !msg.contains("<!DOCTYPE") && !msg.contains("<script>"),
                "HTML leaked into error message: {}",
                &msg[..msg.len().min(400)]
            );
            assert!(
                msg.contains("endpoint not found"),
                "expected soft wording: {msg}"
            );
        }

        #[tokio::test]
        async fn test_structure_api_xml_404_is_sanitised_end_to_end() {
            // Structure plugin (when installed but endpoint path changed)
            // returns an XML 404 envelope.
            let server = MockServer::start();
            let xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><status><status-code>404</status-code><message>null for uri: /rest/structure/2.0/structure</message></status>"#;
            server.mock(|when, then| {
                when.method(GET).path("/rest/structure/2.0/structure");
                then.status(404)
                    .header("content-type", "application/xml")
                    .body(xml);
            });

            let client = create_client(&server);
            let err = client
                .get_structures()
                .await
                .expect_err("XML 404 must error out");
            let msg = err.to_string();
            assert!(!msg.contains("<?xml"), "XML leaked: {msg}");
            assert!(msg.contains("endpoint not found"));
        }

        #[tokio::test]
        async fn test_structure_api_json_error_forwarded_verbatim() {
            // Concurrent-modification errors from Structure plugin come back
            // as JSON. That body is the real diagnostic — must not be
            // trimmed or replaced with the install hint.
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(PUT).path("/rest/structure/2.0/forest/1/item");
                then.status(409).json_body(serde_json::json!({
                    "errorMessages": ["Forest version conflict"],
                    "errors": {}
                }));
            });

            let client = create_client(&server);
            let err = client
                .add_structure_rows(
                    1,
                    AddStructureRowsInput {
                        items: vec![StructureRowItem {
                            item_id: "PROJ-1".into(),
                            item_type: None,
                        }],
                        under: None,
                        after: None,
                        forest_version: Some(100),
                    },
                )
                .await
                .expect_err("409 must error out");
            let msg = err.to_string();
            assert!(
                msg.contains("Forest version conflict"),
                "JSON dropped: {msg}"
            );
        }

        #[tokio::test]
        async fn test_structure_api_200_with_html_body_does_not_leak() {
            // Rare: Jira replies 200 with an SSO redirect HTML page instead
            // of JSON. Parse-error path must redact the body rather than
            // echo up to 300 chars of HTML.
            let server = MockServer::start();
            let html = "<!DOCTYPE html><html><body>".to_string()
                + &"password=secret".repeat(50)
                + "</body></html>";
            server.mock(|when, then| {
                when.method(GET).path("/rest/structure/2.0/structure");
                then.status(200)
                    .header("content-type", "text/html;charset=UTF-8")
                    .body(html.clone());
            });

            let client = create_client(&server);
            let err = client
                .get_structures()
                .await
                .expect_err("HTML body must fail to parse");
            let msg = err.to_string();
            assert!(
                !msg.contains("password=secret") && !msg.contains("<!DOCTYPE"),
                "HTML body leaked into parse-error message: {}",
                &msg[..msg.len().min(400)]
            );
            assert!(msg.contains("redacted"), "missing redaction marker: {msg}");
        }

        // -----------------------------------------------------------------
        // list_structures_for_metadata — graceful-degrade variant used by
        // the metadata-assembly pipeline (swallows "plugin missing" 404,
        // propagates auth/network failures).
        // -----------------------------------------------------------------

        #[tokio::test]
        async fn test_list_structures_for_metadata_maps_response() {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/rest/structure/2.0/structure");
                then.status(200).json_body(serde_json::json!({
                    "structures": [
                        {"id": 1, "name": "Q1 Planning", "description": "Quarter 1 plan"},
                        {"id": 2, "name": "Sprint Board"}
                    ]
                }));
            });

            let client = create_client(&server);
            let refs = client.list_structures_for_metadata().await.unwrap();

            assert_eq!(refs.len(), 2);
            assert_eq!(refs[0].id, 1);
            assert_eq!(refs[0].name, "Q1 Planning");
            assert_eq!(refs[0].description.as_deref(), Some("Quarter 1 plan"));
            assert_eq!(refs[1].id, 2);
            assert_eq!(refs[1].description, None);
        }

        #[tokio::test]
        async fn test_list_structures_for_metadata_returns_empty_on_plugin_missing() {
            // Structure plugin uninstalled → Jira returns 404 HTML page.
            // `structure_error_from_status` maps this to `Error::NotFound`,
            // which `list_structures_for_metadata` swallows into `Ok(vec![])`
            // so a broader metadata build can continue without try/catch.
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/rest/structure/2.0/structure");
                then.status(404)
                    .header("content-type", "text/html;charset=UTF-8")
                    .body("<!DOCTYPE html><html><title>Oops</title></html>");
            });

            let client = create_client(&server);
            let refs = client.list_structures_for_metadata().await.unwrap();
            assert!(refs.is_empty());
        }

        #[tokio::test]
        async fn test_list_structures_for_metadata_returns_empty_on_200_empty_list() {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/rest/structure/2.0/structure");
                then.status(200)
                    .json_body(serde_json::json!({ "structures": [] }));
            });

            let client = create_client(&server);
            let refs = client.list_structures_for_metadata().await.unwrap();
            assert!(refs.is_empty());
        }

        #[tokio::test]
        async fn test_list_structures_for_metadata_propagates_401() {
            // Bad / expired credentials must surface as an error — otherwise
            // the caller would silently record "no structures" and swallow
            // a real integration misconfiguration.
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/rest/structure/2.0/structure");
                then.status(401).body("Unauthorized");
            });

            let client = create_client(&server);
            let err = client
                .list_structures_for_metadata()
                .await
                .expect_err("401 must not be swallowed");
            assert!(matches!(err, Error::Unauthorized(_)), "got {err:?}");
        }

        #[tokio::test]
        async fn test_list_structures_for_metadata_propagates_403() {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/rest/structure/2.0/structure");
                then.status(403).body("Forbidden");
            });

            let client = create_client(&server);
            let err = client
                .list_structures_for_metadata()
                .await
                .expect_err("403 must not be swallowed");
            assert!(matches!(err, Error::Forbidden(_)), "got {err:?}");
        }

        // =====================================================================
        // Structure generators — issue #179
        // =====================================================================

        #[tokio::test]
        async fn test_structure_generator_lifecycle() {
            let server = MockServer::start();

            // GET list
            server.mock(|when, then| {
                when.method(GET)
                    .path("/rest/structure/2.0/structure/1/generator");
                then.status(200).json_body(serde_json::json!({
                    "generators": [
                        { "id": "g1", "type": "jql", "spec": {"query": "project = PROJ"} }
                    ]
                }));
            });
            // POST add
            server.mock(|when, then| {
                when.method(POST)
                    .path("/rest/structure/2.0/structure/1/generator")
                    .body_includes("\"type\":\"agile-board\"");
                then.status(200).json_body(serde_json::json!({
                    "id": "g2",
                    "type": "agile-board",
                    "spec": {"boardId": 42}
                }));
            });
            // POST sync
            server.mock(|when, then| {
                when.method(POST)
                    .path("/rest/structure/2.0/structure/1/generator/g2/sync");
                then.status(200).json_body(serde_json::json!({}));
            });

            let client = create_client(&server);

            let list = client.get_structure_generators(1).await.unwrap();
            assert_eq!(list.items.len(), 1);
            assert_eq!(list.items[0].generator_type, "jql");

            let added = client
                .add_structure_generator(devboy_core::AddStructureGeneratorInput {
                    structure_id: 1,
                    generator_type: "agile-board".into(),
                    spec: serde_json::json!({"boardId": 42}),
                })
                .await
                .unwrap();
            assert_eq!(added.id, "g2");

            client
                .sync_structure_generator(devboy_core::SyncStructureGeneratorInput {
                    structure_id: 1,
                    generator_id: "g2".into(),
                })
                .await
                .unwrap();
        }

        // =====================================================================
        // Structure delete + automation — issue #180
        // =====================================================================

        #[tokio::test]
        async fn test_delete_structure() {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(DELETE).path("/rest/structure/2.0/structure/7");
                then.status(204);
            });

            let client = create_client(&server);
            client.delete_structure(7).await.unwrap();
        }

        #[tokio::test]
        async fn test_structure_automation() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(PUT)
                    .path("/rest/structure/2.0/structure/5/automation")
                    .body_includes("\"enabled\":true");
                then.status(200).json_body(serde_json::json!({}));
            });
            server.mock(|when, then| {
                when.method(POST)
                    .path("/rest/structure/2.0/structure/5/automation/run");
                then.status(200).json_body(serde_json::json!({}));
            });

            let client = create_client(&server);
            // `automation_id: None` → replaces the whole automation set.
            client
                .update_structure_automation(devboy_core::UpdateStructureAutomationInput {
                    structure_id: 5,
                    automation_id: None,
                    config: serde_json::json!({"enabled": true}),
                })
                .await
                .unwrap();
            client.trigger_structure_automation(5).await.unwrap();
        }

        /// Rule-scoped automation — `automation_id: Some(..)` routes to
        /// `/automation/{id}` (Copilot review on PR #205).
        #[tokio::test]
        async fn test_structure_automation_rule_scoped() {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(PUT)
                    .path("/rest/structure/2.0/structure/5/automation/rule-7")
                    .body_includes("\"action\":\"move\"");
                then.status(200).json_body(serde_json::json!({}));
            });

            let client = create_client(&server);
            client
                .update_structure_automation(devboy_core::UpdateStructureAutomationInput {
                    structure_id: 5,
                    automation_id: Some("rule-7".into()),
                    config: serde_json::json!({"action": "move"}),
                })
                .await
                .unwrap();
        }
    }

    // =====================================================================
    // Agile / Sprint — issue #198
    // =====================================================================
    mod agile_integration {
        use super::*;
        use httpmock::prelude::*;

        fn token(s: &str) -> SecretString {
            SecretString::from(s.to_string())
        }

        fn create_client(server: &MockServer) -> JiraClient {
            JiraClient::with_base_url(
                server.base_url(),
                "PROJ",
                "user@example.com",
                token("token"),
                false,
            )
        }

        #[tokio::test]
        async fn test_get_board_sprints_active() {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET)
                    .path("/rest/agile/1.0/board/10/sprint")
                    .query_param("state", "active");
                then.status(200).json_body(serde_json::json!({
                    "isLast": true,
                    "values": [
                        {
                            "id": 1,
                            "name": "Sprint 1",
                            "state": "active",
                            "originBoardId": 10,
                            "startDate": "2026-04-01T00:00:00.000Z"
                        }
                    ]
                }));
            });

            let client = create_client(&server);
            let sprints = client
                .get_board_sprints(10, devboy_core::SprintState::Active)
                .await
                .unwrap();
            assert_eq!(sprints.items.len(), 1);
            assert_eq!(sprints.items[0].state, "active");
            assert_eq!(sprints.items[0].origin_board_id, Some(10));
        }

        /// Codex P2 review on PR #205 — `/board/{id}/sprint` is paginated;
        /// we must walk `startAt` until `isLast: true`.
        #[tokio::test]
        async fn test_get_board_sprints_walks_pagination() {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET)
                    .path("/rest/agile/1.0/board/10/sprint")
                    .query_param("startAt", "0");
                then.status(200).json_body(serde_json::json!({
                    "isLast": false,
                    "values": [
                        {"id": 1, "name": "S1", "state": "closed"},
                        {"id": 2, "name": "S2", "state": "closed"}
                    ]
                }));
            });
            server.mock(|when, then| {
                when.method(GET)
                    .path("/rest/agile/1.0/board/10/sprint")
                    .query_param("startAt", "2");
                then.status(200).json_body(serde_json::json!({
                    "isLast": true,
                    "values": [
                        {"id": 3, "name": "S3", "state": "active"}
                    ]
                }));
            });

            let client = create_client(&server);
            let sprints = client
                .get_board_sprints(10, devboy_core::SprintState::All)
                .await
                .unwrap();
            assert_eq!(sprints.items.len(), 3);
            assert_eq!(sprints.items[2].name, "S3");
        }

        #[tokio::test]
        async fn test_get_board_sprints_all_omits_state() {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET)
                    .path("/rest/agile/1.0/board/10/sprint")
                    .is_true(|req| req.query_params().iter().all(|(k, _)| k != "state"));
                then.status(200)
                    .json_body(serde_json::json!({"values": []}));
            });

            let client = create_client(&server);
            let sprints = client
                .get_board_sprints(10, devboy_core::SprintState::All)
                .await
                .unwrap();
            assert_eq!(sprints.items.len(), 0);
        }

        #[tokio::test]
        async fn test_assign_to_sprint_strips_jira_prefix() {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(POST)
                    .path("/rest/agile/1.0/sprint/42/issue")
                    .body_includes("\"issues\":[\"PROJ-1\",\"PROJ-2\"]");
                then.status(204);
            });

            let client = create_client(&server);
            client
                .assign_to_sprint(devboy_core::AssignToSprintInput {
                    sprint_id: 42,
                    issue_keys: vec!["jira#PROJ-1".to_string(), "PROJ-2".to_string()],
                })
                .await
                .unwrap();
        }
    }

    // =====================================================================
    // Project Versions / fixVersion — issue #238
    // =====================================================================
    mod versions_integration {
        use super::*;
        use devboy_core::{ListProjectVersionsParams, UpsertProjectVersionInput};
        use httpmock::prelude::*;

        fn token(s: &str) -> SecretString {
            SecretString::from(s.to_string())
        }

        fn create_client(server: &MockServer) -> JiraClient {
            JiraClient::with_base_url(
                server.base_url(),
                "PROJ",
                "user@example.com",
                token("pat-token"),
                false,
            )
        }

        fn create_cloud_client(server: &MockServer) -> JiraClient {
            JiraClient::with_base_url(
                server.base_url(),
                "PROJ",
                "user@example.com",
                token("api-token"),
                true,
            )
        }

        fn version_dto(
            id: &str,
            name: &str,
            release_date: Option<&str>,
            released: bool,
            archived: bool,
        ) -> serde_json::Value {
            let mut v = serde_json::json!({
                "id": id,
                "name": name,
                "project": "PROJ",
                "released": released,
                "archived": archived,
            });
            if let Some(d) = release_date {
                v["releaseDate"] = serde_json::json!(d);
            }
            v
        }

        #[tokio::test]
        async fn list_project_versions_returns_rich_payload() {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/project/PROJ/versions");
                then.status(200).json_body(serde_json::json!([
                    {
                        "id": "10001",
                        "name": "1.0.0",
                        "project": "PROJ",
                        "description": "Initial release",
                        "startDate": "2025-01-01",
                        "releaseDate": "2025-02-01",
                        "released": true,
                        "archived": false,
                        "overdue": false,
                    },
                    version_dto("10002", "2.0.0", Some("2026-04-01"), false, false),
                    version_dto("10003", "0.9.0", Some("2024-06-01"), true, true),
                ]));
            });

            let client = create_client(&server);
            let result = client
                .list_project_versions(ListProjectVersionsParams {
                    project: "PROJ".into(),
                    released: None,
                    archived: None,
                    limit: None,
                    include_issue_count: false,
                })
                .await
                .unwrap();

            assert_eq!(result.items.len(), 3);
            // Sorted by release_date desc
            assert_eq!(result.items[0].name, "2.0.0");
            assert_eq!(result.items[1].name, "1.0.0");
            assert_eq!(result.items[2].name, "0.9.0");
            assert_eq!(
                result.items[1].description.as_deref(),
                Some("Initial release")
            );
            assert_eq!(result.items[1].source, "jira");
        }

        #[tokio::test]
        async fn list_project_versions_filters_archived_and_released() {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/project/PROJ/versions");
                then.status(200).json_body(serde_json::json!([
                    version_dto("1", "current", Some("2026-04-01"), false, false),
                    version_dto("2", "shipped", Some("2025-12-01"), true, false),
                    version_dto("3", "old", Some("2024-01-01"), true, true),
                ]));
            });

            let client = create_client(&server);

            let unreleased_only = client
                .list_project_versions(ListProjectVersionsParams {
                    project: "PROJ".into(),
                    released: Some(false),
                    archived: Some(false),
                    limit: None,
                    include_issue_count: false,
                })
                .await
                .unwrap();
            assert_eq!(unreleased_only.items.len(), 1);
            assert_eq!(unreleased_only.items[0].name, "current");

            // Re-mock for the second call (httpmock mocks are per-server,
            // and the previous mock matches all GETs to that path).
        }

        #[tokio::test]
        async fn list_project_versions_applies_limit_and_keeps_most_recent() {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/project/PROJ/versions");
                then.status(200).json_body(serde_json::json!([
                    version_dto("1", "v1", Some("2024-01-01"), true, false),
                    version_dto("2", "v2", Some("2025-01-01"), true, false),
                    version_dto("3", "v3", Some("2026-01-01"), true, false),
                    version_dto("4", "v4", Some("2026-02-01"), false, false),
                ]));
            });

            let client = create_client(&server);
            let result = client
                .list_project_versions(ListProjectVersionsParams {
                    project: "PROJ".into(),
                    released: None,
                    archived: None,
                    limit: Some(2),
                    include_issue_count: false,
                })
                .await
                .unwrap();
            assert_eq!(result.items.len(), 2);
            assert_eq!(result.items[0].name, "v4");
            assert_eq!(result.items[1].name, "v3");
        }

        #[tokio::test]
        async fn list_project_versions_passes_expand_query_on_cloud() {
            // Cloud responds to `?expand=issuesstatus` with a per-status
            // breakdown we can sum into `issue_count`. Server/DC ignores
            // the param, so the gate applies only to Cloud — see the
            // sibling `omits_expand_on_self_hosted` test below.
            let server = MockServer::start();
            let mock = server.mock(|when, then| {
                when.method(GET)
                    .path("/project/PROJ/versions")
                    .query_param("expand", "issuesstatus");
                then.status(200).json_body(serde_json::json!([
                    {
                        "id": "1",
                        "name": "v1",
                        "released": false,
                        "archived": false,
                        "issuesStatusForFixVersion": {
                            "unmapped": 0,
                            "toDo": 5,
                            "inProgress": 3,
                            "done": 2
                        }
                    }
                ]));
            });

            let client = create_cloud_client(&server);
            let result = client
                .list_project_versions(ListProjectVersionsParams {
                    project: "PROJ".into(),
                    released: None,
                    archived: None,
                    limit: None,
                    include_issue_count: true,
                })
                .await
                .unwrap();
            mock.assert();
            assert_eq!(result.items.len(), 1);
            assert_eq!(result.items[0].issue_count, Some(10));
        }

        #[tokio::test]
        async fn list_project_versions_omits_expand_on_self_hosted() {
            // Copilot review on PR #239 — the expand parameter is a Cloud
            // payload extension. We don't want to bake "Server/DC silently
            // ignores Cloud query params" into the URL contract, so on
            // Self-Hosted the client must not append `?expand=...` even
            // when the caller asks for issue counts.
            let server = MockServer::start();
            let bare_mock = server.mock(|when, then| {
                when.method(GET).path("/project/PROJ/versions");
                then.status(200).json_body(serde_json::json!([{
                    "id": "1",
                    "name": "v1",
                    "released": false,
                    "archived": false,
                    "issuesUnresolvedCount": 4,
                }]));
            });
            let expanded_mock = server.mock(|when, then| {
                when.method(GET)
                    .path("/project/PROJ/versions")
                    .query_param("expand", "issuesstatus");
                then.status(500); // would fail the test if we hit it
            });

            let client = create_client(&server); // self-hosted
            let result = client
                .list_project_versions(ListProjectVersionsParams {
                    project: "PROJ".into(),
                    released: None,
                    archived: None,
                    limit: None,
                    include_issue_count: true,
                })
                .await
                .unwrap();
            bare_mock.assert();
            expanded_mock.assert_calls(0);
            // On Self-Hosted we don't have a true total — it goes into
            // `unresolved_issue_count` rather than `issue_count` to keep
            // the two flavors comparable (Codex review on PR #239).
            assert_eq!(result.items[0].issue_count, None);
            assert_eq!(result.items[0].unresolved_issue_count, Some(4));
        }

        #[tokio::test]
        async fn list_project_versions_orders_unreleased_first_then_recent() {
            // Copilot review #3 on PR #239 — unreleased versions are the
            // ones the agent is actually working on, they must surface
            // before history regardless of date.
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/project/PROJ/versions");
                then.status(200).json_body(serde_json::json!([
                    version_dto("1", "9.10.0", Some("2026-04-01"), true, false),
                    version_dto("2", "10.0.0", Some("2026-04-02"), false, false),
                    version_dto("3", "next", None, false, false),
                    version_dto("4", "1.0.0", Some("2024-01-01"), true, true),
                ]));
            });

            let client = create_client(&server);
            let result = client
                .list_project_versions(ListProjectVersionsParams {
                    project: "PROJ".into(),
                    released: None,
                    archived: None,
                    limit: None,
                    include_issue_count: false,
                })
                .await
                .unwrap();
            // Unreleased first: undated `next` then dated `10.0.0`.
            // Released after: `9.10.0` (newer date) then `1.0.0` (older).
            let names: Vec<_> = result.items.iter().map(|v| v.name.as_str()).collect();
            assert_eq!(names, vec!["next", "10.0.0", "9.10.0", "1.0.0"]);
        }

        #[tokio::test]
        async fn list_project_versions_pagination_reflects_truncation() {
            // Copilot review #4 on PR #239 — without total/has_more the
            // formatter can't render a "+N more" hint.
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/project/PROJ/versions");
                then.status(200).json_body(serde_json::json!([
                    version_dto("1", "v1", Some("2024-01-01"), true, false),
                    version_dto("2", "v2", Some("2025-01-01"), true, false),
                    version_dto("3", "v3", Some("2026-01-01"), true, false),
                ]));
            });

            let client = create_client(&server);
            let result = client
                .list_project_versions(ListProjectVersionsParams {
                    project: "PROJ".into(),
                    released: None,
                    archived: None,
                    limit: Some(2),
                    include_issue_count: false,
                })
                .await
                .unwrap();
            let p = result.pagination.expect("pagination must be set");
            assert_eq!(p.total, Some(3));
            assert_eq!(p.limit, 2);
            assert!(p.has_more);

            // No truncation → has_more is false.
            let server2 = MockServer::start();
            server2.mock(|when, then| {
                when.method(GET).path("/project/PROJ/versions");
                then.status(200).json_body(serde_json::json!([version_dto(
                    "1",
                    "v1",
                    Some("2024-01-01"),
                    true,
                    false
                ),]));
            });
            let client2 = create_client(&server2);
            let result2 = client2
                .list_project_versions(ListProjectVersionsParams {
                    project: "PROJ".into(),
                    released: None,
                    archived: None,
                    limit: Some(20),
                    include_issue_count: false,
                })
                .await
                .unwrap();
            let p2 = result2.pagination.unwrap();
            assert_eq!(p2.total, Some(1));
            assert!(!p2.has_more);
        }

        #[test]
        fn compare_version_names_handles_semver_and_alpha() {
            use std::cmp::Ordering;
            assert_eq!(compare_version_names("10.0.0", "9.10.0"), Ordering::Greater);
            assert_eq!(compare_version_names("1.0.0", "1.0.0"), Ordering::Equal);
            assert_eq!(compare_version_names("1.0.10", "1.0.2"), Ordering::Greater);
            // Pre-release < release at the same numeric prefix.
            assert_eq!(compare_version_names("1.0.0-rc1", "1.0.0"), Ordering::Less);
            // Non-semver names fall back to lexicographic / token compare,
            // but at minimum they must be a total order.
            let _ = compare_version_names("Sprint 42 cleanup", "Sprint 9 cleanup");
        }

        #[tokio::test]
        async fn upsert_project_version_creates_when_missing() {
            let server = MockServer::start();
            // 1) list returns no match
            server.mock(|when, then| {
                when.method(GET).path("/project/PROJ/versions");
                then.status(200).json_body(serde_json::json!([version_dto(
                    "99",
                    "1.0.0",
                    Some("2025-01-01"),
                    true,
                    false
                ),]));
            });
            // 2) POST /version creates
            server.mock(|when, then| {
                when.method(POST)
                    .path("/version")
                    .body_includes("\"name\":\"3.18.0\"")
                    .body_includes("\"project\":\"PROJ\"")
                    .body_includes("\"description\":\"Release notes draft\"");
                then.status(201).json_body(serde_json::json!({
                    "id": "10500",
                    "name": "3.18.0",
                    "project": "PROJ",
                    "description": "Release notes draft",
                    "released": false,
                    "archived": false,
                }));
            });

            let client = create_client(&server);
            let v = client
                .upsert_project_version(UpsertProjectVersionInput {
                    project: "PROJ".into(),
                    name: "3.18.0".into(),
                    description: Some("Release notes draft".into()),
                    start_date: None,
                    release_date: None,
                    released: None,
                    archived: None,
                })
                .await
                .unwrap();
            assert_eq!(v.id, "10500");
            assert_eq!(v.name, "3.18.0");
            assert_eq!(v.description.as_deref(), Some("Release notes draft"));
        }

        #[tokio::test]
        async fn upsert_project_version_updates_when_present() {
            let server = MockServer::start();
            // 1) list returns match
            server.mock(|when, then| {
                when.method(GET).path("/project/PROJ/versions");
                then.status(200).json_body(serde_json::json!([version_dto(
                    "777", "3.18.0", None, false, false
                ),]));
            });
            // 2) PUT /version/{id}
            server.mock(|when, then| {
                when.method(PUT)
                    .path("/version/777")
                    .body_includes("\"description\":\"final notes\"")
                    .body_includes("\"released\":true")
                    .body_includes("\"releaseDate\":\"2026-05-01\"");
                then.status(200).json_body(serde_json::json!({
                    "id": "777",
                    "name": "3.18.0",
                    "project": "PROJ",
                    "description": "final notes",
                    "releaseDate": "2026-05-01",
                    "released": true,
                    "archived": false,
                }));
            });

            let client = create_client(&server);
            let v = client
                .upsert_project_version(UpsertProjectVersionInput {
                    project: "PROJ".into(),
                    name: "3.18.0".into(),
                    description: Some("final notes".into()),
                    start_date: None,
                    release_date: Some("2026-05-01".into()),
                    released: Some(true),
                    archived: None,
                })
                .await
                .unwrap();
            assert_eq!(v.id, "777");
            assert!(v.released);
            assert_eq!(v.release_date.as_deref(), Some("2026-05-01"));
        }

        #[tokio::test]
        async fn upsert_project_version_partial_update_sends_only_description() {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/project/PROJ/versions");
                then.status(200).json_body(serde_json::json!([version_dto(
                    "42",
                    "2.0.0",
                    Some("2026-01-01"),
                    false,
                    false
                ),]));
            });
            // PUT body should include description; `name`, `released`,
            // `archived`, and date fields stay out (serde skip_if = None).
            let put_mock = server.mock(|when, then| {
                when.method(PUT)
                    .path("/version/42")
                    .body_includes("\"description\":\"draft\"")
                    .body_excludes("\"name\":")
                    .body_excludes("\"released\":")
                    .body_excludes("\"archived\":")
                    .body_excludes("\"releaseDate\":");
                then.status(200).json_body(serde_json::json!({
                    "id": "42",
                    "name": "2.0.0",
                    "project": "PROJ",
                    "description": "draft",
                    "releaseDate": "2026-01-01",
                    "released": false,
                    "archived": false,
                }));
            });

            let client = create_client(&server);
            client
                .upsert_project_version(UpsertProjectVersionInput {
                    project: "PROJ".into(),
                    name: "2.0.0".into(),
                    description: Some("draft".into()),
                    start_date: None,
                    release_date: None,
                    released: None,
                    archived: None,
                })
                .await
                .unwrap();
            put_mock.assert();
        }

        #[tokio::test]
        async fn upsert_project_version_rejects_empty_name() {
            let server = MockServer::start();
            let client = create_client(&server);
            let err = client
                .upsert_project_version(UpsertProjectVersionInput {
                    project: "PROJ".into(),
                    name: "  ".into(),
                    ..Default::default()
                })
                .await
                .unwrap_err();
            assert!(matches!(err, devboy_core::Error::InvalidData(_)));
        }

        #[tokio::test]
        async fn upsert_project_version_rejects_overlong_name() {
            // Codex review on PR #239 — Jira caps version names at 255
            // chars; failing client-side gives a clearer error than
            // letting the server return a 400.
            let server = MockServer::start();
            let client = create_client(&server);
            let err = client
                .upsert_project_version(UpsertProjectVersionInput {
                    project: "PROJ".into(),
                    name: "x".repeat(256),
                    ..Default::default()
                })
                .await
                .unwrap_err();
            assert!(matches!(err, devboy_core::Error::InvalidData(_)));
        }

        #[test]
        fn duplicate_version_error_classifier_matches_jira_phrasing() {
            // Codex review on PR #239 — race recovery hangs on this
            // classifier. Pin the strings Jira actually returns so a
            // copy-paste regression in the wording table doesn't
            // silently turn duplicate errors into hard failures.
            let dup1 = devboy_core::Error::Api {
                status: 400,
                message: "A version with this name already exists in this project.".into(),
            };
            let dup2 = devboy_core::Error::Api {
                status: 400,
                message: "Name is already used by another version in this project.".into(),
            };
            let unrelated = devboy_core::Error::Api {
                status: 400,
                message: "releaseDate is in the wrong format.".into(),
            };
            assert!(is_duplicate_version_error(&dup1));
            assert!(is_duplicate_version_error(&dup2));
            assert!(!is_duplicate_version_error(&unrelated));
        }

        #[tokio::test]
        async fn upsert_project_version_propagates_non_duplicate_400() {
            // Make sure the duplicate-recovery path doesn't swallow
            // unrelated 400s — only "already exists" is retried.
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/project/PROJ/versions");
                then.status(200).json_body(serde_json::json!([]));
            });
            server.mock(|when, then| {
                when.method(POST).path("/version");
                then.status(400).json_body(serde_json::json!({
                    "errorMessages": ["releaseDate is in the wrong format."]
                }));
            });
            let client = create_client(&server);
            let err = client
                .upsert_project_version(UpsertProjectVersionInput {
                    project: "PROJ".into(),
                    name: "3.18.0".into(),
                    release_date: Some("not-a-date".into()),
                    ..Default::default()
                })
                .await
                .unwrap_err();
            // 400 → Error::Api (see Error::from_status).
            assert!(matches!(err, devboy_core::Error::Api { .. }));
        }

        #[tokio::test]
        async fn upsert_project_version_works_on_cloud_flavor() {
            // Codex review on PR #239 — coverage gap: every upsert test
            // ran against self-hosted. Pin Cloud insert path to make
            // sure the same code works against Cloud's response shape.
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(GET).path("/project/CLOUDPROJ/versions");
                then.status(200).json_body(serde_json::json!([]));
            });
            let post_mock = server.mock(|when, then| {
                when.method(POST)
                    .path("/version")
                    .body_includes("\"name\":\"4.0.0\"")
                    .body_includes("\"project\":\"CLOUDPROJ\"");
                then.status(201).json_body(serde_json::json!({
                    "id": "30001",
                    "name": "4.0.0",
                    "project": "CLOUDPROJ",
                    "description": "Cloud release",
                    "released": false,
                    "archived": false,
                    // Cloud-shaped issuesStatusForFixVersion would normally
                    // not appear on the create response — the field
                    // surfaces via list_project_versions(includeIssueCount).
                }));
            });

            let client = create_cloud_client(&server);
            let v = client
                .upsert_project_version(UpsertProjectVersionInput {
                    project: "CLOUDPROJ".into(),
                    name: "4.0.0".into(),
                    description: Some("Cloud release".into()),
                    ..Default::default()
                })
                .await
                .unwrap();
            post_mock.assert();
            assert_eq!(v.id, "30001");
            assert_eq!(v.project, "CLOUDPROJ");
            assert_eq!(v.description.as_deref(), Some("Cloud release"));
        }
    }
}
