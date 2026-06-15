//! YouGile API client scaffold.

use std::collections::HashMap;

use async_trait::async_trait;
use devboy_core::{
    Error, Issue, IssueFilter, IssueProvider, MergeRequestProvider, Pagination, PipelineProvider,
    Provider, ProviderResult, Result, User,
};
use reqwest::{Response, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use tracing::warn;

use crate::DEFAULT_YOUGILE_URL;
use crate::types::{YouGileColumn, YouGileListResponse, YouGileTask, YouGileUser};

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

    async fn get_json<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T> {
        let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
        let response = self
            .client
            .get(&url)
            .bearer_auth(self.token.expose_secret())
            .query(query)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        parse_json_response(response).await
    }

    async fn list_board_columns(&self) -> Result<Vec<YouGileColumn>> {
        let mut offset = 0_u32;
        let mut columns = Vec::new();

        loop {
            let page: YouGileListResponse<YouGileColumn> = self
                .get_json(
                    "/columns",
                    &[
                        ("boardId", self.board_id.clone()),
                        ("limit", "1000".to_string()),
                        ("offset", offset.to_string()),
                    ],
                )
                .await?;
            columns.extend(
                page.content
                    .into_iter()
                    .filter(|column| !column.deleted && column.board_id == self.board_id),
            );
            if !page.paging.next {
                break;
            }
            offset = offset.saturating_add(page.paging.limit);
        }

        Ok(columns)
    }

    async fn list_tasks_for_column(
        &self,
        column_id: &str,
        filter: &IssueFilter,
    ) -> Result<Vec<YouGileTask>> {
        let mut offset = 0_u32;
        let mut tasks = Vec::new();

        loop {
            let mut query = vec![
                ("columnId", column_id.to_string()),
                ("limit", "1000".to_string()),
                ("offset", offset.to_string()),
            ];

            if let Some(search) = &filter.search {
                query.push(("title", search.clone()));
            }
            if let Some(assignee) = &filter.assignee {
                query.push(("assignedTo", assignee.clone()));
            }
            if matches!(filter.state.as_deref(), Some("all")) {
                query.push(("includeDeleted", "true".to_string()));
            }

            let page: YouGileListResponse<YouGileTask> =
                self.get_json("/task-list", &query).await?;
            tasks.extend(page.content);
            if !page.paging.next {
                break;
            }
            offset = offset.saturating_add(page.paging.limit);
        }

        Ok(tasks)
    }

    async fn list_board_tasks(&self, filter: &IssueFilter) -> Result<Vec<YouGileTask>> {
        let columns = self.list_board_columns().await?;
        let column_titles: HashMap<String, String> = columns
            .iter()
            .map(|column| (column.id.clone(), column.title.clone()))
            .collect();

        let mut tasks = Vec::new();
        for column in &columns {
            tasks.extend(self.list_tasks_for_column(&column.id, filter).await?);
        }

        tasks.retain(|task| {
            if task.deleted && !matches!(filter.state.as_deref(), Some("all")) {
                return false;
            }

            match filter.state.as_deref() {
                Some("open") | Some("opened") => !is_closed_task(task),
                Some("closed") => is_closed_task(task),
                _ => true,
            }
        });

        if let Some(state_category) = filter.state_category.as_deref() {
            tasks.retain(|task| matches_state_category(task, state_category));
        }

        tasks.sort_by_key(|task| std::cmp::Reverse(task.timestamp));

        if let Some(sort_by) = filter.sort_by.as_deref() {
            match sort_by {
                "created_at" | "created" => {
                    if matches!(filter.sort_order.as_deref(), Some("asc")) {
                        tasks.sort_by_key(|task| task.timestamp);
                    } else {
                        tasks.sort_by_key(|task| std::cmp::Reverse(task.timestamp));
                    }
                }
                unsupported => {
                    warn!(
                        sort_by = unsupported,
                        "YouGile API does not expose native sorting for this field; keeping timestamp order"
                    );
                }
            }
        }

        // Make sure column mapping covers every task we return.
        tasks.retain(|task| {
            task.column_id
                .as_ref()
                .is_none_or(|column_id| column_titles.contains_key(column_id))
        });

        Ok(tasks)
    }

    async fn resolve_task_id(&self, key: &str) -> Result<String> {
        if let Some(raw_id) = key.strip_prefix("yougile#") {
            return Ok(raw_id.to_string());
        }
        if looks_like_uuid(key) {
            return Ok(key.to_string());
        }

        let tasks = self.list_board_tasks(&IssueFilter::default()).await?;
        tasks.into_iter()
            .find(|task| task_matches_key(task, key))
            .map(|task| task.id)
            .ok_or_else(|| Error::NotFound(format!("YouGile task '{key}' not found")))
    }

    fn map_issue(&self, task: &YouGileTask, columns: &HashMap<String, String>) -> Issue {
        let status = task
            .column_id
            .as_ref()
            .and_then(|column_id| columns.get(column_id))
            .cloned();
        let state = if is_closed_task(task) { "closed" } else { "open" }.to_string();
        let status_category = if task.deleted || task.archived {
            Some("cancelled".to_string())
        } else if task.completed {
            Some("done".to_string())
        } else {
            None
        };

        Issue {
            key: task_display_key(task),
            title: task.title.clone(),
            description: task.description.clone(),
            state,
            status,
            status_category,
            source: "yougile".to_string(),
            priority: None,
            labels: Vec::new(),
            author: task.created_by.as_ref().map(|id| minimal_user(id)),
            assignees: task.assigned_ids.iter().map(|id| minimal_user(id)).collect(),
            url: None,
            created_at: Some(epoch_millis_to_rfc3339(task.timestamp)),
            updated_at: None,
            attachments_count: None,
            parent: None,
            subtasks: Vec::new(),
            custom_fields: HashMap::new(),
        }
    }
}

