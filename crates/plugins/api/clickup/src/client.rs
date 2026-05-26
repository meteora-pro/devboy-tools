//! ClickUp API client implementation.

use async_trait::async_trait;
use devboy_core::{
    AssetCapabilities, AssetMeta, Comment, ContextCapabilities, CreateIssueInput, Error, Issue,
    IssueFilter, IssueLink, IssueProvider, IssueRelations, IssueStatus, MergeRequestProvider,
    PipelineProvider, Provider, ProviderResult, Result, SortInfo, SortOrder, UpdateIssueInput,
    User,
};
use secrecy::{ExposeSecret, SecretString};
use tracing::{debug, warn};

use crate::DEFAULT_CLICKUP_URL;
use crate::types::{
    ClickUpAttachment, ClickUpComment, ClickUpCommentList, ClickUpLinkedTask, ClickUpListInfo,
    ClickUpPriority, ClickUpTask, ClickUpTaskList, ClickUpUser, CreateCommentRequest,
    CreateCommentResponse, CreateTaskRequest, UpdateTaskRequest,
};

/// Maximum number of tasks per page in ClickUp API.
const PAGE_SIZE: u32 = 100;

/// Percent-encode a tag name for use in ClickUp URL paths.
fn encode_tag(tag: &str) -> String {
    urlencoding::encode(tag).into_owned()
}

pub struct ClickUpClient {
    base_url: String,
    list_id: String,
    team_id: Option<String>,
    token: SecretString,
    client: reqwest::Client,
}

impl ClickUpClient {
    /// Create a new ClickUp client.
    pub fn new(list_id: impl Into<String>, token: SecretString) -> Self {
        Self::with_base_url(DEFAULT_CLICKUP_URL, list_id, token)
    }

    /// Create a new ClickUp client with a custom base URL (for testing).
    pub fn with_base_url(
        base_url: impl Into<String>,
        list_id: impl Into<String>,
        token: SecretString,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            list_id: list_id.into(),
            team_id: None,
            token,
            client: reqwest::Client::builder()
                .user_agent("devboy-tools")
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    /// Set team (workspace) ID — required for custom task ID resolution.
    pub fn with_team_id(mut self, team_id: impl Into<String>) -> Self {
        self.team_id = Some(team_id.into());
        self
    }

    /// Build request with common headers.
    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, url)
            .header("Authorization", self.token.expose_secret())
            .header("Content-Type", "application/json")
    }

    /// Make an authenticated GET request.
    async fn get<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        debug!(url = url, "ClickUp GET request");

        let response = self
            .request(reqwest::Method::GET, url)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        self.handle_response(response).await
    }

    /// Make an authenticated GET request with properly encoded query parameters.
    async fn get_with_query<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        params: &[(&str, &str)],
    ) -> Result<T> {
        debug!(url = url, params = ?params, "ClickUp GET request with query");

        let response = self
            .request(reqwest::Method::GET, url)
            .query(params)
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
        debug!(url = url, "ClickUp POST request");

        let response = self
            .request(reqwest::Method::POST, url)
            .json(body)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        self.handle_response(response).await
    }

    /// Make an authenticated PUT request.
    async fn put<T: serde::de::DeserializeOwned, B: serde::Serialize>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T> {
        debug!(url = url, "ClickUp PUT request");

        let response = self
            .request(reqwest::Method::PUT, url)
            .json(body)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        self.handle_response(response).await
    }

    /// Make an authenticated DELETE request.
    /// Returns `Ok(())` on success. Treats 404 as success (idempotent delete).
    async fn delete(&self, url: &str) -> Result<()> {
        debug!(url = url, "ClickUp DELETE request");

        let response = self
            .request(reqwest::Method::DELETE, url)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            // 404 = already removed, treat as success
            return Ok(());
        }
        if !status.is_success() {
            let status_code = status.as_u16();
            let message = response.text().await.unwrap_or_default();
            warn!(
                status = status_code,
                message = message,
                "ClickUp API error response"
            );
            return Err(Error::from_status(status_code, message));
        }
        Ok(())
    }

    /// Make an authenticated DELETE request with query parameters.
    /// Returns `Ok(())` on success. Treats 404 as success (idempotent delete).
    async fn delete_with_query(&self, url: &str, params: &[(&str, &str)]) -> Result<()> {
        debug!(url = url, params = ?params, "ClickUp DELETE request with query");

        let response = self
            .request(reqwest::Method::DELETE, url)
            .query(params)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        if !status.is_success() {
            let status_code = status.as_u16();
            let message = response.text().await.unwrap_or_default();
            warn!(
                status = status_code,
                message = message,
                "ClickUp API error response"
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
                "ClickUp API error response"
            );
            return Err(Error::from_status(status_code, message));
        }

        response
            .json()
            .await
            .map_err(|e| Error::InvalidData(format!("Failed to parse response: {}", e)))
    }

    /// Fetch the configured statuses for `self.list_id`. Shared by
    /// `validate_status_name` (literal-name match, #288) and
    /// `resolve_status` (generic open/closed → type-matched name);
    /// extracted so both stay in sync if the list endpoint shape
    /// evolves.
    async fn fetch_list_statuses(&self) -> Result<ClickUpListInfo> {
        let url = format!("{}/list/{}", self.base_url, self.list_id);
        self.get(&url).await
    }

    /// Maximum number of status names embedded in the "Unknown status"
    /// error message. Some workspaces have 50+ configured statuses and
    /// dumping all of them creates wall-of-text errors that are worse
    /// than just listing the common ones.
    const STATUS_LIST_ERROR_LIMIT: usize = 10;

    /// Validate a literal ClickUp custom status name (#288) against the
    /// list's configured statuses. Returns the canonical name from the
    /// list (preserving the case as configured in ClickUp) when the
    /// input matches case-insensitively. Errors with a clear message
    /// listing valid statuses when there's no match — silent fallthrough
    /// caused the regression on the cloud side where the MCP layer
    /// accepted "in progress" and ClickUp returned 200 without changing
    /// anything.
    ///
    /// If the input looks like a generic state keyword (`open`,
    /// `opened`, `closed`), the error message points the caller at the
    /// `state` field instead. Those keywords are rarely actual ClickUp
    /// status names; the most likely cause is the caller mistakenly
    /// passing them through the wrong field.
    async fn validate_status_name(&self, requested: &str) -> Result<String> {
        let list_info = self.fetch_list_statuses().await?;
        if let Some(found) = list_info
            .statuses
            .iter()
            .find(|s| s.status.eq_ignore_ascii_case(requested))
        {
            return Ok(found.status.clone());
        }
        let total = list_info.statuses.len();
        let valid_preview: Vec<&str> = list_info
            .statuses
            .iter()
            .take(Self::STATUS_LIST_ERROR_LIMIT)
            .map(|s| s.status.as_str())
            .collect();
        let valid_str = if total > Self::STATUS_LIST_ERROR_LIMIT {
            format!(
                "{}, …and {} more",
                valid_preview.join(", "),
                total - Self::STATUS_LIST_ERROR_LIMIT
            )
        } else {
            valid_preview.join(", ")
        };
        let hint = match requested.to_ascii_lowercase().as_str() {
            "open" | "opened" | "closed" => {
                " (note: for generic open/closed transitions, pass via the `state` field instead)"
            }
            _ => "",
        };
        Err(Error::InvalidData(format!(
            "Unknown ClickUp status '{requested}' for list {}. Valid: [{valid_str}]{hint}",
            self.list_id
        )))
    }

    /// Resolve a unified state name ("open"/"closed") to the actual ClickUp status name
    /// by fetching the list's configured statuses.
    /// If the state doesn't match a known type, it's passed as-is (exact status name).
    async fn resolve_status(&self, state: &str) -> Result<String> {
        let status_type = match state {
            "closed" => "closed",
            "open" | "opened" => "open",
            _ => return Ok(state.to_string()),
        };

        let list_info = self.fetch_list_statuses().await?;

        list_info
            .statuses
            .iter()
            .find(|s| s.status_type.as_deref() == Some(status_type))
            .map(|s| s.status.clone())
            .ok_or_else(|| {
                Error::InvalidData(format!(
                    "No status with type '{}' found in list {}",
                    status_type, self.list_id
                ))
            })
    }

    /// Resolve a task key to its raw ClickUp task ID.
    /// For `CU-{id}` keys, strips the prefix.
    /// For custom IDs (e.g., `DEV-42`), returns as-is (ClickUp dependency API accepts custom IDs).
    fn resolve_task_id(&self, key: &str) -> Result<String> {
        if let Some(raw_id) = key.strip_prefix("CU-") {
            Ok(raw_id.to_string())
        } else {
            Ok(key.to_string())
        }
    }

    /// Resolve a task key to its native ClickUp task ID by fetching the task if needed.
    /// For `CU-{id}` keys, strips the prefix (fast path).
    /// For custom IDs (e.g., `DEV-42`), fetches the task to get the native `.id`.
    /// Use this when the native ID is required in URL path segments.
    async fn resolve_to_native_id(&self, key: &str) -> Result<String> {
        if let Some(raw_id) = key.strip_prefix("CU-") {
            Ok(raw_id.to_string())
        } else {
            let url = self.task_url(key)?;
            let task: ClickUpTask = self.get(&url).await?;
            Ok(task.id)
        }
    }

    /// Build the URL for accessing a task by key.
    /// For `CU-{id}` keys, uses the raw task ID directly.
    /// For custom IDs (e.g., `DEV-42`), appends `?custom_task_ids=true&team_id=` params.
    fn task_url(&self, key: &str) -> Result<String> {
        if let Some(raw_id) = key.strip_prefix("CU-") {
            Ok(format!("{}/task/{}", self.base_url, raw_id))
        } else {
            // Custom task ID — requires team_id
            let team_id = self.team_id.as_ref().ok_or_else(|| {
                Error::Config(format!(
                    "team_id is required to resolve custom task ID '{}'. \
                     Run: devboy config set clickup.team_id <team_id>",
                    key
                ))
            })?;
            Ok(format!(
                "{}/task/{}?custom_task_ids=true&team_id={}",
                self.base_url, key, team_id
            ))
        }
    }
}

// =============================================================================
// Mapping functions: ClickUp types -> Unified types
// =============================================================================

fn map_user(cu_user: Option<&ClickUpUser>) -> Option<User> {
    cu_user.map(|u| User {
        id: u.id.to_string(),
        username: u.username.clone(),
        name: Some(u.username.clone()),
        email: u.email.clone(),
        avatar_url: u.profile_picture.clone(),
    })
}

fn map_user_required(cu_user: Option<&ClickUpUser>) -> User {
    map_user(cu_user).unwrap_or_else(|| User {
        id: "unknown".to_string(),
        username: "unknown".to_string(),
        name: Some("Unknown".to_string()),
        ..Default::default()
    })
}

fn map_tags(tags: &[crate::types::ClickUpTag]) -> Vec<String> {
    tags.iter().map(|t| t.name.clone()).collect()
}

fn map_priority(priority: Option<&ClickUpPriority>) -> Option<String> {
    priority.map(|p| match p.id.as_str() {
        "1" => "urgent".to_string(),
        "2" => "high".to_string(),
        "3" => "normal".to_string(),
        "4" => "low".to_string(),
        _ => p.priority.to_lowercase(),
    })
}

fn map_state(task: &ClickUpTask) -> String {
    match task.status.status_type.as_deref() {
        Some("closed") => "closed".to_string(),
        _ => "open".to_string(),
    }
}

/// Map a ClickUp status to a semantic category using both the status type field
/// and name-based heuristics (for custom statuses where the type is always "custom").
///
/// Categories: "backlog", "todo", "in_progress", "done", "cancelled"
fn map_status_category(status_type: Option<&str>, status_name: &str) -> String {
    // First, use the explicit type from ClickUp
    match status_type {
        Some("closed") | Some("done") => return "done".to_string(),
        // "open" type in ClickUp is the initial/default status — map via name heuristics below
        // "custom" type covers most user-defined statuses — also use name heuristics
        _ => {}
    }

    // Name-based heuristic matching (case-insensitive)
    let name_lower = status_name.to_lowercase();

    if name_lower.contains("backlog") {
        "backlog".to_string()
    } else if name_lower.contains("cancel")
        || name_lower.contains("archived")
        || name_lower.contains("rejected")
    {
        "cancelled".to_string()
    } else if name_lower.contains("done")
        || name_lower.contains("complete")
        || name_lower.contains("closed")
        || name_lower.contains("resolved")
    {
        "done".to_string()
    } else if name_lower.contains("progress")
        || name_lower.contains("doing")
        || name_lower.contains("active")
        || name_lower.contains("review")
    {
        "in_progress".to_string()
    } else if name_lower.contains("todo")
        || name_lower.contains("to do")
        || name_lower.contains("open")
        || name_lower.contains("new")
    {
        "todo".to_string()
    } else {
        // Unknown custom status — default based on type
        match status_type {
            Some("open") => "todo".to_string(),
            _ => "in_progress".to_string(),
        }
    }
}

/// Build the unified issue key for a task.
/// Uses `custom_id` when available (e.g., `DEV-42`), otherwise `CU-{id}`.
fn map_task_key(task: &ClickUpTask) -> String {
    if let Some(custom_id) = &task.custom_id {
        custom_id.clone()
    } else {
        format!("CU-{}", task.id)
    }
}

/// Convert ClickUp epoch-millisecond timestamp to ISO 8601 string.
fn epoch_ms_to_iso8601(epoch_ms: &str) -> Option<String> {
    let ms: i64 = epoch_ms.parse().ok()?;
    let secs = ms / 1000;
    let datetime = time_from_unix(secs);
    Some(datetime)
}

/// Convert unix timestamp to ISO 8601 string without external crate.
fn time_from_unix(secs: i64) -> String {
    // Days from unix epoch
    let mut days = secs / 86400;
    let day_secs = secs.rem_euclid(86400);
    if secs % 86400 < 0 {
        days -= 1;
    }

    let hours = day_secs / 3600;
    let minutes = (day_secs % 3600) / 60;
    let seconds = day_secs % 60;

    // Convert days since epoch to year-month-day
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, m, d, hours, minutes, seconds
    )
}

fn map_timestamp(ts: &Option<String>) -> Option<String> {
    ts.as_ref().and_then(|s| epoch_ms_to_iso8601(s))
}

