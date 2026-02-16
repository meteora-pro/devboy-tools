//! ClickUp API client implementation.

use async_trait::async_trait;
use devboy_core::{
    Comment, CreateCommentInput, CreateIssueInput, Discussion, Error, FileDiff, Issue, IssueFilter,
    IssueProvider, MergeRequest, MergeRequestProvider, MrFilter, Provider, Result,
    UpdateIssueInput, User,
};
use tracing::{debug, warn};

use crate::types::{
    ClickUpComment, ClickUpCommentList, ClickUpListInfo, ClickUpPriority, ClickUpTask,
    ClickUpTaskList, ClickUpUser, CreateCommentRequest, CreateCommentResponse, CreateTaskRequest,
    UpdateTaskRequest,
};
use crate::DEFAULT_CLICKUP_URL;

/// Maximum number of tasks per page in ClickUp API.
const PAGE_SIZE: u32 = 100;

/// ClickUp API client.
pub struct ClickUpClient {
    base_url: String,
    list_id: String,
    team_id: Option<String>,
    token: String,
    client: reqwest::Client,
}

impl ClickUpClient {
    /// Create a new ClickUp client.
    pub fn new(list_id: impl Into<String>, token: impl Into<String>) -> Self {
        Self::with_base_url(DEFAULT_CLICKUP_URL, list_id, token)
    }