#[async_trait]
impl IssueProvider for YouGileClient {
    async fn get_issues(&self, filter: IssueFilter) -> Result<ProviderResult<Issue>> {
        let columns = self.list_board_columns().await?;
        let column_titles: HashMap<String, String> = columns
            .iter()
            .map(|column| (column.id.clone(), column.title.clone()))
            .collect();

        let all_tasks = self.list_board_tasks(&filter).await?;
        let total = all_tasks.len() as u32;
        let offset = filter.offset.unwrap_or(0) as usize;
        let limit = filter.limit.unwrap_or(20) as usize;

        let page_tasks = if offset >= all_tasks.len() {
            Vec::new()
        } else {
            all_tasks
                .into_iter()
                .skip(offset)
                .take(limit)
                .collect::<Vec<_>>()
        };
        let has_more = (offset + page_tasks.len()) < total as usize;
        let items = page_tasks
            .iter()
            .map(|task| self.map_issue(task, &column_titles))
            .collect::<Vec<_>>();

        Ok(ProviderResult::new(items).with_pagination(Pagination {
            offset: offset as u32,
            limit: limit as u32,
            total: Some(total),
            has_more,
            next_cursor: None,
        }))
    }

    async fn get_issue(&self, key: &str) -> Result<Issue> {
        let task_id = self.resolve_task_id(key).await?;
        let task: YouGileTask = self
            .get_json(&format!("/tasks/{task_id}"), &[])
            .await?;
        let columns = self.list_board_columns().await?;
        let column_titles: HashMap<String, String> = columns
            .iter()
            .map(|column| (column.id.clone(), column.title.clone()))
            .collect();
        Ok(self.map_issue(&task, &column_titles))
    }

    async fn create_issue(&self, _input: devboy_core::CreateIssueInput) -> Result<Issue> {
        Err(Error::ProviderUnsupported {
            provider: IssueProvider::provider_name(self).to_string(),
            operation: "create_issue".to_string(),
        })
    }

