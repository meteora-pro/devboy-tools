//! Jira API client implementation.
//!
//! Supports both Jira Cloud (API v3) and Jira Self-Hosted/Data Center (API v2).
//! Flavor is auto-detected from the URL: `*.atlassian.net` → Cloud, otherwise → SelfHosted.

use async_trait::async_trait;
use devboy_core::{
    Comment, CreateCommentInput, CreateIssueInput, Discussion, Error, FileDiff, Issue, IssueFilter,
    IssueProvider, MergeRequest, MergeRequestProvider, MrFilter, Provider, Result,
    UpdateIssueInput, User,
};
use tracing::{debug, warn};

use crate::types::{
    AddCommentPayload, CreateIssueFields, CreateIssuePayload, CreateIssueResponse, IssueType,
    JiraCloudSearchResponse, JiraComment, JiraCommentsResponse, JiraIssue, JiraIssueTypeStatuses,
    JiraPriority, JiraProjectStatus, JiraSearchResponse, JiraStatus, JiraTransition,
    JiraTransitionsResponse, JiraUser, PriorityName, ProjectKey, TransitionId, TransitionPayload,
    UpdateIssueFields, UpdateIssuePayload,
};

/// Jira deployment flavor.
#[derive(Debug, Clone, Copy, PartialEq)]
enum JiraFlavor {
    /// Jira Cloud — API v3, ADF format, accountId-based users
    Cloud,
    /// Jira Self-Hosted / Data Center — API v2, plain text, username-based users
    SelfHosted,
}

/// Jira API client.
pub struct JiraClient {
    base_url: String,
    project_key: String,
    email: String,
    token: String,
    flavor: JiraFlavor,
    client: reqwest::Client,
}