fn map_task(task: &ClickUpTask) -> Issue {
    // Surface set custom fields keyed by ClickUp's stable field id
    // (matches `get_custom_fields` output across providers). Display
    // name rides along inside `CustomFieldValue.name` so consumers
    // don't lose it. Unset fields (`value: None`) are skipped to
    // keep the map noise-free.
    let custom_fields: std::collections::HashMap<String, devboy_core::CustomFieldValue> = task
        .custom_fields
        .iter()
        .filter_map(|cf| {
            cf.value.as_ref().map(|v| {
                (
                    cf.id.clone(),
                    devboy_core::CustomFieldValue {
                        name: cf.name.clone(),
                        value: v.clone(),
                    },
                )
            })
        })
        .collect();
    Issue {
        custom_fields,
        key: map_task_key(task),
        title: task.name.clone(),
        description: task
            .text_content
            .clone()
            .or_else(|| task.description.clone()),
        state: map_state(task),
        source: "clickup".to_string(),
        priority: map_priority(task.priority.as_ref()),
        labels: map_tags(&task.tags),
        author: map_user(task.creator.as_ref()),
        assignees: task
            .assignees
            .iter()
            .map(|u| map_user_required(Some(u)))
            .collect(),
        url: Some(task.url.clone()),
        created_at: map_timestamp(&task.date_created),
        updated_at: map_timestamp(&task.date_updated),
        attachments_count: if task.attachments.is_empty() {
            None
        } else {
            Some(task.attachments.len() as u32)
        },
        parent: task.parent.as_ref().map(|id| format!("CU-{id}")),
        subtasks: task
            .subtasks
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(map_task)
            .collect(),
    }
}

fn map_comment(cu_comment: &ClickUpComment) -> Comment {
    Comment {
        id: cu_comment.id.clone(),
        body: cu_comment.comment_text.clone(),
        author: map_user(cu_comment.user.as_ref()),
        created_at: map_timestamp(&cu_comment.date),
        updated_at: None,
        position: None,
    }
}

/// Map a ClickUp attachment payload to the provider-agnostic [`AssetMeta`].
fn map_clickup_attachment(raw: &ClickUpAttachment) -> AssetMeta {
    let filename = raw
        .title
        .clone()
        .or_else(|| {
            raw.url
                .as_deref()
                .map(devboy_core::asset::filename_from_url)
        })
        .unwrap_or_else(|| format!("attachment-{}", raw.id));

    let size = match raw.size.as_ref() {
        Some(serde_json::Value::Number(n)) => n.as_u64(),
        Some(serde_json::Value::String(s)) => s.parse::<u64>().ok(),
        _ => None,
    };

    let created_at = raw.date.as_deref().and_then(epoch_ms_to_iso8601);

    let author = raw.user.as_ref().map(|u| u.username.clone());

    AssetMeta {
        id: raw.id.clone(),
        filename,
        mime_type: raw.mimetype.clone(),
        size,
        url: raw.url.clone(),
        created_at,
        author,
        cached: false,
        local_path: None,
        checksum_sha256: None,
        analysis: None,
    }
}

/// Sorting key for priority (lower = more urgent).
/// urgent=1, high=2, normal=3, low=4, None=5
fn priority_sort_key(priority: Option<&str>) -> u8 {
    match priority {
        Some("urgent") => 1,
        Some("high") => 2,
        Some("normal") => 3,
        Some("low") => 4,
        _ => 5,
    }
}

/// Map a unified priority string to a ClickUp priority number.
fn priority_to_clickup(priority: &str) -> Option<u8> {
    match priority {
        "urgent" => Some(1),
        "high" => Some(2),
        "normal" => Some(3),
        "low" => Some(4),
        _ => None,
    }
}

