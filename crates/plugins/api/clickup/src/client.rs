//! ClickUp API client implementation.

use async_trait::async_trait;
use devboy_core::{
    Comment, CreateCommentInput, CreateIssueInput, Discussion, Error, FileDiff, Issue, IssueFilter,
    IssueProvider, MergeRequest, MergeRequestProvider, MrFilter, Provider, Result,
    UpdateIssueInput, User,
};
use tracing::{debug, warn};

use crate::types::{
    ClickUpComment, ClickUpCommentList, ClickUpPriority, ClickUpTask, ClickUpTaskList, ClickUpUser,
    CreateCommentRequest, CreateTaskRequest, UpdateTaskRequest,
};
use crate::DEFAULT_CLICKUP_URL;

/// ClickUp API client.
pub struct ClickUpClient {
    base_url: String,
    list_id: String,
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
            token: token.into(),
            client: reqwest::Client::builder()
                .user_agent("devboy-tools")
                .build()
                .expect("Failed to create HTTP client"),
        }
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

fn map_task_key(task: &ClickUpTask) -> String {
    if let Some(custom_id) = &task.custom_id {
        custom_id.clone()
    } else {
        format!("CU-{}", task.id)
    }
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
        created_at: task.date_created.clone(),
        updated_at: task.date_updated.clone(),
    }
}

fn map_comment(cu_comment: &ClickUpComment) -> Comment {
    Comment {
        id: cu_comment.id.clone(),
        body: cu_comment.comment_text.clone(),
        author: map_user(cu_comment.user.as_ref()),
        created_at: cu_comment.date.clone(),
        updated_at: None,
        position: None,
    }
}

/// Parse task key to extract the raw task ID.
/// - `CU-abc123` -> `abc123`
/// - Anything else (e.g., `DEV-123`) -> used as-is (treated as custom_id or raw ID)
fn parse_task_key(key: &str) -> &str {
    key.strip_prefix("CU-").unwrap_or(key)
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
        let mut url = format!("{}/list/{}/task", self.base_url, self.list_id);
        let mut params = vec![];

        // ClickUp uses include_closed=true to also show closed tasks
        let include_closed = matches!(filter.state.as_deref(), Some("closed") | Some("all"));
        if include_closed {
            params.push("include_closed=true".to_string());
        }

        params.push("subtasks=true".to_string());

        if let Some(assignees) = &filter.assignee {
            // ClickUp filters assignees by user ID, but we receive a username.
            // Pass it as a query param; the API will ignore unknown values gracefully.
            params.push(format!("assignees[]={}", assignees));
        }

        if let Some(tags) = &filter.labels {
            for tag in tags {
                params.push(format!("tags[]={}", tag));
            }
        }

        // Pagination: ClickUp uses page-based (0-indexed, 100 per page)
        let limit = filter.limit.unwrap_or(20);
        let offset = filter.offset.unwrap_or(0);
        let page = offset / 100;
        params.push(format!("page={}", page));

        if let Some(order_by) = &filter.sort_by {
            let cu_order_by = match order_by.as_str() {
                "created_at" | "created" => "created",
                "updated_at" | "updated" => "updated",
                _ => "updated",
            };
            params.push(format!("order_by={}", cu_order_by));
        }

        if let Some(order) = &filter.sort_order {
            let reverse = order == "asc";
            if reverse {
                params.push("reverse=true".to_string());
            }
        }

        if !params.is_empty() {
            url.push_str(&format!("?{}", params.join("&")));
        }

        let response: ClickUpTaskList = self.get(&url).await?;

        let mut issues: Vec<Issue> = response.tasks.iter().map(map_task).collect();

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

        // Apply client-side offset within page and limit
        let page_offset = (offset % 100) as usize;
        if page_offset > 0 && page_offset < issues.len() {
            issues = issues.split_off(page_offset);
        } else if page_offset >= issues.len() {
            issues.clear();
        }

        issues.truncate(limit as usize);

        Ok(issues)
    }

    async fn get_issue(&self, key: &str) -> Result<Issue> {
        let task_id = parse_task_key(key);
        let url = format!("{}/task/{}", self.base_url, task_id);
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
        Ok(map_task(&task))
    }

    async fn update_issue(&self, key: &str, input: UpdateIssueInput) -> Result<Issue> {
        let task_id = parse_task_key(key);
        let url = format!("{}/task/{}", self.base_url, task_id);

        let status = input.state.map(|s| match s.as_str() {
            "closed" => "closed".to_string(),
            "open" | "opened" => "open".to_string(),
            other => other.to_string(),
        });

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
        let task_id = parse_task_key(issue_key);
        let url = format!("{}/task/{}/comment", self.base_url, task_id);
        let response: ClickUpCommentList = self.get(&url).await?;
        Ok(response.comments.iter().map(map_comment).collect())
    }

    async fn add_comment(&self, issue_key: &str, body: &str) -> Result<Comment> {
        let task_id = parse_task_key(issue_key);
        let url = format!("{}/task/{}/comment", self.base_url, task_id);
        let request = CreateCommentRequest {
            comment_text: body.to_string(),
        };

        let cu_comment: ClickUpComment = self.post(&url, &request).await?;
        Ok(map_comment(&cu_comment))
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
    fn test_parse_task_key_cu_prefix() {
        assert_eq!(parse_task_key("CU-abc123"), "abc123");
    }

    #[test]
    fn test_parse_task_key_custom_id() {
        assert_eq!(parse_task_key("DEV-123"), "DEV-123");
    }

    #[test]
    fn test_parse_task_key_raw_id() {
        assert_eq!(parse_task_key("abc123"), "abc123");
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
        assert_eq!(comment.created_at, Some("1705312800000".to_string()));
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
    fn test_provider_name() {
        let client = ClickUpClient::new("12345", "token");
        assert_eq!(IssueProvider::provider_name(&client), "clickup");
        assert_eq!(MergeRequestProvider::provider_name(&client), "clickup");
    }

    #[test]
    fn test_map_task_description_fallback() {
        // When text_content is None, use description
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

            // Only open tasks
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

            // Create 5 tasks
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

            // Request with limit=2, offset=1
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
        async fn test_create_issue() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST)
                    .path("/list/12345/task")
                    .body_includes("\"name\":\"New Task\"");
                then.status(200).json_body(sample_task_json());
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

            assert_eq!(issue.key, "CU-abc123");
        }

        #[tokio::test]
        async fn test_create_issue_with_priority() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST)
                    .path("/list/12345/task")
                    .body_includes("\"priority\":1");
                then.status(200).json_body(sample_task_json());
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
        async fn test_update_issue_state_mapping() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(PUT)
                    .path("/task/abc123")
                    .body_includes("\"status\":\"closed\"");
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
        }

        #[tokio::test]
        async fn test_add_comment() {
            let server = MockServer::start();

            server.mock(|when, then| {
                when.method(POST)
                    .path("/task/abc123/comment")
                    .body_includes("\"comment_text\":\"My comment\"");
                then.status(200).json_body(serde_json::json!({
                    "id": "42",
                    "comment_text": "My comment",
                    "user": {"id": 1, "username": "me"},
                    "date": "1705312800000"
                }));
            });

            let client = create_test_client(&server);
            let comment = IssueProvider::add_comment(&client, "CU-abc123", "My comment")
                .await
                .unwrap();

            assert_eq!(comment.body, "My comment");
            assert_eq!(comment.id, "42");
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