impl JiraClient {
    /// Create a new Jira client. Flavor is auto-detected from the URL.
    pub fn new(
        url: impl Into<String>,
        project_key: impl Into<String>,
        email: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        let url = url.into();
        let flavor = detect_flavor(&url);
        let api_base = build_api_base(&url, flavor);
        Self {
            base_url: api_base,
            project_key: project_key.into(),
            email: email.into(),
            token: token.into(),
            flavor,
            client: reqwest::Client::builder()
                .user_agent("devboy-tools")
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Create a new Jira client with explicit base URL (for testing with httpmock).
    /// The base URL is used as-is (no `/rest/api/N` suffix appended).
    pub fn with_base_url(
        base_url: impl Into<String>,
        project_key: impl Into<String>,
        email: impl Into<String>,
        token: impl Into<String>,
        flavor: bool, // true = Cloud, false = SelfHosted
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            project_key: project_key.into(),
            email: email.into(),
            token: token.into(),
            flavor: if flavor {
                JiraFlavor::Cloud
            } else {
                JiraFlavor::SelfHosted
            },
            client: reqwest::Client::builder()
                .user_agent("devboy-tools")
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Build request with auth header.
    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let builder = self
            .client
            .request(method, url)
            .header("Content-Type", "application/json");

        match self.flavor {
            JiraFlavor::Cloud => {
                // Cloud: Basic auth with email:token
                let credentials = base64_encode(&format!("{}:{}", self.email, self.token));
                builder.header("Authorization", format!("Basic {}", credentials))
            }
            JiraFlavor::SelfHosted => {
                if self.token.contains(':') {
                    // user:password format — Basic auth
                    let credentials = base64_encode(&self.token);
                    builder.header("Authorization", format!("Basic {}", credentials))
                } else {
                    // Personal Access Token — Bearer auth
                    builder.header("Authorization", format!("Bearer {}", self.token))
                }
            }
        }
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

        response
            .json()
            .await
            .map_err(|e| Error::InvalidData(format!("Failed to parse response: {}", e)))
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
                if text.is_empty() {
                    None
                } else {
                    Some(text)
                }
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
    }
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

/// Get the Jira instance URL from the API base URL.
fn instance_url_from_base(base_url: &str) -> String {
    base_url
        .trim_end_matches("/rest/api/3")
        .trim_end_matches("/rest/api/2")
        .to_string()
}

// =============================================================================
// Trait implementations
// =============================================================================

#[async_trait]
impl IssueProvider for JiraClient {
    async fn get_issues(&self, filter: IssueFilter) -> Result<Vec<Issue>> {
        let limit = filter.limit.unwrap_or(20);
        if limit == 0 {
            return Ok(vec![]);
        }
        let offset = filter.offset.unwrap_or(0);

        // Build JQL query
        let mut jql_parts: Vec<String> = vec![format!("project = \"{}\"", self.project_key)];

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
                    jql_parts.push(format!("status = \"{}\"", other));
                }
            }
        }

        if let Some(search) = &filter.search {
            jql_parts.push(format!("summary ~ \"{}\"", search));
        }

        if let Some(labels) = &filter.labels {
            for label in labels {
                jql_parts.push(format!("labels = \"{}\"", label));
            }
        }

        if let Some(assignee) = &filter.assignee {
            jql_parts.push(format!("assignee = \"{}\"", assignee));
        }

        let jql = jql_parts.join(" AND ");

        // Add ORDER BY
        let order_by = match filter.sort_by.as_deref() {
            Some("created_at" | "created") => "created",
            Some("priority") => "priority",
            _ => "updated",
        };
        let order = match filter.sort_order.as_deref() {
            Some("asc") => "ASC",
            _ => "DESC",
        };
        let jql_with_order = format!("{} ORDER BY {} {}", jql, order_by, order);

        let instance_url = instance_url_from_base(&self.base_url);

        match self.flavor {
            JiraFlavor::Cloud => {
                // Cloud: GET /search/jql?jql=...&maxResults=...&nextPageToken=...
                let url = format!("{}/search/jql", self.base_url);

                let mut all_issues: Vec<Issue> = Vec::new();
                let mut next_page_token: Option<String> = None;
                let total_needed = offset + limit;
                let mut fetched_count = 0u32;

                loop {
                    let mut params: Vec<(&str, String)> = vec![
                        ("jql", jql_with_order.clone()),
                        ("maxResults", std::cmp::min(limit, 50).to_string()),
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
                            all_issues.push(map_issue(issue, self.flavor, &instance_url));
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

                Ok(all_issues)
            }
            JiraFlavor::SelfHosted => {
                // Self-Hosted: GET /search?jql=...&startAt=...&maxResults=...
                let url = format!("{}/search", self.base_url);

                let params: Vec<(&str, String)> = vec![
                    ("jql", jql_with_order),
                    ("startAt", offset.to_string()),
                    ("maxResults", limit.to_string()),
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

                let issues = search_resp
                    .issues
                    .iter()
                    .map(|i| map_issue(i, self.flavor, &instance_url))
                    .collect();

                Ok(issues)
            }
        }
    }

    async fn get_issue(&self, key: &str) -> Result<Issue> {
        let jira_key = parse_jira_key(key);
        let url = format!("{}/issue/{}", self.base_url, jira_key);
        let issue: JiraIssue = self.get(&url).await?;
        let instance_url = instance_url_from_base(&self.base_url);
        Ok(map_issue(&issue, self.flavor, &instance_url))
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

        let payload = CreateIssuePayload {
            fields: CreateIssueFields {
                project: ProjectKey {
                    key: self.project_key.clone(),
                },
                summary: input.title,
                issuetype: IssueType {
                    name: "Task".to_string(),
                },
                description,
                labels,
                priority,
                assignee,
            },
        };

        let url = format!("{}/issue", self.base_url);
        let create_resp: CreateIssueResponse = self.post(&url, &payload).await?;

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

        let fields = UpdateIssueFields {
            summary: input.title,
            description,
            labels,
            priority,
            assignee,
        };

        // Only call PUT if there are field updates
        let has_field_updates = fields.summary.is_some()
            || fields.description.is_some()
            || fields.labels.is_some()
            || fields.priority.is_some()
            || fields.assignee.is_some();

        if has_field_updates {
            let url = format!("{}/issue/{}", self.base_url, jira_key);
            let payload = UpdateIssuePayload { fields };
            self.put(&url, &payload).await?;
        }

        // Handle status change via transitions
        if let Some(state) = &input.state {
            self.transition_issue(jira_key, state).await?;
        }

        // Fetch updated issue
        self.get_issue(jira_key).await
    }

    async fn get_comments(&self, issue_key: &str) -> Result<Vec<Comment>> {
        let jira_key = parse_jira_key(issue_key);
        let url = format!("{}/issue/{}/comment", self.base_url, jira_key);
        let response: JiraCommentsResponse = self.get(&url).await?;
        Ok(response
            .comments
            .iter()
            .map(|c| map_comment(c, self.flavor))
            .collect())
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

    fn provider_name(&self) -> &'static str {
        "jira"
    }
}

#[async_trait]
impl MergeRequestProvider for JiraClient {
    async fn get_merge_requests(&self, _filter: MrFilter) -> Result<Vec<MergeRequest>> {
        Err(Error::ProviderUnsupported {
            provider: "jira".to_string(),
            operation: "get_merge_requests".to_string(),
        })
    }

    async fn get_merge_request(&self, _key: &str) -> Result<MergeRequest> {
        Err(Error::ProviderUnsupported {
            provider: "jira".to_string(),
            operation: "get_merge_request".to_string(),
        })
    }

    async fn get_discussions(&self, _mr_key: &str) -> Result<Vec<Discussion>> {
        Err(Error::ProviderUnsupported {
            provider: "jira".to_string(),
            operation: "get_discussions".to_string(),
        })
    }

    async fn get_diffs(&self, _mr_key: &str) -> Result<Vec<FileDiff>> {
        Err(Error::ProviderUnsupported {
            provider: "jira".to_string(),
            operation: "get_diffs".to_string(),
        })
    }

    async fn add_comment(&self, _mr_key: &str, _input: CreateCommentInput) -> Result<Comment> {
        Err(Error::ProviderUnsupported {
            provider: "jira".to_string(),
            operation: "add_merge_request_comment".to_string(),
        })
    }

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

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

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
            "api-token-123",
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
            "personal-access-token",
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
            "user:password",
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
            "token",
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

        fn create_self_hosted_client(server: &MockServer) -> JiraClient {
            JiraClient::with_base_url(
                server.base_url(),
                "PROJ",
                "user@example.com",
                "pat-token",
                false,
            )
        }

        fn create_cloud_client(server: &MockServer) -> JiraClient {
            JiraClient::with_base_url(
                server.base_url(),
                "PROJ",
                "user@example.com",
                "api-token",
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
            let issues = client.get_issues(IssueFilter::default()).await.unwrap();

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
                .unwrap();

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
                .unwrap();

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
                    labels: vec![],
                    assignees: vec![],
                    priority: None,
                })
                .await
                .unwrap();

            assert_eq!(issue.key, "jira#PROJ-2");
            assert_eq!(issue.title, "New task");
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
            let comments = client.get_comments("PROJ-1").await.unwrap();

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
            let issues = client.get_issues(IssueFilter::default()).await.unwrap();

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
                "token",
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
    }
}