/// Map ClickUp dependencies (serde_json::Value) to (blocked_by, blocks) IssueLink vectors.
///
/// ClickUp dependency JSON shape (observed):
/// ```json
/// { "task_id": "abc", "depends_on": "xyz", "type": 1, ... }
/// ```
/// - If `depends_on` == this task's ID → `task_id` depends on this task, so this task **blocks** `task_id`
/// - If `dependency_of` == this task's ID → this task depends on `task_id`, so this task is **blocked by** `task_id`
fn map_dependencies(
    deps: &[serde_json::Value],
    this_task_id: &str,
) -> (Vec<IssueLink>, Vec<IssueLink>) {
    let mut blocked_by = Vec::new();
    let mut blocks = Vec::new();

    for dep in deps {
        let task_id = dep
            .get("task_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let depends_on = dep
            .get("depends_on")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let dependency_of = dep
            .get("dependency_of")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        let other_id = if !task_id.is_empty() {
            task_id
        } else {
            continue;
        };

        let other_issue = Issue {
            key: format!("CU-{other_id}"),
            source: "clickup".to_string(),
            ..Default::default()
        };

        if depends_on == this_task_id {
            // task_id depends on this task → this task blocks task_id
            blocks.push(IssueLink {
                issue: other_issue,
                link_type: "blocks".to_string(),
            });
        } else if dependency_of == this_task_id {
            // task_id is dependency of this task → this task is blocked by task_id
            blocked_by.push(IssueLink {
                issue: other_issue,
                link_type: "blocked_by".to_string(),
            });
        } else {
            // Fallback: try to infer from "type" field
            // ClickUp type 1 = waiting on, type 0 = blocking
            let dep_type = dep.get("type").and_then(|v| v.as_u64());
            match dep_type {
                Some(1) => {
                    blocked_by.push(IssueLink {
                        issue: other_issue,
                        link_type: "blocked_by".to_string(),
                    });
                }
                Some(0) => {
                    blocks.push(IssueLink {
                        issue: other_issue,
                        link_type: "blocks".to_string(),
                    });
                }
                _ => {
                    // Unknown direction, add as blocked_by by default
                    blocked_by.push(IssueLink {
                        issue: other_issue,
                        link_type: "blocked_by".to_string(),
                    });
                }
            }
        }
    }

    (blocked_by, blocks)
}

/// Map ClickUp linked tasks to IssueLinks, preserving dependency semantics when available.
fn map_linked_tasks(links: &[ClickUpLinkedTask]) -> Vec<IssueLink> {
    links
        .iter()
        .map(|link| {
            let link_type = match link.link_type.as_deref() {
                Some("blocked_by") => "blocked_by",
                Some("blocking") => "blocks",
                _ => "relates_to",
            }
            .to_string();

            IssueLink {
                issue: Issue {
                    key: format!("CU-{}", link.task_id),
                    source: "clickup".to_string(),
                    ..Default::default()
                },
                link_type,
            }
        })
        .collect()
}

// =============================================================================
// Trait implementations
// =============================================================================

#[async_trait]
impl IssueProvider for ClickUpClient {
    async fn get_issues(&self, filter: IssueFilter) -> Result<ProviderResult<Issue>> {
        let limit = filter.limit.unwrap_or(20) as usize;
        if limit == 0 {
            return Ok(vec![].into());
        }
        let offset = filter.offset.unwrap_or(0) as usize;

        // Calculate which pages we need to fetch
        let start_page = offset / PAGE_SIZE as usize;
        let end_page = (offset + limit).saturating_sub(1) / PAGE_SIZE as usize;

        // Build base query params (without page).
        // Values are properly URL-encoded by reqwest's .query() method.
        let mut base_params: Vec<(&str, String)> = vec![];

        let include_closed = matches!(filter.state.as_deref(), Some("closed") | Some("all"))
            || matches!(
                filter.state_category.as_deref(),
                Some("done") | Some("cancelled")
            );
        if include_closed {
            base_params.push(("include_closed", "true".to_string()));
        }

        base_params.push(("subtasks", "true".to_string()));

        if let Some(assignee) = &filter.assignee {
            // ClickUp API expects numeric user IDs for assignee filtering,
            // but IssueFilter.assignee is documented as a username.
            // Pass through as-is — it will work if the caller provides a user ID.
            warn!(
                assignee = assignee.as_str(),
                "ClickUp assignee filter expects numeric user IDs, not usernames"
            );
            base_params.push(("assignees[]", assignee.clone()));
        }

        if let Some(tags) = &filter.labels {
            for tag in tags {
                base_params.push(("tags[]", tag.clone()));
            }
        }

        // Track whether client-side sorting is needed for unsupported fields
        let mut client_side_sort: Option<String> = None;

        if let Some(order_by) = &filter.sort_by {
            match order_by.as_str() {
                "created_at" | "created" => {
                    base_params.push(("order_by", "created".to_string()));
                }
                "updated_at" | "updated" => {
                    base_params.push(("order_by", "updated".to_string()));
                }
                other => {
                    // Unsupported by ClickUp API — will sort client-side after fetch
                    client_side_sort = Some(other.to_string());
                    warn!(
                        sort_by = other,
                        "ClickUp API does not support sorting by '{}', applying client-side sort",
                        other
                    );
                }
            }
        }

        let sort_order_is_asc = filter.sort_order.as_deref().is_some_and(|o| o == "asc");

        if sort_order_is_asc && client_side_sort.is_none() {
            base_params.push(("reverse", "true".to_string()));
        }

        // Fetch all needed pages
        let base_url = format!("{}/list/{}/task", self.base_url, self.list_id);
        let mut all_tasks: Vec<ClickUpTask> = Vec::new();

        for page in start_page..=end_page {
            let mut params = base_params.clone();
            params.push(("page", page.to_string()));

            let param_refs: Vec<(&str, &str)> =
                params.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let response: ClickUpTaskList = self.get_with_query(&base_url, &param_refs).await?;
            let page_len = response.tasks.len();
            all_tasks.extend(response.tasks);

            // Stop if this page has fewer than PAGE_SIZE items (no more data)
            if page_len < PAGE_SIZE as usize {
                break;
            }
        }

        // Filter by stateCategory if provided (semantic status filtering).
        // This must happen on raw ClickUp tasks before mapping, since the actual
        // status name (e.g., "Backlog", "In Progress") is lost during map_task().
        if let Some(ref state_category) = filter.state_category {
            let statuses = self.get_statuses().await?;
            let matching_status_names: Vec<String> = statuses
                .items
                .iter()
                .filter(|s| s.category == *state_category)
                .map(|s| s.name.to_lowercase())
                .collect();

            all_tasks.retain(|t| matching_status_names.contains(&t.status.status.to_lowercase()));
        }

        let mut issues: Vec<Issue> = all_tasks.iter().map(map_task).collect();

        // Filter by state client-side if needed
        if let Some(state) = &filter.state {
            match state.as_str() {
                "opened" | "open" => {
                    issues.retain(|i| i.state == "open");
                }
                "closed" => {
                    issues.retain(|i| i.state == "closed");
                }
                _ => {} // "all" — no filter
            }
        }

        // Labels AND operator: ClickUp API uses OR by default. For AND, post-filter.
        if filter.labels_operator.as_deref() == Some("and")
            && let Some(ref required_labels) = filter.labels
        {
            let required: Vec<String> = required_labels.iter().map(|l| l.to_lowercase()).collect();
            issues.retain(|issue| {
                let issue_labels: Vec<String> =
                    issue.labels.iter().map(|l| l.to_lowercase()).collect();
                required.iter().all(|r| issue_labels.contains(r))
            });
        }

        // Client-side search filtering (ClickUp API has no search endpoint for tasks)
        if let Some(ref query) = filter.search {
            let q = query.to_lowercase();
            issues.retain(|issue| {
                issue.title.to_lowercase().contains(&q)
                    || issue
                        .description
                        .as_ref()
                        .is_some_and(|d| d.to_lowercase().contains(&q))
                    || issue.key.to_lowercase().contains(&q)
            });
        }

        // Client-side sorting for fields unsupported by ClickUp API
        if let Some(ref sort_field) = client_side_sort {
            match sort_field.as_str() {
                "priority" => {
                    issues.sort_by(|a, b| {
                        let pa = priority_sort_key(a.priority.as_deref());
                        let pb = priority_sort_key(b.priority.as_deref());
                        if sort_order_is_asc {
                            pa.cmp(&pb)
                        } else {
                            pb.cmp(&pa)
                        }
                    });
                }
                "title" => {
                    issues.sort_by(|a, b| {
                        let cmp = a.title.to_lowercase().cmp(&b.title.to_lowercase());
                        if sort_order_is_asc {
                            cmp
                        } else {
                            cmp.reverse()
                        }
                    });
                }
                _ => {
                    // Unknown sort field — leave as-is (API default order)
                }
            }
        }

        // Apply offset within first page and limit
        let offset_in_first_page = offset % PAGE_SIZE as usize;
        if offset_in_first_page < issues.len() {
            issues = issues.split_off(offset_in_first_page);
        } else {
            issues.clear();
        }

        issues.truncate(limit);

        // Build sort info metadata
        let sort_info = SortInfo {
            sort_by: filter.sort_by.clone(),
            sort_order: if sort_order_is_asc {
                SortOrder::Asc
            } else {
                SortOrder::Desc
            },
            available_sorts: vec![
                "created_at".into(),
                "updated_at".into(),
                "priority".into(),
                "title".into(),
            ],
        };

        Ok(ProviderResult::new(issues).with_sort_info(sort_info))
    }

    async fn get_issue(&self, key: &str) -> Result<Issue> {
        let base_url = self.task_url(key)?;
        let separator = if base_url.contains('?') { "&" } else { "?" };
        let url = format!("{}{}include_subtasks=true", base_url, separator);
        let task: ClickUpTask = self.get(&url).await?;
        Ok(map_task(&task))
    }

    async fn create_issue(&self, input: CreateIssueInput) -> Result<Issue> {
        let url = format!("{}/list/{}/task", self.base_url, self.list_id);

        let priority = input.priority.as_deref().and_then(priority_to_clickup);

        let tags = if input.labels.is_empty() {
            None
        } else {
            Some(input.labels)
        };

        // Resolve parent key to native ClickUp task ID if provided.
        // Fast-path: if the key is already a CU-{id} key, the native ID is known.
        let parent = match input.parent {
            Some(ref parent_key) => {
                if let Some(stripped) = parent_key.strip_prefix("CU-") {
                    Some(stripped.to_string())
                } else {
                    let parent_url = self.task_url(parent_key)?;
                    let parent_task: ClickUpTask = self.get(&parent_url).await?;
                    Some(parent_task.id)
                }
            }
            None => None,
        };

        let (description, markdown_content) = if input.markdown {
            (None, input.description)
        } else {
            (input.description, None)
        };

        let request = CreateTaskRequest {
            name: input.title,
            description,
            markdown_content,
            parent,
            status: None,
            priority,
            tags,
            assignees: None, // ClickUp expects user IDs, not usernames
        };

        let task: ClickUpTask = self.post(&url, &request).await?;
        let task_id = task.id.clone();

        // ClickUp generates custom_id asynchronously after task creation.
        // Retry GET until custom_id is available (matching DevBoy backend pattern).
        if task.custom_id.is_none() {
            for attempt in 1..=3u64 {
                tokio::time::sleep(std::time::Duration::from_millis(300 * attempt)).await;
                let fetch_url = format!("{}/task/{}", self.base_url, task_id);
                if let Ok(fetched) = self.get::<ClickUpTask>(&fetch_url).await
                    && fetched.custom_id.is_some()
                {
                    debug!(
                        task_id = task_id,
                        custom_id = ?fetched.custom_id,
                        attempt = attempt,
                        "Got custom_id after retry"
                    );
                    return Ok(map_task(&fetched));
                }
            }
            warn!(
                task_id = task_id,
                "custom_id not available after 3 retries, using POST response"
            );
        }

        Ok(map_task(&task))
    }

    async fn update_issue(&self, key: &str, input: UpdateIssueInput) -> Result<Issue> {
        let url = self.task_url(key)?;

        // `status` takes precedence over `state` (#288). It is a literal
        // ClickUp custom status name (e.g. "in progress", "review");
        // we validate against the list's configured statuses
        // (case-insensitive) and forward the canonical form. `state`
        // is the legacy generic open/closed path resolved by type.
        let status = if let Some(s) = input.status.as_deref() {
            Some(self.validate_status_name(s).await?)
        } else if let Some(s) = input.state.as_deref() {
            Some(self.resolve_status(s).await?)
        } else {
            None
        };

        let priority = input.priority.as_deref().and_then(priority_to_clickup);

        let (description, markdown_content) = if input.markdown {
            (None, input.description)
        } else {
            (input.description, None)
        };

        // Resolve parent key to native ClickUp task ID if provided.
        // "none" or "" → detach from parent (convert subtask → standalone task).
        // ClickUp API accepts {"parent": "none"} to remove parent (undocumented but verified).
        let parent = match input.parent_id {
            Some(ref parent_key) if parent_key == "none" || parent_key.is_empty() => {
                Some("none".to_string())
            }
            Some(ref parent_key) => {
                if let Some(stripped) = parent_key.strip_prefix("CU-") {
                    Some(stripped.to_string())
                } else {
                    let parent_url = self.task_url(parent_key)?;
                    let parent_task: ClickUpTask = self.get(&parent_url).await?;
                    Some(parent_task.id)
                }
            }
            None => None,
        };

        let request = UpdateTaskRequest {
            name: input.title,
            description,
            markdown_content,
            status,
            priority,
            parent,
            tags: None, // Tags updated via separate API below
        };

        let task: ClickUpTask = self.put(&url, &request).await?;

        // Update tags via ClickUp Tag API (PUT /task ignores tags field).
        // POST /task/{id}/tag/{name} to add, DELETE /task/{id}/tag/{name} to remove.
        if let Some(ref new_labels) = input.labels {
            let current_tags: Vec<String> = task.tags.iter().map(|t| t.name.clone()).collect();
            let new_tags: Vec<String> = new_labels.iter().map(|l| l.to_lowercase()).collect();

            // Remove tags not in new list
            for tag in &current_tags {
                if !new_tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
                    let tag_url =
                        format!("{}/task/{}/tag/{}", self.base_url, task.id, encode_tag(tag));
                    if let Err(e) = self.delete(&tag_url).await {
                        warn!(tag = tag, error = %e, "Failed to remove tag");
                    }
                }
            }

            // Add tags not in current list
            for tag in &new_tags {
                if !current_tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
                    let tag_url =
                        format!("{}/task/{}/tag/{}", self.base_url, task.id, encode_tag(tag));
                    let resp = self
                        .request(reqwest::Method::POST, &tag_url)
                        .send()
                        .await
                        .map_err(|e| Error::Http(e.to_string()))?;
                    if !resp.status().is_success() {
                        warn!(
                            tag = tag,
                            status = resp.status().as_u16(),
                            "Failed to add tag"
                        );
                    }
                }
            }
        }

        // Re-fetch after PUT because ClickUp can return stale status in the PUT response (#117),
        // but do not fail the whole update if the refresh itself is transiently unavailable.
        match self.get::<ClickUpTask>(&url).await {
            Ok(updated_task) => Ok(map_task(&updated_task)),
            Err(e) => {
                warn!(
                    issue_key = key,
                    error = %e,
                    "Task updated successfully, but failed to re-fetch fresh state; falling back to PUT response"
                );
                Ok(map_task(&task))
            }
        }
    }

    async fn set_custom_fields(&self, issue_key: &str, fields: &[serde_json::Value]) -> Result<()> {
        let task_id = self.resolve_to_native_id(issue_key).await?;
        for field in fields {
            let field_id = field["id"].as_str().unwrap_or_default();
            if field_id.is_empty() {
                continue;
            }
            let url = format!("{}/task/{}/field/{}", self.base_url, task_id, field_id);
            let body = serde_json::json!({ "value": field["value"] });
            let resp = self
                .request(reqwest::Method::POST, &url)
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::Http(e.to_string()))?;
            if !resp.status().is_success() {
                let status = resp.status().as_u16();
                let msg = resp.text().await.unwrap_or_default();
                warn!(
                    field_id = field_id,
                    status = status,
                    "Failed to set custom field: {}",
                    msg
                );
            }
        }
        Ok(())
    }

    async fn get_comments(&self, issue_key: &str) -> Result<ProviderResult<Comment>> {
        let base_url = self.task_url(issue_key)?;
        // Append /comment — handle both raw URL and URL with query params
        let url = if base_url.contains('?') {
            let (path, query) = base_url.split_once('?').unwrap();
            format!("{}/comment?{}", path, query)
        } else {
            format!("{}/comment", base_url)
        };
        let response: ClickUpCommentList = self.get(&url).await?;
        Ok(response
            .comments
            .iter()
            .map(map_comment)
            .collect::<Vec<_>>()
            .into())
    }

    async fn add_comment(&self, issue_key: &str, body: &str) -> Result<Comment> {
        let base_url = self.task_url(issue_key)?;
        let url = if base_url.contains('?') {
            let (path, query) = base_url.split_once('?').unwrap();
            format!("{}/comment?{}", path, query)
        } else {
            format!("{}/comment", base_url)
        };
        let request = CreateCommentRequest {
            comment_text: body.to_string(),
        };

        // ClickUp POST returns minimal response (id + date), not full comment
        let response: CreateCommentResponse = self.post(&url, &request).await?;
        Ok(Comment {
            id: response.id,
            body: body.to_string(),
            author: None,
            created_at: map_timestamp(&response.date),
            updated_at: None,
            position: None,
        })
    }

    async fn upload_attachment(
        &self,
        issue_key: &str,
        filename: &str,
        data: &[u8],
    ) -> Result<String> {
        let task_id = self.resolve_to_native_id(issue_key).await?;
        let url = format!("{}/task/{}/attachment", self.base_url, task_id);

        let part = reqwest::multipart::Part::bytes(data.to_vec())
            .file_name(filename.to_string())
            .mime_str("application/octet-stream")
            .map_err(|e| Error::Http(format!("Failed to create multipart: {}", e)))?;

        let form = reqwest::multipart::Form::new().part("attachment", part);

        let response = self
            .client
            .post(&url)
            .header("Authorization", self.token.expose_secret())
            .multipart(form)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(Error::from_status(status.as_u16(), message));
        }

        // ClickUp returns: { "attachment": { "url": "..." } } or similar
        let body: serde_json::Value = response.json().await.map_err(|e| {
            Error::InvalidData(format!("Failed to parse attachment response: {}", e))
        })?;

        // Extract URL from response
        let download_url = body
            .pointer("/url")
            .or_else(|| body.pointer("/attachment/url"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        Ok(download_url)
    }

    async fn get_issue_attachments(&self, issue_key: &str) -> Result<Vec<AssetMeta>> {
        let url = self.task_url(issue_key)?;
        let task: ClickUpTask = self.get(&url).await?;
        Ok(task
            .attachments
            .iter()
            .map(map_clickup_attachment)
            .collect())
    }

    async fn download_attachment(&self, issue_key: &str, asset_id: &str) -> Result<Vec<u8>> {
        // ClickUp does not expose an "attachment by id" endpoint: the
        // download URL lives on the task payload. Fetch the task, look up
        // the attachment, and download from its URL using our authenticated
        // client.
        let url = self.task_url(issue_key)?;
        let task: ClickUpTask = self.get(&url).await?;
        let attachment = task
            .attachments
            .iter()
            .find(|a| a.id == asset_id)
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "attachment '{asset_id}' not found on task {issue_key}",
                ))
            })?;
        let download_url = attachment.url.as_deref().ok_or_else(|| {
            Error::InvalidData(format!(
                "attachment '{asset_id}' on task {issue_key} has no URL",
            ))
        })?;

        let response = self
            .client
            .get(download_url)
            .header("Authorization", self.token.expose_secret())
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

    fn asset_capabilities(&self) -> AssetCapabilities {
        // ClickUp supports upload / download / list on issue (task) bodies.
        // There is no public delete attachment endpoint, so `delete` stays
        // false for every context.
        AssetCapabilities {
            issue: ContextCapabilities {
                upload: true,
                download: true,
                delete: false,
                list: true,
                max_file_size: None,
                allowed_types: Vec::new(),
            },
            ..Default::default()
        }
    }

    async fn get_statuses(&self) -> Result<ProviderResult<IssueStatus>> {
        let url = format!("{}/list/{}", self.base_url, self.list_id);
        let list_info: ClickUpListInfo = self.get(&url).await?;

        let statuses: Vec<IssueStatus> = list_info
            .statuses
            .iter()
            .enumerate()
            .map(|(idx, s)| {
                let category = map_status_category(s.status_type.as_deref(), &s.status);
                IssueStatus {
                    id: s.status.clone(),
                    name: s.status.clone(),
                    category,
                    color: s.color.clone(),
                    order: s.orderindex.or(Some(idx as u32)),
                }
            })
            .collect();

        Ok(statuses.into())
    }

    async fn link_issues(&self, source_key: &str, target_key: &str, link_type: &str) -> Result<()> {
        match link_type {
            "subtask" => {
                // source becomes subtask of target → set parent on source task
                let source_url = self.task_url(source_key)?;
                let target_native_id = self.resolve_to_native_id(target_key).await?;
                let body = serde_json::json!({ "parent": target_native_id });
                let _: ClickUpTask = self.put(&source_url, &body).await?;
            }
            "blocks" => {
                // source blocks target → target depends_on source
                let source_id = self.resolve_task_id(source_key)?;
                let target_id = self.resolve_task_id(target_key)?;
                let url = format!("{}/task/{}/dependency", self.base_url, target_id);
                let body = serde_json::json!({ "depends_on": source_id });
                let _: serde_json::Value = self.post(&url, &body).await?;
            }
            "blocked_by" => {
                // source is blocked by target → source depends_on target
                let source_id = self.resolve_task_id(source_key)?;
                let target_id = self.resolve_task_id(target_key)?;
                let url = format!("{}/task/{}/dependency", self.base_url, source_id);
                let body = serde_json::json!({ "depends_on": target_id });
                let _: serde_json::Value = self.post(&url, &body).await?;
            }
            _ => {
                // Link tasks (bidirectional, non-dependency relationship)
                let source_id = self.resolve_to_native_id(source_key).await?;
                let target_id = self.resolve_to_native_id(target_key).await?;
                let url = format!("{}/task/{}/link/{}", self.base_url, source_id, target_id);
                let body = serde_json::json!({});
                let _: serde_json::Value = self.post(&url, &body).await?;
            }
        }

        Ok(())
    }

    async fn unlink_issues(
        &self,
        source_key: &str,
        target_key: &str,
        link_type: &str,
    ) -> Result<()> {
        match link_type {
            "subtask" => {
                // Detach source from parent (convert subtask → standalone task).
                // Use native task ID + "parent": "none" (undocumented but verified working).
                let source_id = self.resolve_to_native_id(source_key).await?;
                let url = format!("{}/task/{}", self.base_url, source_id);
                let body = serde_json::json!({ "parent": "none" });
                let _: ClickUpTask = self.put(&url, &body).await?;
            }
            "blocks" => {
                // Remove: source blocks target → target depends_on source
                let source_id = self.resolve_to_native_id(source_key).await?;
                let target_id = self.resolve_to_native_id(target_key).await?;
                let url = format!("{}/task/{}/dependency", self.base_url, target_id);
                self.delete_with_query(&url, &[("depends_on", &source_id)])
                    .await?;
            }
            "blocked_by" => {
                // Remove: source is blocked by target → source depends_on target
                let source_id = self.resolve_to_native_id(source_key).await?;
                let target_id = self.resolve_to_native_id(target_key).await?;
                let url = format!("{}/task/{}/dependency", self.base_url, source_id);
                self.delete_with_query(&url, &[("depends_on", &target_id)])
                    .await?;
            }
            _ => {
                // Remove bidirectional link
                let source_id = self.resolve_to_native_id(source_key).await?;
                let target_id = self.resolve_to_native_id(target_key).await?;
                let url = format!("{}/task/{}/link/{}", self.base_url, source_id, target_id);
                self.delete(&url).await?;
            }
        }

        Ok(())
    }

    async fn get_issue_relations(&self, issue_key: &str) -> Result<IssueRelations> {
        let url = self.task_url(issue_key)?;
        let task: ClickUpTask = self
            .get_with_query(
                &url,
                &[("include_subtasks", "true"), ("include_closed", "true")],
            )
            .await?;

        let mut relations = IssueRelations::default();

        // Parent: fetch parent task for full Issue data
        if let Some(ref parent_id) = task.parent {
            let parent_url = format!("{}/task/{}", self.base_url, parent_id);
            match self.get::<ClickUpTask>(&parent_url).await {
                Ok(parent_task) => {
                    relations.parent = Some(map_task(&parent_task));
                }
                Err(e) => {
                    tracing::warn!("Failed to fetch parent task {}: {}", parent_id, e);
                    // Still include a minimal parent reference
                    relations.parent = Some(Issue {
                        key: format!("CU-{parent_id}"),
                        source: "clickup".to_string(),
                        ..Default::default()
                    });
                }
            }
        }

        // Subtasks
        if let Some(ref subtasks) = task.subtasks {
            relations.subtasks = subtasks.iter().map(map_task).collect();
        }

        // Dependencies → blocked_by / blocks
        if let Some(ref deps) = task.dependencies {
            let (blocked_by, blocks) = map_dependencies(deps, &task.id);
            relations.blocked_by = blocked_by;
            relations.blocks = blocks;
        }

        // Linked tasks → related_to
        if let Some(ref linked) = task.linked_tasks {
            relations.related_to = map_linked_tasks(linked);
        }

        Ok(relations)
    }

    fn provider_name(&self) -> &'static str {
        "clickup"
    }
}