    /// Create a new ClickUp client with a custom base URL (for testing).
    pub fn with_base_url(
        base_url: impl Into<String>,
        list_id: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            list_id: list_id.into(),
            team_id: None,
            token: token.into(),
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
            .header("Authorization", &self.token)
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

    /// Resolve a unified state name ("open"/"closed") to the actual ClickUp status name
    /// by fetching the list's configured statuses.
    /// If the state doesn't match a known type, it's passed as-is (exact status name).
    async fn resolve_status(&self, state: &str) -> Result<String> {
        let status_type = match state {
            "closed" => "closed",
            "open" | "opened" => "open",
            _ => return Ok(state.to_string()),
        };

        let url = format!("{}/list/{}", self.base_url, self.list_id);
        let list_info: ClickUpListInfo = self.get(&url).await?;

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
    let nanos = ((ms % 1000) * 1_000_000) as u32;

    // Format as ISO 8601 using chrono-free manual approach
    // Unix epoch: 1970-01-01T00:00:00Z
    // We use a simple formatting approach via time calculation
    let datetime = time_from_unix(secs, nanos);
    Some(datetime)
}

/// Convert unix timestamp to ISO 8601 string without external crate.
fn time_from_unix(secs: i64, _nanos: u32) -> String {
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
    Issue {
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

// =============================================================================
// Trait implementations
// =============================================================================

#[async_trait]
impl IssueProvider for ClickUpClient {
    async fn get_issues(&self, filter: IssueFilter) -> Result<Vec<Issue>> {
        let limit = filter.limit.unwrap_or(20) as usize;
        let offset = filter.offset.unwrap_or(0) as usize;

        // Calculate which pages we need to fetch
        let start_page = offset / PAGE_SIZE as usize;
        let end_page = (offset + limit).saturating_sub(1) / PAGE_SIZE as usize;

        // Build base query params (without page)
        let mut base_params = vec![];

        let include_closed = matches!(filter.state.as_deref(), Some("closed") | Some("all"));
        if include_closed {
            base_params.push("include_closed=true".to_string());
        }

        base_params.push("subtasks=true".to_string());

        if let Some(assignees) = &filter.assignee {
            base_params.push(format!("assignees[]={}", assignees));
        }

        if let Some(tags) = &filter.labels {
            for tag in tags {
                base_params.push(format!("tags[]={}", tag));
            }
        }

        if let Some(order_by) = &filter.sort_by {
            let cu_order_by = match order_by.as_str() {
                "created_at" | "created" => "created",
                "updated_at" | "updated" => "updated",
                _ => "updated",
            };
            base_params.push(format!("order_by={}", cu_order_by));
        }

        if let Some(order) = &filter.sort_order {
            if order == "asc" {
                base_params.push("reverse=true".to_string());
            }
        }

        // Fetch all needed pages
        let mut all_tasks: Vec<ClickUpTask> = Vec::new();

        for page in start_page..=end_page {
            let mut params = base_params.clone();
            params.push(format!("page={}", page));

            let url = format!(
                "{}/list/{}/task?{}",
                self.base_url,
                self.list_id,
                params.join("&")
            );

            let response: ClickUpTaskList = self.get(&url).await?;
            let page_len = response.tasks.len();
            all_tasks.extend(response.tasks);

            // Stop if this page has fewer than PAGE_SIZE items (no more data)
            if page_len < PAGE_SIZE as usize {
                break;
            }
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

        // Apply offset within first page and limit
        let offset_in_first_page = offset % PAGE_SIZE as usize;
        if offset_in_first_page < issues.len() {
            issues = issues.split_off(offset_in_first_page);
        } else {
            issues.clear();
        }

        issues.truncate(limit);

        Ok(issues)
    }

    async fn get_issue(&self, key: &str) -> Result<Issue> {
        let url = self.task_url(key)?;
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

        let request = CreateTaskRequest {
            name: input.title,
            description: input.description,
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
                if let Ok(fetched) = self.get::<ClickUpTask>(&fetch_url).await {
                    if fetched.custom_id.is_some() {
                        debug!(
                            task_id = task_id,
                            custom_id = ?fetched.custom_id,
                            attempt = attempt,
                            "Got custom_id after retry"
                        );
                        return Ok(map_task(&fetched));
                    }
                }
            }
            warn!(task_id = task_id, "custom_id not available after 3 retries, using POST response");
        }

        Ok(map_task(&task))
    }

    async fn update_issue(&self, key: &str, input: UpdateIssueInput) -> Result<Issue> {
        let url = self.task_url(key)?;

        let status = match input.state {
            Some(s) => Some(self.resolve_status(&s).await?),
            None => None,
        };

        let priority = input.priority.as_deref().and_then(priority_to_clickup);

        let request = UpdateTaskRequest {
            name: input.title,
            description: input.description,
            status,
            priority,
        };

        let task: ClickUpTask = self.put(&url, &request).await?;
        Ok(map_task(&task))
    }

    async fn get_comments(&self, issue_key: &str) -> Result<Vec<Comment>> {
        let base_url = self.task_url(issue_key)?;
        // Append /comment — handle both raw URL and URL with query params
        let url = if base_url.contains('?') {
            let (path, query) = base_url.split_once('?').unwrap();
            format!("{}/comment?{}", path, query)
        } else {
            format!("{}/comment", base_url)
        };
        let response: ClickUpCommentList = self.get(&url).await?;
        Ok(response.comments.iter().map(map_comment).collect())
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

    fn provider_name(&self) -> &'static str {
        "clickup"
    }
}

#[async_trait]
impl MergeRequestProvider for ClickUpClient {
    async fn get_merge_requests(&self, _filter: MrFilter) -> Result<Vec<MergeRequest>> {
        Err(Error::ProviderUnsupported {
            provider: "clickup".to_string(),
            operation: "get_merge_requests".to_string(),
        })
    }

    async fn get_merge_request(&self, _key: &str) -> Result<MergeRequest> {
        Err(Error::ProviderUnsupported {
            provider: "clickup".to_string(),
            operation: "get_merge_request".to_string(),
        })
    }

    async fn get_discussions(&self, _mr_key: &str) -> Result<Vec<Discussion>> {
        Err(Error::ProviderUnsupported {
            provider: "clickup".to_string(),
            operation: "get_discussions".to_string(),
        })
    }

    async fn get_diffs(&self, _mr_key: &str) -> Result<Vec<FileDiff>> {
        Err(Error::ProviderUnsupported {
            provider: "clickup".to_string(),
            operation: "get_diffs".to_string(),
        })
    }

    async fn add_comment(&self, _mr_key: &str, _input: CreateCommentInput) -> Result<Comment> {
        Err(Error::ProviderUnsupported {
            provider: "clickup".to_string(),
            operation: "add_merge_request_comment".to_string(),
        })
    }

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
            ClickUpClient::with_base_url("https://api.clickup.com/api/v2", "12345", "token");
        let url = client.task_url("CU-abc123").unwrap();
        assert_eq!(url, "https://api.clickup.com/api/v2/task/abc123");
    }

    #[test]
    fn test_task_url_custom_id_with_team() {
        let client =
            ClickUpClient::with_base_url("https://api.clickup.com/api/v2", "12345", "token")
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
            ClickUpClient::with_base_url("https://api.clickup.com/api/v2", "12345", "token");
        let result = client.task_url("DEV-42");
        assert!(result.is_err());
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
            ClickUpClient::with_base_url("https://api.clickup.com/api/v2", "12345", "token");
        assert_eq!(client.base_url, "https://api.clickup.com/api/v2");
        assert_eq!(client.list_id, "12345");
    }

    #[test]
    fn test_api_url_strips_trailing_slash() {
        let client =
            ClickUpClient::with_base_url("https://api.clickup.com/api/v2/", "12345", "token");
        assert_eq!(client.base_url, "https://api.clickup.com/api/v2");
    }

    #[test]
    fn test_with_team_id() {
        let client = ClickUpClient::new("12345", "token").with_team_id("9876");
        assert_eq!(client.team_id, Some("9876".to_string()));
    }

    #[test]
    fn test_provider_name() {
        let client = ClickUpClient::new("12345", "token");
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
        };

        let issue = map_task(&task);
        assert_eq!(issue.state, "open");
    }

    // =========================================================================
    // Integration tests with httpmock
    // =========================================================================

    mod integration {
        use super::*;
        use httpmock::prelude::*;

        fn create_test_client(server: &MockServer) -> ClickUpClient {
            ClickUpClient::with_base_url(server.base_url(), "12345", "pk_test_token")
        }

        fn create_test_client_with_team(server: &MockServer) -> ClickUpClient {
            ClickUpClient::with_base_url(server.base_url(), "12345", "pk_test_token")
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
            let issues = client.get_issues(IssueFilter::default()).await.unwrap();

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
                .unwrap();

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
                .unwrap();

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
                .unwrap();

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
                .unwrap();

            assert_eq!(issues.len(), 2);
            assert_eq!(issues[0].key, "CU-task1");
            assert_eq!(issues[1].key, "CU-task2");
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
                .unwrap();

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
            let client = ClickUpClient::new("12345", "token");
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
                    assignees: vec![],
                    priority: None,
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
            let comments = client.get_comments("CU-abc123").await.unwrap();

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
            assert_eq!(
                comment.created_at,
                Some("2024-01-15T10:00:00Z".to_string())
            );
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
            let client = ClickUpClient::new("12345", "token");

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
    }
}