    async fn update_issue(
        &self,
        _key: &str,
        _input: devboy_core::UpdateIssueInput,
    ) -> Result<Issue> {
        Err(Error::ProviderUnsupported {
            provider: IssueProvider::provider_name(self).to_string(),
            operation: "update_issue".to_string(),
        })
    }

    async fn get_comments(&self, _issue_key: &str) -> Result<ProviderResult<devboy_core::Comment>> {
        Err(Error::ProviderUnsupported {
            provider: IssueProvider::provider_name(self).to_string(),
            operation: "get_comments".to_string(),
        })
    }

    async fn add_comment(&self, _issue_key: &str, _body: &str) -> Result<devboy_core::Comment> {
        Err(Error::ProviderUnsupported {
            provider: IssueProvider::provider_name(self).to_string(),
            operation: "add_comment".to_string(),
        })
    }

    fn provider_name(&self) -> &'static str {
        "yougile"
    }
}

#[async_trait]
impl MergeRequestProvider for YouGileClient {
    fn provider_name(&self) -> &'static str {
        "yougile"
    }
}

#[async_trait]
impl PipelineProvider for YouGileClient {
    fn provider_name(&self) -> &'static str {
        "yougile"
    }
}

#[async_trait]
impl Provider for YouGileClient {
    async fn get_current_user(&self) -> Result<User> {
        let user: YouGileUser = self.get_json("/users/me", &[]).await?;
        Ok(User {
            id: user.id,
            username: user.email.clone(),
            name: Some(user.real_name),
            email: Some(user.email),
            avatar_url: None,
        })
    }
}

fn map_reqwest_error(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::Timeout
    } else if error.is_connect() {
        Error::Network(error.to_string())
    } else {
        Error::Http(error.to_string())
    }
}

async fn parse_json_response<T: DeserializeOwned>(response: Response) -> Result<T> {
    let status = response.status();
    let body = response.text().await.map_err(map_reqwest_error)?;
    if !status.is_success() {
        let message = if body.trim().is_empty() {
            status
                .canonical_reason()
                .unwrap_or("request failed")
                .to_string()
        } else {
            body
        };
        return Err(match status {
            StatusCode::UNAUTHORIZED => Error::Unauthorized(message),
            StatusCode::FORBIDDEN => Error::Forbidden(message),
            StatusCode::NOT_FOUND => Error::NotFound(message),
            StatusCode::TOO_MANY_REQUESTS => Error::RateLimited { retry_after: None },
            s if s.is_server_error() => Error::ServerError {
                status: s.as_u16(),
                message,
            },
            _ => Error::Api {
                status: status.as_u16(),
                message,
            },
        });
    }

    serde_json::from_str(&body).map_err(Error::from)
}

fn task_display_key(task: &YouGileTask) -> String {
    task.id_task_project
        .clone()
        .or_else(|| task.id_task_common.clone())
        .unwrap_or_else(|| format!("yougile#{}", task.id))
}

fn task_matches_key(task: &YouGileTask, key: &str) -> bool {
    task.id == key
        || task_display_key(task) == key
        || task.id_task_common.as_deref() == Some(key)
        || task.id_task_project.as_deref() == Some(key)
        || format!("yougile#{}", task.id) == key
}

fn is_closed_task(task: &YouGileTask) -> bool {
    task.completed || task.archived || task.deleted
}

fn matches_state_category(task: &YouGileTask, state_category: &str) -> bool {
    match state_category {
        "done" => task.completed,
        "cancelled" => task.deleted || task.archived,
        "todo" | "backlog" | "in_progress" => !is_closed_task(task),
        _ => true,
    }
}

fn minimal_user(id: &str) -> User {
    User {
        id: id.to_string(),
        username: id.to_string(),
        name: None,
        email: None,
        avatar_url: None,
    }
}