#[async_trait]
impl MergeRequestProvider for ClickUpClient {
    fn provider_name(&self) -> &'static str {
        "clickup"
    }
}

#[async_trait]
impl PipelineProvider for ClickUpClient {
    fn provider_name(&self) -> &'static str {
        "clickup"
    }
}

#[async_trait]
impl Provider for ClickUpClient {
    async fn get_current_user(&self) -> Result<User> {
        // ClickUp v2 API does not have a /user/me endpoint.
        // Verify the token by fetching the first page of tasks with a minimal request.
        let url = format!(
            "{}/list/{}/task?page=0&subtasks=false",
            self.base_url, self.list_id
        );
        let _: ClickUpTaskList = self.get(&url).await?;

        // Token is valid — return a synthetic user
        Ok(User {
            id: "clickup".to_string(),
            username: "clickup-user".to_string(),
            name: Some("ClickUp User".to_string()),
            ..Default::default()
        })
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ClickUpStatus, ClickUpTag};
    use devboy_core::{CreateCommentInput, MrFilter};

    fn token(s: &str) -> SecretString {
        SecretString::from(s.to_string())
    }

    #[test]
    fn test_epoch_ms_to_iso8601() {
        // 2024-01-01T00:00:00Z = 1704067200000 ms
        assert_eq!(
            epoch_ms_to_iso8601("1704067200000"),
            Some("2024-01-01T00:00:00Z".to_string())
        );

        // 2024-01-02T00:00:00Z = 1704153600000 ms
        assert_eq!(
            epoch_ms_to_iso8601("1704153600000"),
            Some("2024-01-02T00:00:00Z".to_string())
        );

        // 2024-01-15T10:00:00Z = 1705312800000 ms
        assert_eq!(
            epoch_ms_to_iso8601("1705312800000"),
            Some("2024-01-15T10:00:00Z".to_string())
        );

        // Invalid input
        assert_eq!(epoch_ms_to_iso8601("not_a_number"), None);
    }

    #[test]
    fn test_task_url_cu_prefix() {
        let client =
            ClickUpClient::with_base_url("https://api.clickup.com/api/v2", "12345", token("token"));
        let url = client.task_url("CU-abc123").unwrap();
        assert_eq!(url, "https://api.clickup.com/api/v2/task/abc123");
    }

    #[test]
    fn test_task_url_custom_id_with_team() {
        let client =
            ClickUpClient::with_base_url("https://api.clickup.com/api/v2", "12345", token("token"))
                .with_team_id("9876");
        let url = client.task_url("DEV-42").unwrap();
        assert_eq!(
            url,
            "https://api.clickup.com/api/v2/task/DEV-42?custom_task_ids=true&team_id=9876"
        );
    }

    #[test]
    fn test_task_url_custom_id_without_team() {
        let client =
            ClickUpClient::with_base_url("https://api.clickup.com/api/v2", "12345", token("token"));
        let result = client.task_url("DEV-42");
        assert!(result.is_err());
    }

    /// `task.custom_fields` populates `issue.custom_fields` keyed
    /// by ClickUp's stable field **id** (matches the homogeneous
    /// shape `JOIN`able with `get_custom_fields` across providers).
    /// Display name rides along inside `CustomFieldValue.name`.
    /// Unset fields (`value == None`) are filtered.
    #[test]
    fn test_map_task_surfaces_custom_field_values() {
        let task = ClickUpTask {
            id: "abc123".to_string(),
            custom_id: None,
            name: "T".to_string(),
            description: None,
            text_content: None,
            status: ClickUpStatus {
                status: "open".to_string(),
                status_type: Some("open".to_string()),
            },
            priority: None,
            tags: vec![],
            assignees: vec![],
            creator: None,
            url: "https://app.clickup.com/t/abc123".to_string(),
            date_created: None,
            date_updated: None,
            parent: None,
            subtasks: None,
            dependencies: None,
            linked_tasks: None,
            attachments: Vec::new(),
            custom_fields: vec![
                crate::types::ClickUpCustomField {
                    id: "cf-1".to_string(),
                    name: Some("Severity".to_string()),
                    field_type: Some("drop_down".to_string()),
                    value: Some(serde_json::json!("High")),
                },
                crate::types::ClickUpCustomField {
                    id: "cf-2".to_string(),
                    name: Some("Sprint".to_string()),
                    field_type: Some("text".to_string()),
                    value: None, // unset → must be skipped
                },
                crate::types::ClickUpCustomField {
                    id: "cf-3".to_string(),
                    name: None, // anonymous → falls back to id
                    field_type: Some("number".to_string()),
                    value: Some(serde_json::json!(42)),
                },
            ],
        };

        let issue = map_task(&task);
        // Keyed by id; display name rides along in
        // `CustomFieldValue.name`.
        let severity = issue.custom_fields.get("cf-1").expect("cf-1 present");
        assert_eq!(severity.name.as_deref(), Some("Severity"));
        assert_eq!(severity.value, serde_json::json!("High"));
        // Unset value (`cf-2`) is filtered out entirely.
        assert!(!issue.custom_fields.contains_key("cf-2"));
        // Anonymous field (no name) keeps `name: None`.
        let anon = issue.custom_fields.get("cf-3").expect("cf-3 present");
        assert!(anon.name.is_none());
        assert_eq!(anon.value, serde_json::json!(42));
    }

    #[test]
    fn test_map_task() {
        let task = ClickUpTask {
            id: "abc123".to_string(),
            custom_id: None,
            name: "Fix bug".to_string(),
            description: Some("Bug description".to_string()),
            text_content: Some("Bug text content".to_string()),
            status: ClickUpStatus {
                status: "open".to_string(),
                status_type: Some("open".to_string()),
            },
            priority: Some(ClickUpPriority {
                id: "2".to_string(),
                priority: "high".to_string(),
                color: None,
            }),
            tags: vec![ClickUpTag {
                name: "bug".to_string(),
            }],
            assignees: vec![ClickUpUser {
                id: 1,
                username: "dev1".to_string(),
                email: Some("dev1@example.com".to_string()),
                profile_picture: None,
            }],
            creator: Some(ClickUpUser {
                id: 2,
                username: "creator".to_string(),
                email: None,
                profile_picture: None,
            }),
            url: "https://app.clickup.com/t/abc123".to_string(),
            date_created: Some("1704067200000".to_string()),
            date_updated: Some("1704153600000".to_string()),
            parent: None,
            subtasks: None,
            dependencies: None,
            linked_tasks: None,
            attachments: Vec::new(),
            custom_fields: Vec::new(),
        };

        let issue = map_task(&task);
        assert_eq!(issue.key, "CU-abc123");
        assert_eq!(issue.title, "Fix bug");
        assert_eq!(issue.description, Some("Bug text content".to_string()));
        assert_eq!(issue.state, "open");
        assert_eq!(issue.source, "clickup");
        assert_eq!(issue.priority, Some("high".to_string()));
        assert_eq!(issue.labels, vec!["bug"]);
        assert_eq!(issue.assignees.len(), 1);
        assert_eq!(issue.assignees[0].username, "dev1");
        assert!(issue.author.is_some());
        assert_eq!(issue.author.unwrap().username, "creator");
        assert_eq!(
            issue.url,
            Some("https://app.clickup.com/t/abc123".to_string())
        );
        // Timestamps are now ISO 8601
        assert_eq!(issue.created_at, Some("2024-01-01T00:00:00Z".to_string()));
        assert_eq!(issue.updated_at, Some("2024-01-02T00:00:00Z".to_string()));
    }

    #[test]
    fn test_map_task_with_custom_id() {
        let task = ClickUpTask {
            id: "abc123".to_string(),
            custom_id: Some("DEV-42".to_string()),
            name: "Task with custom ID".to_string(),
            description: None,
            text_content: None,
            status: ClickUpStatus {
                status: "open".to_string(),
                status_type: Some("open".to_string()),
            },
            priority: None,
            tags: vec![],
            assignees: vec![],
            creator: None,
            url: "https://app.clickup.com/t/abc123".to_string(),
            date_created: None,
            date_updated: None,
            parent: None,
            subtasks: None,
            dependencies: None,
            linked_tasks: None,
            attachments: Vec::new(),
            custom_fields: Vec::new(),
        };

        let issue = map_task(&task);
        assert_eq!(issue.key, "DEV-42");
    }

    #[test]
    fn test_map_task_closed_status() {
        let task = ClickUpTask {
            id: "abc123".to_string(),
            custom_id: None,
            name: "Closed task".to_string(),
            description: None,
            text_content: None,
            status: ClickUpStatus {
                status: "done".to_string(),
                status_type: Some("closed".to_string()),
            },
            priority: None,
            tags: vec![],
            assignees: vec![],
            creator: None,
            url: "https://app.clickup.com/t/abc123".to_string(),
            date_created: None,
            date_updated: None,
            parent: None,
            subtasks: None,
            dependencies: None,
            linked_tasks: None,
            attachments: Vec::new(),
            custom_fields: Vec::new(),
        };

        let issue = map_task(&task);
        assert_eq!(issue.state, "closed");
    }

    #[test]
    fn test_map_priority_all_levels() {
        let make_priority = |id: &str, name: &str| ClickUpPriority {
            id: id.to_string(),
            priority: name.to_string(),
            color: None,
        };

        assert_eq!(
            map_priority(Some(&make_priority("1", "urgent"))),
            Some("urgent".to_string())
        );
        assert_eq!(
            map_priority(Some(&make_priority("2", "high"))),
            Some("high".to_string())
        );
        assert_eq!(
            map_priority(Some(&make_priority("3", "normal"))),
            Some("normal".to_string())
        );
        assert_eq!(
            map_priority(Some(&make_priority("4", "low"))),
            Some("low".to_string())
        );
        assert_eq!(map_priority(None), None);
    }

    #[test]
    fn test_map_user() {
        let cu_user = ClickUpUser {
            id: 123,
            username: "testuser".to_string(),
            email: Some("test@example.com".to_string()),
            profile_picture: Some("https://example.com/avatar.png".to_string()),
        };

        let user = map_user(Some(&cu_user)).unwrap();
        assert_eq!(user.id, "123");
        assert_eq!(user.username, "testuser");
        assert_eq!(user.name, Some("testuser".to_string()));
        assert_eq!(user.email, Some("test@example.com".to_string()));
        assert_eq!(
            user.avatar_url,
            Some("https://example.com/avatar.png".to_string())
        );
    }

    #[test]
    fn test_map_user_none() {
        assert!(map_user(None).is_none());
    }

    #[test]
    fn test_map_user_required_with_user() {
        let cu_user = ClickUpUser {
            id: 1,
            username: "user1".to_string(),
            email: None,
            profile_picture: None,
        };
        let user = map_user_required(Some(&cu_user));
        assert_eq!(user.username, "user1");
    }

    #[test]
    fn test_map_user_required_without_user() {
        let user = map_user_required(None);
        assert_eq!(user.id, "unknown");
        assert_eq!(user.username, "unknown");
    }

    #[test]
    fn test_map_clickup_attachment_all_fields() {
        let raw = ClickUpAttachment {
            id: "att-1".into(),
            title: Some("report.log".into()),
            url: Some("https://attachments.clickup.com/abc/report.log".into()),
            size: Some(serde_json::json!("2048")),
            extension: Some("log".into()),
            mimetype: Some("text/plain".into()),
            date: Some("1704067200000".into()),
            user: Some(ClickUpUser {
                id: 7,
                username: "uploader".into(),
                email: None,
                profile_picture: None,
            }),
        };
        let meta = map_clickup_attachment(&raw);
        assert_eq!(meta.id, "att-1");
        assert_eq!(meta.filename, "report.log");
        assert_eq!(meta.mime_type.as_deref(), Some("text/plain"));
        assert_eq!(meta.size, Some(2048));
        assert_eq!(
            meta.url.as_deref(),
            Some("https://attachments.clickup.com/abc/report.log")
        );
        assert_eq!(meta.author.as_deref(), Some("uploader"));
        assert_eq!(meta.created_at, Some("2024-01-01T00:00:00Z".to_string()));
        assert!(!meta.cached);
    }

    #[test]
    fn test_map_clickup_attachment_minimal_falls_back_to_url() {
        let raw = ClickUpAttachment {
            id: "att-2".into(),
            title: None,
            url: Some("https://cdn/a/b/screen.png?token=x".into()),
            size: Some(serde_json::json!(4096)),
            extension: None,
            mimetype: None,
            date: None,
            user: None,
        };
        let meta = map_clickup_attachment(&raw);
        // Filename falls back to the last path segment, query stripped.
        assert_eq!(meta.filename, "screen.png");
        assert_eq!(meta.size, Some(4096));
        assert!(meta.created_at.is_none());
        assert!(meta.author.is_none());
    }

    #[test]
    fn test_map_clickup_attachment_missing_everything() {
        let raw = ClickUpAttachment {
            id: "att-3".into(),
            title: None,
            url: None,
            size: None,
            extension: None,
            mimetype: None,
            date: None,
            user: None,
        };
        let meta = map_clickup_attachment(&raw);
        // When title and URL are missing the fallback uses the attachment id.
        assert_eq!(meta.filename, "attachment-att-3");
        assert!(meta.url.is_none());
        assert!(meta.size.is_none());
    }

    #[test]
    fn test_clickup_asset_capabilities() {
        let client =
            ClickUpClient::with_base_url("https://api.clickup.com/api/v2", "12345", token("token"));
        let caps = client.asset_capabilities();
        assert!(caps.issue.upload);
        assert!(caps.issue.download);
        assert!(caps.issue.list);
        assert!(!caps.issue.delete, "ClickUp has no delete attachment API");
        assert!(
            !caps.merge_request.upload,
            "ClickUp does not track merge requests",
        );
    }

    #[test]
    fn test_map_comment() {
        let cu_comment = ClickUpComment {
            id: "42".to_string(),
            comment_text: "Nice work!".to_string(),
            user: Some(ClickUpUser {
                id: 1,
                username: "reviewer".to_string(),
                email: None,
                profile_picture: None,
            }),
            date: Some("1705312800000".to_string()),
        };

        let comment = map_comment(&cu_comment);
        assert_eq!(comment.id, "42");
        assert_eq!(comment.body, "Nice work!");
        assert!(comment.author.is_some());
        assert_eq!(comment.author.unwrap().username, "reviewer");
        // Timestamp is now ISO 8601
        assert_eq!(comment.created_at, Some("2024-01-15T10:00:00Z".to_string()));
        assert!(comment.position.is_none());
    }

    #[test]
    fn test_map_tags() {
        let tags = vec![
            ClickUpTag {
                name: "bug".to_string(),
            },
            ClickUpTag {
                name: "feature".to_string(),
            },
        ];
        let result = map_tags(&tags);
        assert_eq!(result, vec!["bug", "feature"]);
    }

    #[test]
    fn test_map_tags_empty() {
        let result = map_tags(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_priority_to_clickup() {
        assert_eq!(priority_to_clickup("urgent"), Some(1));
        assert_eq!(priority_to_clickup("high"), Some(2));
        assert_eq!(priority_to_clickup("normal"), Some(3));
        assert_eq!(priority_to_clickup("low"), Some(4));
        assert_eq!(priority_to_clickup("unknown"), None);
    }

    #[test]
    fn test_api_url() {
        let client =
            ClickUpClient::with_base_url("https://api.clickup.com/api/v2", "12345", token("token"));
        assert_eq!(client.base_url, "https://api.clickup.com/api/v2");
        assert_eq!(client.list_id, "12345");
    }

    #[test]
    fn test_api_url_strips_trailing_slash() {
        let client = ClickUpClient::with_base_url(
            "https://api.clickup.com/api/v2/",
            "12345",
            token("token"),
        );
        assert_eq!(client.base_url, "https://api.clickup.com/api/v2");
    }

    #[test]
    fn test_with_team_id() {
        let client = ClickUpClient::new("12345", token("token")).with_team_id("9876");
        assert_eq!(client.team_id, Some("9876".to_string()));
    }

    #[test]
    fn test_provider_name() {
        let client = ClickUpClient::new("12345", token("token"));
        assert_eq!(IssueProvider::provider_name(&client), "clickup");
        assert_eq!(MergeRequestProvider::provider_name(&client), "clickup");
    }

    #[test]
    fn test_map_task_description_fallback() {
        let task = ClickUpTask {
            id: "abc".to_string(),
            custom_id: None,
            name: "Task".to_string(),
            description: Some("HTML description".to_string()),
            text_content: None,
            status: ClickUpStatus {
                status: "open".to_string(),
                status_type: Some("open".to_string()),
            },
            priority: None,
            tags: vec![],
            assignees: vec![],
            creator: None,
            url: "https://app.clickup.com/t/abc".to_string(),
            date_created: None,
            date_updated: None,
            parent: None,
            subtasks: None,
            dependencies: None,
            linked_tasks: None,
            attachments: Vec::new(),
            custom_fields: Vec::new(),
        };

        let issue = map_task(&task);
        assert_eq!(issue.description, Some("HTML description".to_string()));
    }

    #[test]
    fn test_map_state_custom_type() {
        let task = ClickUpTask {
            id: "abc".to_string(),
            custom_id: None,
            name: "Task".to_string(),
            description: None,
            text_content: None,
            status: ClickUpStatus {
                status: "in progress".to_string(),
                status_type: Some("custom".to_string()),
            },
            priority: None,
            tags: vec![],
            assignees: vec![],
            creator: None,
            url: "https://app.clickup.com/t/abc".to_string(),
            date_created: None,
            date_updated: None,
            parent: None,
            subtasks: None,
            dependencies: None,
            linked_tasks: None,
            attachments: Vec::new(),
            custom_fields: Vec::new(),
        };

        let issue = map_task(&task);
        assert_eq!(issue.state, "open");
    }

    #[test]
    fn test_map_task_with_parent() {
        let task = ClickUpTask {
            id: "child1".to_string(),
            custom_id: Some("DEV-100".to_string()),
            name: "Child task".to_string(),
            description: None,
            text_content: None,
            status: ClickUpStatus {
                status: "open".to_string(),
                status_type: Some("open".to_string()),
            },
            priority: None,
            tags: vec![],
            assignees: vec![],
            creator: None,
            url: "https://app.clickup.com/t/child1".to_string(),
            date_created: None,
            date_updated: None,
            parent: Some("parent123".to_string()),
            subtasks: None,
            dependencies: None,
            linked_tasks: None,
            attachments: Vec::new(),
            custom_fields: Vec::new(),
        };

        let issue = map_task(&task);
        assert_eq!(issue.parent, Some("CU-parent123".to_string()));
        assert!(issue.subtasks.is_empty());
    }

    #[test]
    fn test_map_task_with_subtasks() {
        let subtask = ClickUpTask {
            id: "sub1".to_string(),
            custom_id: Some("DEV-201".to_string()),
            name: "Subtask 1".to_string(),
            description: None,
            text_content: None,
            status: ClickUpStatus {
                status: "in progress".to_string(),
                status_type: Some("custom".to_string()),
            },
            priority: None,
            tags: vec![],
            assignees: vec![],
            creator: None,
            url: "https://app.clickup.com/t/sub1".to_string(),
            date_created: None,
            date_updated: None,
            parent: Some("epic1".to_string()),
            subtasks: None,
            dependencies: None,
            linked_tasks: None,
            attachments: Vec::new(),
            custom_fields: Vec::new(),
        };

        let task = ClickUpTask {
            id: "epic1".to_string(),
            custom_id: Some("DEV-200".to_string()),
            name: "Epic task".to_string(),
            description: None,
            text_content: None,
            status: ClickUpStatus {
                status: "open".to_string(),
                status_type: Some("open".to_string()),
            },
            priority: None,
            tags: vec![ClickUpTag {
                name: "epic".to_string(),
            }],
            assignees: vec![],
            creator: None,
            url: "https://app.clickup.com/t/epic1".to_string(),
            date_created: None,
            date_updated: None,
            parent: None,
            subtasks: Some(vec![subtask]),
            dependencies: None,
            linked_tasks: None,
            attachments: Vec::new(),
            custom_fields: Vec::new(),
        };

        let issue = map_task(&task);
        assert_eq!(issue.key, "DEV-200");
        assert!(issue.parent.is_none());
        assert_eq!(issue.subtasks.len(), 1);
        assert_eq!(issue.subtasks[0].key, "DEV-201");
        assert_eq!(issue.subtasks[0].title, "Subtask 1");
        assert_eq!(issue.subtasks[0].parent, Some("CU-epic1".to_string()));
    }

    #[test]
    fn test_map_task_no_parent_no_subtasks() {
        let task = ClickUpTask {
            id: "standalone".to_string(),
            custom_id: None,
            name: "Standalone task".to_string(),
            description: None,
            text_content: None,
            status: ClickUpStatus {
                status: "open".to_string(),
                status_type: Some("open".to_string()),
            },
            priority: None,
            tags: vec![],
            assignees: vec![],
            creator: None,
            url: "https://app.clickup.com/t/standalone".to_string(),
            date_created: None,
            date_updated: None,
            parent: None,
            subtasks: None,
            dependencies: None,
            linked_tasks: None,
            attachments: Vec::new(),
            custom_fields: Vec::new(),
        };

        let issue = map_task(&task);
        assert!(issue.parent.is_none());
        assert!(issue.subtasks.is_empty());
    }

    #[test]
    fn test_deserialize_task_with_parent_and_subtasks() {
        let json = serde_json::json!({
            "id": "epic1",
            "custom_id": "DEV-300",
            "name": "Epic with subtasks",
            "status": {"status": "open", "type": "open"},
            "tags": [{"name": "epic"}],
            "assignees": [],
            "url": "https://app.clickup.com/t/epic1",
            "parent": null,
            "subtasks": [
                {
                    "id": "sub1",
                    "custom_id": "DEV-301",
                    "name": "Subtask A",
                    "status": {"status": "open", "type": "open"},
                    "tags": [],
                    "assignees": [],
                    "url": "https://app.clickup.com/t/sub1",
                    "parent": "epic1"
                },
                {
                    "id": "sub2",
                    "name": "Subtask B",
                    "status": {"status": "closed", "type": "closed"},
                    "tags": [],
                    "assignees": [],
                    "url": "https://app.clickup.com/t/sub2",
                    "parent": "epic1"
                }
            ]
        });

        let task: ClickUpTask = serde_json::from_value(json).unwrap();
        assert!(task.parent.is_none());
        assert_eq!(task.subtasks.as_ref().unwrap().len(), 2);
        assert_eq!(
            task.subtasks.as_ref().unwrap()[0].custom_id,
            Some("DEV-301".to_string())
        );
        assert_eq!(
            task.subtasks.as_ref().unwrap()[1].parent,
            Some("epic1".to_string())
        );

        let issue = map_task(&task);
        assert_eq!(issue.subtasks.len(), 2);
        assert_eq!(issue.subtasks[0].key, "DEV-301");
        assert_eq!(issue.subtasks[1].key, "CU-sub2");
        assert_eq!(issue.subtasks[0].parent, Some("CU-epic1".to_string()));
    }

    #[test]
    fn test_deserialize_task_without_subtasks_field() {
        // ClickUp API may omit subtasks field entirely
        let json = serde_json::json!({
            "id": "task1",
            "name": "Simple task",
            "status": {"status": "open", "type": "open"},
            "tags": [],
            "assignees": [],
            "url": "https://app.clickup.com/t/task1"
        });

        let task: ClickUpTask = serde_json::from_value(json).unwrap();
        assert!(task.parent.is_none());
        assert!(task.subtasks.is_none());

        let issue = map_task(&task);
        assert!(issue.parent.is_none());
        assert!(issue.subtasks.is_empty());
    }

    #[test]
    fn test_map_status_category_name_heuristics() {
        // Explicit types
        assert_eq!(map_status_category(Some("closed"), "Done"), "done");
        assert_eq!(map_status_category(Some("done"), "Complete"), "done");

        // Custom statuses — name-based mapping
        assert_eq!(map_status_category(Some("custom"), "Backlog"), "backlog");
        assert_eq!(
            map_status_category(Some("custom"), "Product Backlog"),
            "backlog"
        );
        assert_eq!(map_status_category(Some("custom"), "To Do"), "todo");
        assert_eq!(map_status_category(Some("custom"), "New"), "todo");
        assert_eq!(
            map_status_category(Some("custom"), "In Progress"),
            "in_progress"
        );
        assert_eq!(
            map_status_category(Some("custom"), "Code Review"),
            "in_progress"
        );
        assert_eq!(map_status_category(Some("custom"), "Doing"), "in_progress");
        assert_eq!(map_status_category(Some("custom"), "Active"), "in_progress");
        assert_eq!(map_status_category(Some("custom"), "Done"), "done");
        assert_eq!(map_status_category(Some("custom"), "Completed"), "done");
        assert_eq!(map_status_category(Some("custom"), "Resolved"), "done");
        assert_eq!(
            map_status_category(Some("custom"), "Cancelled"),
            "cancelled"
        );
        assert_eq!(map_status_category(Some("custom"), "Archived"), "cancelled");
        assert_eq!(map_status_category(Some("custom"), "Rejected"), "cancelled");

        // Open type — defaults to "todo"
        assert_eq!(map_status_category(Some("open"), "Open"), "todo");

        // Unknown custom status — defaults to "in_progress"
        assert_eq!(
            map_status_category(Some("custom"), "Some Custom Status"),
            "in_progress"
        );
    }

    #[test]
    fn test_priority_sort_key() {
        assert_eq!(priority_sort_key(Some("urgent")), 1);
        assert_eq!(priority_sort_key(Some("high")), 2);
        assert_eq!(priority_sort_key(Some("normal")), 3);
        assert_eq!(priority_sort_key(Some("low")), 4);
        assert_eq!(priority_sort_key(None), 5);
    }

    // =========================================================================
    // Integration tests with httpmock
    // =========================================================================

    mod integration {
        use super::*;
        use httpmock::prelude::*;

        fn create_test_client(server: &MockServer) -> ClickUpClient {
            ClickUpClient::with_base_url(server.base_url(), "12345", token("pk_test_token"))
        }

        fn create_test_client_with_team(server: &MockServer) -> ClickUpClient {
            ClickUpClient::with_base_url(server.base_url(), "12345", token("pk_test_token"))
                .with_team_id("9876")
        }

        fn sample_task_json() -> serde_json::Value {
            serde_json::json!({
                "id": "abc123",
                "name": "Test Task",
                "description": "<p>Task description</p>",
                "text_content": "Task description",
                "status": {
                    "status": "open",
                    "type": "open"
                },
                "priority": {
                    "id": "2",
                    "priority": "high",
                    "color": "#ffcc00"
                },
                "tags": [{"name": "bug"}],
                "assignees": [{"id": 1, "username": "dev1"}],
                "creator": {"id": 2, "username": "creator"},
                "url": "https://app.clickup.com/t/abc123",
                "date_created": "1704067200000",
                "date_updated": "1704153600000"
            })
        }

        fn sample_closed_task_json() -> serde_json::Value {
            serde_json::json!({
                "id": "def456",
                "name": "Closed Task",
                "status": {
                    "status": "done",
                    "type": "closed"
                },
                "tags": [],
                "assignees": [],
                "url": "https://app.clickup.com/t/def456",
                "date_created": "1704067200000",
                "date_updated": "1704153600000"
            })
        }

        fn sample_task_with_custom_id_json() -> serde_json::Value {
            serde_json::json!({
                "id": "abc123",
                "custom_id": "DEV-42",
                "name": "Task with custom ID",
                "status": {
                    "status": "open",
                    "type": "open"
                },
                "tags": [],
                "assignees": [],
                "url": "https://app.clickup.com/t/abc123",
                "date_created": "1704067200000",
                "date_updated": "1704153600000"
            })
        }

        #[tokio::test]
        async fn test_get_issues() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/list/12345/task")
                    .header("Authorization", "pk_test_token");
                then.status(200)
                    .json_body(serde_json::json!({"tasks": [sample_task_json()]}));
            });

            let client = create_test_client(&server);
            let issues = client
                .get_issues(IssueFilter::default())
                .await
                .unwrap()
                .items;

            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].key, "CU-abc123");
            assert_eq!(issues[0].title, "Test Task");
            assert_eq!(issues[0].source, "clickup");
            assert_eq!(issues[0].priority, Some("high".to_string()));
            // Verify ISO 8601 timestamps
            assert_eq!(
                issues[0].created_at,
                Some("2024-01-01T00:00:00Z".to_string())
            );
        }

        #[tokio::test]
        async fn test_get_issues_with_filters() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/list/12345/task")
                    .query_param("include_closed", "true")
                    .query_param("subtasks", "true")
                    .query_param("tags[]", "bug");
                then.status(200).json_body(
                    serde_json::json!({"tasks": [sample_task_json(), sample_closed_task_json()]}),
                );
            });

            let client = create_test_client(&server);
            let issues = client
                .get_issues(IssueFilter {
                    state: Some("all".to_string()),
                    labels: Some(vec!["bug".to_string()]),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;

            assert_eq!(issues.len(), 2);
        }

        #[tokio::test]
        async fn test_get_issues_state_filter_open() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/list/12345/task");
                then.status(200).json_body(serde_json::json!({
                    "tasks": [sample_task_json(), sample_closed_task_json()]
                }));
            });

            let client = create_test_client(&server);
            let issues = client
                .get_issues(IssueFilter {
                    state: Some("open".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;

            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].state, "open");
        }

        #[tokio::test]
        async fn test_get_issues_state_filter_closed() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/list/12345/task")
                    .query_param("include_closed", "true");
                then.status(200).json_body(serde_json::json!({
                    "tasks": [sample_task_json(), sample_closed_task_json()]
                }));
            });

            let client = create_test_client(&server);
            let issues = client
                .get_issues(IssueFilter {
                    state: Some("closed".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;

            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].state, "closed");
        }

        #[tokio::test]
        async fn test_get_issues_pagination() {
            let server = MockServer::start();

            let tasks: Vec<serde_json::Value> = (0..5)
                .map(|i| {
                    serde_json::json!({
                        "id": format!("task{}", i),
                        "name": format!("Task {}", i),
                        "status": {"status": "open", "type": "open"},
                        "tags": [],
                        "assignees": [],
                        "url": format!("https://app.clickup.com/t/task{}", i),
                        "date_created": "1704067200000",
                        "date_updated": "1704153600000"
                    })
                })
                .collect();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/list/12345/task")
                    .query_param("page", "0");
                then.status(200)
                    .json_body(serde_json::json!({"tasks": tasks}));
            });

            let client = create_test_client(&server);

            let issues = client
                .get_issues(IssueFilter {
                    limit: Some(2),
                    offset: Some(1),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;

            assert_eq!(issues.len(), 2);
            assert_eq!(issues[0].key, "CU-task1");
            assert_eq!(issues[1].key, "CU-task2");
        }

        #[tokio::test]
        async fn test_get_issues_limit_zero() {
            // No server needed — should return immediately without making API calls
            let client = ClickUpClient::new("12345", token("token"));
            let issues = client
                .get_issues(IssueFilter {
                    limit: Some(0),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;

            assert!(issues.is_empty());
        }

        #[tokio::test]
        async fn test_get_issues_multi_page() {
            let server = MockServer::start();

            // Page 0: 100 tasks
            let page0_tasks: Vec<serde_json::Value> = (0..100)
                .map(|i| {
                    serde_json::json!({
                        "id": format!("task{}", i),
                        "name": format!("Task {}", i),
                        "status": {"status": "open", "type": "open"},
                        "tags": [],
                        "assignees": [],
                        "url": format!("https://app.clickup.com/t/task{}", i),
                        "date_created": "1704067200000",
                        "date_updated": "1704153600000"
                    })
                })
                .collect();

            // Page 1: 50 tasks
            let page1_tasks: Vec<serde_json::Value> = (100..150)
                .map(|i| {
                    serde_json::json!({
                        "id": format!("task{}", i),
                        "name": format!("Task {}", i),
                        "status": {"status": "open", "type": "open"},
                        "tags": [],
                        "assignees": [],
                        "url": format!("https://app.clickup.com/t/task{}", i),
                        "date_created": "1704067200000",
                        "date_updated": "1704153600000"
                    })
                })
                .collect();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/list/12345/task")
                    .query_param("page", "0");
                then.status(200)
                    .json_body(serde_json::json!({"tasks": page0_tasks}));
            });

            server.mock(|when, then| {
                when.method(GET)
                    .path("/list/12345/task")
                    .query_param("page", "1");
                then.status(200)
                    .json_body(serde_json::json!({"tasks": page1_tasks}));
            });

            let client = create_test_client(&server);

            // Request 120 tasks — should fetch 2 pages
            let issues = client
                .get_issues(IssueFilter {
                    limit: Some(120),
                    offset: Some(0),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;

            assert_eq!(issues.len(), 120);
            assert_eq!(issues[0].key, "CU-task0");
            assert_eq!(issues[99].key, "CU-task99");
            assert_eq!(issues[100].key, "CU-task100");
            assert_eq!(issues[119].key, "CU-task119");
        }

        #[tokio::test]
        async fn test_get_issue() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/task/abc123");
                then.status(200).json_body(sample_task_json());
            });

            let client = create_test_client(&server);
            let issue = client.get_issue("CU-abc123").await.unwrap();

            assert_eq!(issue.key, "CU-abc123");
            assert_eq!(issue.title, "Test Task");
            assert_eq!(issue.priority, Some("high".to_string()));
        }

        #[tokio::test]
        async fn test_get_issue_by_custom_id() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/task/DEV-42")
                    .query_param("custom_task_ids", "true")
                    .query_param("team_id", "9876");
                then.status(200)
                    .json_body(sample_task_with_custom_id_json());
            });

            let client = create_test_client_with_team(&server);
            let issue = client.get_issue("DEV-42").await.unwrap();

            assert_eq!(issue.key, "DEV-42");
            assert_eq!(issue.title, "Task with custom ID");
        }

        #[tokio::test]
        async fn test_get_issue_custom_id_without_team_fails() {
            let client = ClickUpClient::new("12345", token("token"));
            let result = client.get_issue("DEV-42").await;
            assert!(result.is_err());
        }

        #[tokio::test]
        async fn test_create_issue_with_custom_id_retry() {
            let server = MockServer::start();

            // POST returns task without custom_id
            server.mock(|when, then| {
                when.method(POST)
                    .path("/list/12345/task")
                    .body_includes("\"name\":\"New Task\"");
                then.status(200).json_body(sample_task_json());
            });

            // GET retry returns task with custom_id
            let mut task_with_custom_id = sample_task_json();
            task_with_custom_id["custom_id"] = serde_json::json!("DEV-100");

            server.mock(|when, then| {
                when.method(GET).path("/task/abc123");
                then.status(200).json_body(task_with_custom_id);
            });

            let client = create_test_client(&server);
            let issue = client
                .create_issue(CreateIssueInput {
                    title: "New Task".to_string(),
                    description: Some("Description".to_string()),
                    labels: vec!["bug".to_string()],
                    ..Default::default()
                })
                .await
                .unwrap();

            // Should use custom_id from retry GET
            assert_eq!(issue.key, "DEV-100");
        }

        #[tokio::test]
        async fn test_create_issue_fallback_without_custom_id() {
            let server = MockServer::start();

            // POST returns task without custom_id
            server.mock(|when, then| {
                when.method(POST)
                    .path("/list/12345/task")
                    .body_includes("\"name\":\"New Task\"");
                then.status(200).json_body(sample_task_json());
            });

            // GET retry also returns without custom_id
            server.mock(|when, then| {
                when.method(GET).path("/task/abc123");
                then.status(200).json_body(sample_task_json());
            });

            let client = create_test_client(&server);
            let issue = client
                .create_issue(CreateIssueInput {
                    title: "New Task".to_string(),
                    ..Default::default()
                })
                .await
                .unwrap();

            // Fallback to CU-{id}
            assert_eq!(issue.key, "CU-abc123");
        }

        #[tokio::test]
        async fn test_create_issue_with_priority() {
            let server = MockServer::start();

            // Return task with custom_id to skip retry
            let mut task = sample_task_json();
            task["custom_id"] = serde_json::json!("DEV-101");

            server.mock(|when, then| {
                when.method(POST)
                    .path("/list/12345/task")
                    .body_includes("\"priority\":1");
                then.status(200).json_body(task);
            });

            let client = create_test_client(&server);
            let result = client
                .create_issue(CreateIssueInput {
                    title: "Urgent Task".to_string(),
                    priority: Some("urgent".to_string()),
                    ..Default::default()
                })
                .await;

            assert!(result.is_ok());
            assert_eq!(result.unwrap().key, "DEV-101");
        }

        #[tokio::test]
        async fn test_update_issue() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(PUT)
                    .path("/task/abc123")
                    .body_includes("\"name\":\"Updated Task\"");
                then.status(200).json_body(sample_task_json());
            });

            server.mock(|when, then| {
                when.method(GET).path("/task/abc123");
                then.status(200).json_body(sample_task_json());
            });

            let client = create_test_client(&server);
            let issue = client
                .update_issue(
                    "CU-abc123",
                    UpdateIssueInput {
                        title: Some("Updated Task".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();

            assert_eq!(issue.key, "CU-abc123");
        }

        #[tokio::test]
        async fn test_update_issue_by_custom_id() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(PUT)
                    .path("/task/DEV-42")
                    .query_param("custom_task_ids", "true")
                    .query_param("team_id", "9876");
                then.status(200)
                    .json_body(sample_task_with_custom_id_json());
            });

            server.mock(|when, then| {
                when.method(GET)
                    .path("/task/DEV-42")
                    .query_param("custom_task_ids", "true")
                    .query_param("team_id", "9876");
                then.status(200)
                    .json_body(sample_task_with_custom_id_json());
            });

            let client = create_test_client_with_team(&server);
            let issue = client
                .update_issue(
                    "DEV-42",
                    UpdateIssueInput {
                        title: Some("Updated".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();

            assert_eq!(issue.key, "DEV-42");
        }

        #[tokio::test]
        async fn test_update_issue_state_mapping() {
            let server = MockServer::start();

            // Mock list info endpoint for status resolution
            server.mock(|when, then| {
                when.method(GET).path("/list/12345");
                then.status(200).json_body(serde_json::json!({
                    "statuses": [
                        {"status": "to do", "type": "open"},
                        {"status": "in progress", "type": "custom"},
                        {"status": "complete", "type": "closed"}
                    ]
                }));
            });

            server.mock(|when, then| {
                when.method(PUT)
                    .path("/task/abc123")
                    .body_includes("\"status\":\"complete\"");
                then.status(200).json_body(sample_task_json());
            });

            server.mock(|when, then| {
                when.method(GET).path("/task/abc123");
                then.status(200).json_body(sample_task_json());
            });

            let client = create_test_client(&server);
            let result = client
                .update_issue(
                    "CU-abc123",
                    UpdateIssueInput {
                        state: Some("closed".to_string()),
                        ..Default::default()
                    },
                )
                .await;

            assert!(result.is_ok());
        }

        /// Regression test for #117: PUT response returns stale status,
        /// but re-fetched GET response reflects the actual closed state.
        #[tokio::test]
        async fn test_update_issue_state_refetch_returns_fresh_state() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/list/12345");
                then.status(200).json_body(serde_json::json!({
                    "statuses": [
                        {"status": "to do", "type": "open"},
                        {"status": "complete", "type": "closed"}
                    ]
                }));
            });

            // PUT returns stale "open" status (ClickUp behavior)
            server.mock(|when, then| {
                when.method(PUT)
                    .path("/task/abc123")
                    .body_includes("\"status\":\"complete\"");
                then.status(200).json_body(sample_task_json()); // status.type = "open"
            });

            // GET returns the updated "closed" status
            server.mock(|when, then| {
                when.method(GET).path("/task/abc123");
                then.status(200).json_body(serde_json::json!({
                    "id": "abc123",
                    "name": "Test Task",
                    "status": {
                        "status": "complete",
                        "type": "closed"
                    },
                    "tags": [{"name": "bug"}],
                    "assignees": [{"id": 1, "username": "dev1"}],
                    "url": "https://app.clickup.com/t/abc123",
                    "date_created": "1704067200000",
                    "date_updated": "1704153600000"
                }));
            });

            let client = create_test_client(&server);
            let issue = client
                .update_issue(
                    "CU-abc123",
                    UpdateIssueInput {
                        state: Some("closed".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();

            assert_eq!(issue.state, "closed");
        }

        #[tokio::test]
        async fn test_update_issue_state_open_mapping() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/list/12345");
                then.status(200).json_body(serde_json::json!({
                    "statuses": [
                        {"status": "to do", "type": "open"},
                        {"status": "complete", "type": "closed"}
                    ]
                }));
            });

            server.mock(|when, then| {
                when.method(PUT)
                    .path("/task/abc123")
                    .body_includes("\"status\":\"to do\"");
                then.status(200).json_body(sample_task_json());
            });

            server.mock(|when, then| {
                when.method(GET).path("/task/abc123");
                then.status(200).json_body(sample_task_json());
            });

            let client = create_test_client(&server);
            let result = client
                .update_issue(
                    "CU-abc123",
                    UpdateIssueInput {
                        state: Some("open".to_string()),
                        ..Default::default()
                    },
                )
                .await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_update_issue_exact_status_name() {
            let server = MockServer::start();

            // Exact status name — no list lookup needed
            server.mock(|when, then| {
                when.method(PUT)
                    .path("/task/abc123")
                    .body_includes("\"status\":\"in progress\"");
                then.status(200).json_body(sample_task_json());
            });

            server.mock(|when, then| {
                when.method(GET).path("/task/abc123");
                then.status(200).json_body(sample_task_json());
            });

            let client = create_test_client(&server);
            let result = client
                .update_issue(
                    "CU-abc123",
                    UpdateIssueInput {
                        state: Some("in progress".to_string()),
                        ..Default::default()
                    },
                )
                .await;

            assert!(result.is_ok());
        }

        #[tokio::test]
        async fn test_get_comments() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/task/abc123/comment");
                then.status(200).json_body(serde_json::json!({
                    "comments": [{
                        "id": "1",
                        "comment_text": "Looks good!",
                        "user": {"id": 1, "username": "reviewer"},
                        "date": "1705312800000"
                    }]
                }));
            });

            let client = create_test_client(&server);
            let comments = client.get_comments("CU-abc123").await.unwrap().items;

            assert_eq!(comments.len(), 1);
            assert_eq!(comments[0].body, "Looks good!");
            assert_eq!(comments[0].author.as_ref().unwrap().username, "reviewer");
            // Verify ISO 8601 timestamp
            assert_eq!(
                comments[0].created_at,
                Some("2024-01-15T10:00:00Z".to_string())
            );
        }

        #[tokio::test]
        async fn test_add_comment() {
            let server = MockServer::start();

            // ClickUp POST /comment returns minimal response (id as number, no comment_text)
            server.mock(|when, then| {
                when.method(POST)
                    .path("/task/abc123/comment")
                    .body_includes("\"comment_text\":\"My comment\"");
                then.status(200).json_body(serde_json::json!({
                    "id": 458315,
                    "hist_id": "26b2d7f1-test",
                    "date": 1705312800000_i64
                }));
            });

            let client = create_test_client(&server);
            let comment = IssueProvider::add_comment(&client, "CU-abc123", "My comment")
                .await
                .unwrap();

            assert_eq!(comment.body, "My comment");
            assert_eq!(comment.id, "458315");
            assert_eq!(comment.created_at, Some("2024-01-15T10:00:00Z".to_string()));
        }

        #[tokio::test]
        async fn test_handle_response_401() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/list/12345/task");
                then.status(401).body("Token invalid");
            });

            let client = create_test_client(&server);
            let result = client.get_issues(IssueFilter::default()).await;

            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(matches!(err, Error::Unauthorized(_)));
        }

        #[tokio::test]
        async fn test_handle_response_404() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/task/nonexistent");
                then.status(404).body("Task not found");
            });

            let client = create_test_client(&server);
            let result = client.get_issue("CU-nonexistent").await;

            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(matches!(err, Error::NotFound(_)));
        }

        #[tokio::test]
        async fn test_handle_response_500() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/list/12345/task");
                then.status(500).body("Internal Server Error");
            });

            let client = create_test_client(&server);
            let result = client.get_issues(IssueFilter::default()).await;

            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(matches!(err, Error::ServerError { .. }));
        }

        #[tokio::test]
        async fn test_mr_methods_unsupported() {
            let client = ClickUpClient::new("12345", token("token"));

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

        #[tokio::test]
        async fn test_get_current_user() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/list/12345/task");
                then.status(200).json_body(serde_json::json!({"tasks": []}));
            });

            let client = create_test_client(&server);
            let user = client.get_current_user().await.unwrap();

            assert_eq!(user.username, "clickup-user");
        }

        #[tokio::test]
        async fn test_get_current_user_auth_failure() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/list/12345/task");
                then.status(401).body("Unauthorized");
            });

            let client = create_test_client(&server);
            let result = client.get_current_user().await;

            assert!(result.is_err());
            assert!(matches!(result.unwrap_err(), Error::Unauthorized(_)));
        }

        #[tokio::test]
        async fn test_get_issue_includes_subtasks() {
            let server = MockServer::start();

            let task_with_subtasks = serde_json::json!({
                "id": "epic1",
                "custom_id": "DEV-400",
                "name": "Epic Task",
                "status": {"status": "open", "type": "open"},
                "tags": [{"name": "epic"}],
                "assignees": [],
                "creator": {"id": 1, "username": "author"},
                "url": "https://app.clickup.com/t/epic1",
                "date_created": "1704067200000",
                "date_updated": "1704153600000",
                "subtasks": [
                    {
                        "id": "sub1",
                        "custom_id": "DEV-401",
                        "name": "Subtask 1",
                        "status": {"status": "open", "type": "open"},
                        "tags": [],
                        "assignees": [],
                        "url": "https://app.clickup.com/t/sub1",
                        "parent": "epic1"
                    },
                    {
                        "id": "sub2",
                        "custom_id": "DEV-402",
                        "name": "Subtask 2",
                        "status": {"status": "closed", "type": "closed"},
                        "tags": [],
                        "assignees": [],
                        "url": "https://app.clickup.com/t/sub2",
                        "parent": "epic1"
                    }
                ]
            });

            server.mock(|when, then| {
                when.method(GET)
                    .path("/task/epic1")
                    .query_param("include_subtasks", "true");
                then.status(200).json_body(task_with_subtasks);
            });

            let client = create_test_client(&server);
            let issue = client.get_issue("CU-epic1").await.unwrap();

            assert_eq!(issue.key, "DEV-400");
            assert!(issue.parent.is_none());
            assert_eq!(issue.subtasks.len(), 2);
            assert_eq!(issue.subtasks[0].key, "DEV-401");
            assert_eq!(issue.subtasks[0].title, "Subtask 1");
            assert_eq!(issue.subtasks[0].state, "open");
            assert_eq!(issue.subtasks[0].parent, Some("CU-epic1".to_string()));
            assert_eq!(issue.subtasks[1].key, "DEV-402");
            assert_eq!(issue.subtasks[1].state, "closed");
        }

        #[tokio::test]
        async fn test_get_issue_no_subtasks() {
            let server = MockServer::start();

            let task = sample_task_json();

            server.mock(|when, then| {
                when.method(GET)
                    .path("/task/abc123")
                    .query_param("include_subtasks", "true");
                then.status(200).json_body(task);
            });

            let client = create_test_client(&server);
            let issue = client.get_issue("CU-abc123").await.unwrap();

            assert!(issue.subtasks.is_empty());
            assert!(issue.parent.is_none());
        }

        #[tokio::test]
        async fn test_get_issue_custom_id_includes_subtasks() {
            let server = MockServer::start();

            let task = serde_json::json!({
                "id": "task1",
                "custom_id": "DEV-500",
                "name": "Task via custom ID",
                "status": {"status": "open", "type": "open"},
                "tags": [],
                "assignees": [],
                "url": "https://app.clickup.com/t/task1",
                "parent": "parent123",
                "subtasks": []
            });

            server.mock(|when, then| {
                when.method(GET)
                    .path("/task/DEV-500")
                    .query_param("custom_task_ids", "true")
                    .query_param("team_id", "9876")
                    .query_param("include_subtasks", "true");
                then.status(200).json_body(task);
            });

            let client = create_test_client_with_team(&server);
            let issue = client.get_issue("DEV-500").await.unwrap();

            assert_eq!(issue.key, "DEV-500");
            assert_eq!(issue.parent, Some("CU-parent123".to_string()));
            assert!(issue.subtasks.is_empty());
        }

        #[tokio::test]
        async fn test_update_issue_with_parent_id() {
            let server = MockServer::start();

            // Mock: resolve parent task by custom ID
            let parent_task = serde_json::json!({
                "id": "parent_native_id",
                "custom_id": "DEV-600",
                "name": "Parent Epic",
                "status": {"status": "open", "type": "open"},
                "tags": [],
                "assignees": [],
                "url": "https://app.clickup.com/t/parent_native_id"
            });

            server.mock(|when, then| {
                when.method(GET)
                    .path("/task/DEV-600")
                    .query_param("custom_task_ids", "true")
                    .query_param("team_id", "9876");
                then.status(200).json_body(parent_task);
            });

            // Mock: update task with parent
            let updated_task = serde_json::json!({
                "id": "child1",
                "custom_id": "DEV-601",
                "name": "Child Task",
                "status": {"status": "open", "type": "open"},
                "tags": [],
                "assignees": [],
                "url": "https://app.clickup.com/t/child1",
                "parent": "parent_native_id"
            });

            server.mock(|when, then| {
                when.method(PUT)
                    .path("/task/DEV-601")
                    .query_param("custom_task_ids", "true")
                    .query_param("team_id", "9876")
                    .body_includes("\"parent\":\"parent_native_id\"");
                then.status(200).json_body(updated_task.clone());
            });

            server.mock(|when, then| {
                when.method(GET)
                    .path("/task/DEV-601")
                    .query_param("custom_task_ids", "true")
                    .query_param("team_id", "9876");
                then.status(200).json_body(updated_task);
            });

            let client = create_test_client_with_team(&server);
            let issue = client
                .update_issue(
                    "DEV-601",
                    UpdateIssueInput {
                        parent_id: Some("DEV-600".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();

            assert_eq!(issue.key, "DEV-601");
            assert_eq!(issue.parent, Some("CU-parent_native_id".to_string()));
        }

        #[tokio::test]
        async fn test_create_issue_with_parent() {
            let server = MockServer::start();

            // Mock: resolve parent task
            let parent_task = serde_json::json!({
                "id": "parent_id",
                "custom_id": "DEV-700",
                "name": "Parent",
                "status": {"status": "open", "type": "open"},
                "tags": [],
                "assignees": [],
                "url": "https://app.clickup.com/t/parent_id"
            });

            server.mock(|when, then| {
                when.method(GET)
                    .path("/task/DEV-700")
                    .query_param("custom_task_ids", "true")
                    .query_param("team_id", "9876");
                then.status(200).json_body(parent_task);
            });

            // Mock: create task with parent
            let created_task = serde_json::json!({
                "id": "new_child",
                "custom_id": "DEV-701",
                "name": "New Subtask",
                "status": {"status": "open", "type": "open"},
                "tags": [],
                "assignees": [],
                "url": "https://app.clickup.com/t/new_child",
                "parent": "parent_id"
            });

            server.mock(|when, then| {
                when.method(POST)
                    .path("/list/12345/task")
                    .body_includes("\"parent\":\"parent_id\"");
                then.status(200).json_body(created_task);
            });

            let client = create_test_client_with_team(&server);
            let issue = client
                .create_issue(CreateIssueInput {
                    title: "New Subtask".to_string(),
                    parent: Some("DEV-700".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap();

            assert_eq!(issue.key, "DEV-701");
            assert_eq!(issue.parent, Some("CU-parent_id".to_string()));
        }

        #[tokio::test]
        async fn test_get_issues_search_filter() {
            let server = MockServer::start_async().await;

            server.mock(|when, then| {
                when.method(GET).path("/list/12345/task");
                then.status(200).json_body(serde_json::json!({
                    "tasks": [
                        {
                            "id": "1", "name": "Fix login bug",
                            "description": "Authentication fails",
                            "text_content": "Authentication fails",
                            "status": {"status": "open", "type": "open"},
                            "tags": [], "assignees": [],
                            "url": "https://app.clickup.com/t/1"
                        },
                        {
                            "id": "2", "name": "Add dark mode",
                            "description": "Theme support",
                            "text_content": "Theme support",
                            "status": {"status": "open", "type": "open"},
                            "tags": [], "assignees": [],
                            "url": "https://app.clickup.com/t/2"
                        },
                        {
                            "id": "3", "name": "Update docs",
                            "description": "Fix login instructions",
                            "text_content": "Fix login instructions",
                            "status": {"status": "open", "type": "open"},
                            "tags": [], "assignees": [],
                            "url": "https://app.clickup.com/t/3"
                        }
                    ]
                }));
            });

            let client = create_test_client(&server);

            // Search by title
            let issues = client
                .get_issues(IssueFilter {
                    search: Some("login".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;
            assert_eq!(issues.len(), 2);
            assert!(issues.iter().any(|i| i.title == "Fix login bug"));
            assert!(issues.iter().any(|i| i.title == "Update docs")); // matches description

            // Search by key
            let issues = client
                .get_issues(IssueFilter {
                    search: Some("CU-2".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;
            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].title, "Add dark mode");

            // Search — no matches
            let issues = client
                .get_issues(IssueFilter {
                    search: Some("nonexistent".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;
            assert!(issues.is_empty());
        }

        #[tokio::test]
        async fn test_get_issues_sort_by_priority() {
            let server = MockServer::start_async().await;

            server.mock(|when, then| {
                when.method(GET).path("/list/12345/task");
                then.status(200).json_body(serde_json::json!({
                    "tasks": [
                        {
                            "id": "1", "name": "Low task",
                            "status": {"status": "open", "type": "open"},
                            "priority": {"id": "4", "priority": "low"},
                            "tags": [], "assignees": [],
                            "url": "https://app.clickup.com/t/1"
                        },
                        {
                            "id": "2", "name": "Urgent task",
                            "status": {"status": "open", "type": "open"},
                            "priority": {"id": "1", "priority": "urgent"},
                            "tags": [], "assignees": [],
                            "url": "https://app.clickup.com/t/2"
                        },
                        {
                            "id": "3", "name": "Normal task",
                            "status": {"status": "open", "type": "open"},
                            "priority": {"id": "3", "priority": "normal"},
                            "tags": [], "assignees": [],
                            "url": "https://app.clickup.com/t/3"
                        }
                    ]
                }));
            });

            let client = create_test_client(&server);

            // Sort by priority descending (most urgent first)
            let result = client
                .get_issues(IssueFilter {
                    sort_by: Some("priority".to_string()),
                    sort_order: Some("asc".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap();
            assert_eq!(result.items[0].priority, Some("urgent".to_string()));
            assert_eq!(result.items[1].priority, Some("normal".to_string()));
            assert_eq!(result.items[2].priority, Some("low".to_string()));

            // Verify sort_info is populated
            let sort_info = result.sort_info.unwrap();
            assert_eq!(sort_info.sort_by, Some("priority".to_string()));
            assert!(sort_info.available_sorts.contains(&"priority".into()));
        }

        #[tokio::test]
        async fn test_get_issues_sort_by_title() {
            let server = MockServer::start_async().await;

            server.mock(|when, then| {
                when.method(GET).path("/list/12345/task");
                then.status(200).json_body(serde_json::json!({
                    "tasks": [
                        {
                            "id": "1", "name": "Charlie",
                            "status": {"status": "open", "type": "open"},
                            "tags": [], "assignees": [],
                            "url": "https://app.clickup.com/t/1"
                        },
                        {
                            "id": "2", "name": "Alpha",
                            "status": {"status": "open", "type": "open"},
                            "tags": [], "assignees": [],
                            "url": "https://app.clickup.com/t/2"
                        },
                        {
                            "id": "3", "name": "Bravo",
                            "status": {"status": "open", "type": "open"},
                            "tags": [], "assignees": [],
                            "url": "https://app.clickup.com/t/3"
                        }
                    ]
                }));
            });

            let client = create_test_client(&server);

            let result = client
                .get_issues(IssueFilter {
                    sort_by: Some("title".to_string()),
                    sort_order: Some("asc".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap();
            assert_eq!(result.items[0].title, "Alpha");
            assert_eq!(result.items[1].title, "Bravo");
            assert_eq!(result.items[2].title, "Charlie");
        }

        #[tokio::test]
        async fn test_get_statuses_category_mapping() {
            let server = MockServer::start_async().await;

            server.mock(|when, then| {
                when.method(GET).path("/list/12345");
                then.status(200).json_body(serde_json::json!({
                    "statuses": [
                        {"status": "Backlog", "type": "custom", "color": "#aaa", "orderindex": 0},
                        {"status": "To Do", "type": "open", "color": "#bbb", "orderindex": 1},
                        {"status": "In Progress", "type": "custom", "color": "#ccc", "orderindex": 2},
                        {"status": "In Review", "type": "custom", "color": "#ddd", "orderindex": 3},
                        {"status": "Done", "type": "closed", "color": "#eee", "orderindex": 4},
                        {"status": "Cancelled", "type": "custom", "color": "#fff", "orderindex": 5},
                        {"status": "Archived", "type": "custom", "color": "#000", "orderindex": 6}
                    ]
                }));
            });

            let client = create_test_client(&server);
            let statuses = client.get_statuses().await.unwrap().items;

            assert_eq!(statuses.len(), 7);
            assert_eq!(statuses[0].name, "Backlog");
            assert_eq!(statuses[0].category, "backlog");
            assert_eq!(statuses[1].name, "To Do");
            assert_eq!(statuses[1].category, "todo");
            assert_eq!(statuses[2].name, "In Progress");
            assert_eq!(statuses[2].category, "in_progress");
            assert_eq!(statuses[3].name, "In Review");
            assert_eq!(statuses[3].category, "in_progress");
            assert_eq!(statuses[4].name, "Done");
            assert_eq!(statuses[4].category, "done");
            assert_eq!(statuses[5].name, "Cancelled");
            assert_eq!(statuses[5].category, "cancelled");
            assert_eq!(statuses[6].name, "Archived");
            assert_eq!(statuses[6].category, "cancelled");
        }

        #[tokio::test]
        async fn test_get_issues_state_category_filter() {
            let server = MockServer::start_async().await;

            // Mock for get_statuses (called by stateCategory filter)
            server.mock(|when, then| {
                when.method(GET).path("/list/12345").query_param_exists("!");
                then.status(200).json_body(serde_json::json!({
                    "statuses": [
                        {"status": "Backlog", "type": "custom"},
                        {"status": "To Do", "type": "open"},
                        {"status": "In Progress", "type": "custom"},
                        {"status": "Done", "type": "closed"}
                    ]
                }));
            });

            // This exact path mock for list info (no query params)
            server.mock(|when, then| {
                when.method(GET).path("/list/12345");
                then.status(200).json_body(serde_json::json!({
                    "statuses": [
                        {"status": "Backlog", "type": "custom"},
                        {"status": "To Do", "type": "open"},
                        {"status": "In Progress", "type": "custom"},
                        {"status": "Done", "type": "closed"}
                    ]
                }));
            });

            server.mock(|when, then| {
                when.method(GET).path("/list/12345/task");
                then.status(200).json_body(serde_json::json!({
                    "tasks": [
                        {
                            "id": "1", "name": "Backlog task",
                            "status": {"status": "Backlog", "type": "custom"},
                            "tags": [], "assignees": [],
                            "url": "https://app.clickup.com/t/1"
                        },
                        {
                            "id": "2", "name": "In progress task",
                            "status": {"status": "In Progress", "type": "custom"},
                            "tags": [], "assignees": [],
                            "url": "https://app.clickup.com/t/2"
                        },
                        {
                            "id": "3", "name": "Todo task",
                            "status": {"status": "To Do", "type": "open"},
                            "tags": [], "assignees": [],
                            "url": "https://app.clickup.com/t/3"
                        }
                    ]
                }));
            });

            let client = create_test_client(&server);

            // Filter by in_progress category
            let issues = client
                .get_issues(IssueFilter {
                    state_category: Some("in_progress".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;
            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].title, "In progress task");

            // Filter by backlog category
            let issues = client
                .get_issues(IssueFilter {
                    state_category: Some("backlog".to_string()),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items;
            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].title, "Backlog task");
        }

        #[tokio::test]
        async fn test_get_issue_attachments_maps_all_fields() {
            let server = MockServer::start();

            let task_json = serde_json::json!({
                "id": "abc123",
                "name": "Test",
                "status": {"status": "open", "type": "open"},
                "tags": [], "assignees": [],
                "url": "https://app.clickup.com/t/abc123",
                "date_created": "1704067200000",
                "date_updated": "1704067200000",
                "attachments": [
                    {
                        "id": "att-1",
                        "title": "screen.png",
                        "url": "https://attachments.clickup.com/abc/screen.png",
                        "size": "12345",
                        "extension": "png",
                        "mimetype": "image/png",
                        "date": "1704067200000",
                        "user": {"id": 7, "username": "uploader"}
                    }
                ]
            });

            server.mock(|when, then| {
                when.method(GET).path("/task/abc123");
                then.status(200).json_body(task_json);
            });

            let client = create_test_client(&server);
            let assets = client.get_issue_attachments("CU-abc123").await.unwrap();
            assert_eq!(assets.len(), 1);
            let a = &assets[0];
            assert_eq!(a.id, "att-1");
            assert_eq!(a.filename, "screen.png");
            assert_eq!(a.mime_type.as_deref(), Some("image/png"));
            assert_eq!(a.size, Some(12345));
            assert_eq!(a.author.as_deref(), Some("uploader"));
        }

        #[tokio::test]
        async fn test_get_issue_attachments_empty_when_none() {
            let server = MockServer::start();

            let task_json = serde_json::json!({
                "id": "abc123",
                "name": "Test",
                "status": {"status": "open", "type": "open"},
                "tags": [], "assignees": [],
                "url": "https://app.clickup.com/t/abc123",
                "date_created": "1704067200000",
                "date_updated": "1704067200000"
            });

            server.mock(|when, then| {
                when.method(GET).path("/task/abc123");
                then.status(200).json_body(task_json);
            });

            let client = create_test_client(&server);
            let assets = client.get_issue_attachments("CU-abc123").await.unwrap();
            assert!(assets.is_empty());
        }

        #[tokio::test]
        async fn test_download_attachment_fetches_bytes() {
            let server = MockServer::start();

            let task_json = serde_json::json!({
                "id": "abc123",
                "name": "Test",
                "status": {"status": "open", "type": "open"},
                "tags": [], "assignees": [],
                "url": "https://app.clickup.com/t/abc123",
                "date_created": "1704067200000",
                "date_updated": "1704067200000",
                "attachments": [
                    {
                        "id": "att-1",
                        "title": "log.txt",
                        "url": format!("{}/download/att-1", server.base_url()),
                    }
                ]
            });

            server.mock(|when, then| {
                when.method(GET).path("/task/abc123");
                then.status(200).json_body(task_json);
            });
            server.mock(|when, then| {
                when.method(GET).path("/download/att-1");
                then.status(200).body("hello world");
            });

            let client = create_test_client(&server);
            let bytes = client
                .download_attachment("CU-abc123", "att-1")
                .await
                .unwrap();
            assert_eq!(bytes, b"hello world");
        }

        #[tokio::test]
        async fn test_download_attachment_not_found() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/task/abc123");
                then.status(200).json_body(serde_json::json!({
                    "id": "abc123", "name": "Test",
                    "status": {"status": "open", "type": "open"},
                    "tags": [], "assignees": [],
                    "url": "https://app.clickup.com/t/abc123",
                    "date_created": "1704067200000",
                    "date_updated": "1704067200000",
                    "attachments": []
                }));
            });

            let client = create_test_client(&server);
            let err = client
                .download_attachment("CU-abc123", "missing")
                .await
                .unwrap_err();
            assert!(matches!(err, Error::NotFound(_)));
        }

        // ===== Custom status support — regression tests for #288 =====
        //
        // Before this change the executor schema constrained the `state`
        // field to enum ["open", "closed"], so ClickUp custom statuses
        // like "in progress" / "review" were unreachable via the MCP
        // layer. The new `status` field forwards the literal status
        // name; it is validated against the list's configured statuses
        // (case-insensitive) and the canonical case from the list is
        // sent in the PUT body.

        fn list_with_statuses() -> serde_json::Value {
            serde_json::json!({
                "statuses": [
                    {"status": "to do", "type": "open"},
                    {"status": "in progress", "type": "custom"},
                    {"status": "review", "type": "custom"},
                    {"status": "complete", "type": "closed"}
                ]
            })
        }

        #[tokio::test]
        async fn test_update_issue_status_sets_custom_status() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/list/12345");
                then.status(200).json_body(list_with_statuses());
            });

            server.mock(|when, then| {
                when.method(PUT)
                    .path("/task/abc123")
                    .body_includes("\"status\":\"in progress\"");
                then.status(200).json_body(sample_task_json());
            });

            server.mock(|when, then| {
                when.method(GET).path("/task/abc123");
                then.status(200).json_body(sample_task_json());
            });

            let client = create_test_client(&server);
            let result = client
                .update_issue(
                    "CU-abc123",
                    UpdateIssueInput {
                        status: Some("in progress".to_string()),
                        ..Default::default()
                    },
                )
                .await;
            assert!(result.is_ok(), "got {:?}", result.err());
        }

        #[tokio::test]
        async fn test_update_issue_status_case_insensitive_match() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/list/12345");
                then.status(200).json_body(list_with_statuses());
            });

            // Input "REVIEW", list has "review" → PUT body sends canonical "review".
            server.mock(|when, then| {
                when.method(PUT)
                    .path("/task/abc123")
                    .body_includes("\"status\":\"review\"");
                then.status(200).json_body(sample_task_json());
            });

            server.mock(|when, then| {
                when.method(GET).path("/task/abc123");
                then.status(200).json_body(sample_task_json());
            });

            let client = create_test_client(&server);
            let result = client
                .update_issue(
                    "CU-abc123",
                    UpdateIssueInput {
                        status: Some("REVIEW".to_string()),
                        ..Default::default()
                    },
                )
                .await;
            assert!(result.is_ok(), "got {:?}", result.err());
        }

        #[tokio::test]
        async fn test_update_issue_status_unknown_fails_with_valid_list() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/list/12345");
                then.status(200).json_body(list_with_statuses());
            });

            let put_mock = server.mock(|when, then| {
                when.method(PUT).path("/task/abc123");
                then.status(200).json_body(sample_task_json());
            });

            let client = create_test_client(&server);
            let err = client
                .update_issue(
                    "CU-abc123",
                    UpdateIssueInput {
                        status: Some("released-to-prod".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .expect_err("unknown status must fail before PUT");
            let msg = format!("{err:?}");
            assert!(msg.contains("Unknown ClickUp status"), "msg: {msg}");
            assert!(msg.contains("released-to-prod"), "msg: {msg}");
            assert!(msg.contains("in progress"), "list of valids missing: {msg}");
            put_mock.assert_calls(0);
        }

        #[tokio::test]
        async fn test_update_issue_status_overrides_state_when_both_set() {
            let server = MockServer::start();

            // List endpoint is only hit by validate_status_name (for
            // `status`) — NOT by resolve_status (`state`); pin that with
            // a single mock that's allowed to be called once.
            let list_mock = server.mock(|when, then| {
                when.method(GET).path("/list/12345");
                then.status(200).json_body(list_with_statuses());
            });

            server.mock(|when, then| {
                when.method(PUT)
                    .path("/task/abc123")
                    // status wins → "in progress", not "to do" (open) /
                    // "complete" (closed).
                    .body_includes("\"status\":\"in progress\"");
                then.status(200).json_body(sample_task_json());
            });

            server.mock(|when, then| {
                when.method(GET).path("/task/abc123");
                then.status(200).json_body(sample_task_json());
            });

            let client = create_test_client(&server);
            let result = client
                .update_issue(
                    "CU-abc123",
                    UpdateIssueInput {
                        state: Some("closed".to_string()),
                        status: Some("in progress".to_string()),
                        ..Default::default()
                    },
                )
                .await;
            assert!(result.is_ok(), "got {:?}", result.err());
            list_mock.assert_calls(1);
        }

        #[tokio::test]
        async fn test_update_issue_state_path_unchanged_when_status_absent() {
            // Regression guard: the legacy `state: "closed"` path must
            // keep working through resolve_status (no behavior change
            // for callers that never set `status`).
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/list/12345");
                then.status(200).json_body(list_with_statuses());
            });

            server.mock(|when, then| {
                when.method(PUT)
                    .path("/task/abc123")
                    .body_includes("\"status\":\"complete\"");
                then.status(200).json_body(sample_task_json());
            });

            server.mock(|when, then| {
                when.method(GET).path("/task/abc123");
                then.status(200).json_body(sample_task_json());
            });

            let client = create_test_client(&server);
            let result = client
                .update_issue(
                    "CU-abc123",
                    UpdateIssueInput {
                        state: Some("closed".to_string()),
                        ..Default::default()
                    },
                )
                .await;
            assert!(result.is_ok(), "got {:?}", result.err());
        }

        #[tokio::test]
        async fn test_update_issue_status_open_keyword_hints_at_state_field() {
            // Regression for review feedback on #290: when a caller
            // accidentally passes "open" / "closed" via `status` instead
            // of `state`, the error must point them at the right field
            // instead of just listing valid custom statuses.
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(GET).path("/list/12345");
                then.status(200).json_body(list_with_statuses());
            });

            let put_mock = server.mock(|when, then| {
                when.method(PUT).path("/task/abc123");
                then.status(200).json_body(sample_task_json());
            });

            let client = create_test_client(&server);
            for keyword in ["open", "Opened", "CLOSED"] {
                let err = client
                    .update_issue(
                        "CU-abc123",
                        UpdateIssueInput {
                            status: Some(keyword.to_string()),
                            ..Default::default()
                        },
                    )
                    .await
                    .expect_err("open/closed via `status` must fail");
                let msg = format!("{err:?}");
                assert!(msg.contains("state` field"), "keyword={keyword} msg={msg}");
            }
            put_mock.assert_calls(0);
        }

        #[tokio::test]
        async fn test_update_issue_status_unknown_truncates_large_status_list() {
            // Some workspaces have 50+ statuses; the error message must
            // not dump all of them verbatim. Truncates at 10 with
            // "…and N more" suffix.
            let server = MockServer::start();

            let many_statuses: Vec<serde_json::Value> = (0..15)
                .map(|i| serde_json::json!({"status": format!("status-{i:02}"), "type": "custom"}))
                .collect();

            server.mock(|when, then| {
                when.method(GET).path("/list/12345");
                then.status(200)
                    .json_body(serde_json::json!({"statuses": many_statuses}));
            });

            let client = create_test_client(&server);
            let err = client
                .update_issue(
                    "CU-abc123",
                    UpdateIssueInput {
                        status: Some("nonexistent".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .expect_err("must fail");
            let msg = format!("{err:?}");
            assert!(msg.contains("status-00"), "preview missing first: {msg}");
            assert!(msg.contains("status-09"), "preview missing 10th: {msg}");
            assert!(
                !msg.contains("status-10"),
                "11th status leaked past truncation: {msg}"
            );
            assert!(
                msg.contains("…and 5 more"),
                "truncation suffix missing: {msg}"
            );
        }
    }
}