fn epoch_millis_to_rfc3339(timestamp: u64) -> String {
    let secs = (timestamp / 1000) as i64;
    let millis = (timestamp % 1000) as u32;
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(secs, millis * 1_000_000)
        .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH);
    dt.to_rfc3339()
}

fn looks_like_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(idx, b)| match idx {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::GET;
    use httpmock::MockServer;

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

    #[tokio::test]
    async fn get_issue_fetches_task_by_uuid() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api-v2/tasks/task-1");
                then.status(200).json_body_obj(&serde_json::json!({
                    "id": "task-1",
                    "title": "Implement provider",
                    "timestamp": 1710000000000_u64,
                    "columnId": "col-1",
                    "description": "read path",
                    "completed": false,
                    "archived": false,
                    "deleted": false,
                    "assigned": ["user-1"],
                    "createdBy": "user-2",
                    "idTaskProject": "DEV-484"
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api-v2/columns")
                    .query_param("boardId", "board-1")
                    .query_param("limit", "1000")
                    .query_param("offset", "0");
                then.status(200).json_body_obj(&serde_json::json!({
                    "paging": { "limit": 1000, "offset": 0, "next": false },
                    "content": [
                        { "id": "col-1", "title": "To Do", "boardId": "board-1" }
                    ]
                }));
            })
            .await;

        let client = YouGileClient::with_base_url(
            format!("{}/api-v2", server.base_url()),
            "board-1",
            SecretString::from("token".to_owned()),
        );
        let issue = client.get_issue("yougile#task-1").await.unwrap();
        assert_eq!(issue.key, "DEV-484");
        assert_eq!(issue.status.as_deref(), Some("To Do"));
        assert_eq!(issue.state, "open");
    }

    #[tokio::test]
    async fn get_issues_lists_tasks_across_board_columns() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api-v2/columns")
                    .query_param("boardId", "board-1")
                    .query_param("limit", "1000")
                    .query_param("offset", "0");
                then.status(200).json_body_obj(&serde_json::json!({
                    "paging": { "limit": 1000, "offset": 0, "next": false },
                    "content": [
                        { "id": "col-1", "title": "To Do", "boardId": "board-1" },
                        { "id": "col-2", "title": "Done", "boardId": "board-1" }
                    ]
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api-v2/task-list")
                    .query_param("columnId", "col-1")
                    .query_param("limit", "1000")
                    .query_param("offset", "0");
                then.status(200).json_body_obj(&serde_json::json!({
                    "paging": { "limit": 1000, "offset": 0, "next": false },
                    "content": [
                        {
                            "id": "task-1",
                            "title": "Open task",
                            "timestamp": 1710000000000_u64,
                            "columnId": "col-1",
                            "completed": false,
                            "archived": false,
                            "deleted": false,
                            "idTaskProject": "DEV-1"
                        }
                    ]
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api-v2/task-list")
                    .query_param("columnId", "col-2")
                    .query_param("limit", "1000")
                    .query_param("offset", "0");
                then.status(200).json_body_obj(&serde_json::json!({
                    "paging": { "limit": 1000, "offset": 0, "next": false },
                    "content": [
                        {
                            "id": "task-2",
                            "title": "Done task",
                            "timestamp": 1710000001000_u64,
                            "columnId": "col-2",
                            "completed": true,
                            "archived": false,
                            "deleted": false,
                            "idTaskProject": "DEV-2"
                        }
                    ]
                }));
            })
            .await;

        let client = YouGileClient::with_base_url(
            format!("{}/api-v2", server.base_url()),
            "board-1",
            SecretString::from("token".to_owned()),
        );
        let issues = client.get_issues(IssueFilter::default()).await.unwrap();
        assert_eq!(issues.items.len(), 2);
        assert_eq!(issues.items[0].key, "DEV-2");
        assert_eq!(issues.items[1].key, "DEV-1");
        assert_eq!(issues.pagination.unwrap().total, Some(2));
    }
}
