//! YouGile API client.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use devboy_core::{
    Comment, CreateIssueInput, CustomFieldValue, Error, Issue, IssueFilter, IssueProvider,
    MergeRequestProvider, Pagination, PipelineProvider, Provider, ProviderResult, Result, SortInfo,
    SortOrder, UpdateIssueInput, User,
};
use reqwest::{Response, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::time::{Instant, sleep_until};
use tracing::warn;

use crate::DEFAULT_YOUGILE_URL;
use crate::types::{
    YouGileChatMessage, YouGileColumn, YouGileListResponse, YouGilePaging, YouGileTask, YouGileUser,
};

/// YouGile client used by the workspace provider layer.
#[derive(Clone)]
pub struct YouGileClient {
    base_url: String,
    board_id: String,
    token: SecretString,
    client: reqwest::Client,
    rate_limiter: Arc<YouGileRateLimiter>,
    user_cache: Arc<Mutex<HashMap<String, User>>>,
}

const YOUGILE_REQUEST_INTERVAL: Duration = Duration::from_millis(1200);

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
            rate_limiter: Arc::new(YouGileRateLimiter::new(YOUGILE_REQUEST_INTERVAL)),
            user_cache: Arc::new(Mutex::new(HashMap::new())),
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

    /// API token accessor for internal use.
    pub fn token(&self) -> &SecretString {
        &self.token
    }

    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
        self.rate_limiter.acquire().await;
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

    async fn post_json<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
        self.rate_limiter.acquire().await;
        let response = self
            .client
            .post(&url)
            .bearer_auth(self.token.expose_secret())
            .json(body)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        parse_json_response(response).await
    }

    async fn put_json<T: DeserializeOwned, B: Serialize>(&self, path: &str, body: &B) -> Result<T> {
        let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
        self.rate_limiter.acquire().await;
        let response = self
            .client
            .put(&url)
            .bearer_auth(self.token.expose_secret())
            .json(body)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        parse_json_response(response).await
    }

    async fn list_board_columns(&self) -> Result<Vec<YouGileColumn>> {
        let mut offset = 0_u32;
        let mut columns = Vec::new();

        for page_no in 0..=MAX_PAGES {
            if page_no == MAX_PAGES {
                return Err(exhausted_pages(MAX_PAGES, "/columns"));
            }
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
            offset = advance_offset(offset, &page.paging, "/columns")?;
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

        for page_no in 0..=MAX_PAGES {
            if page_no == MAX_PAGES {
                return Err(exhausted_pages(MAX_PAGES, "/task-list"));
            }
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
            offset = advance_offset(offset, &page.paging, "/task-list")?;
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
            tasks.retain(|task| {
                matches_state_category(
                    task,
                    state_category,
                    task.column_id
                        .as_ref()
                        .and_then(|column_id| column_titles.get(column_id).map(String::as_str)),
                )
            });
        }

        tasks.sort_by_key(|task| std::cmp::Reverse(task.timestamp));

        // Validated up front in `get_issues`, so by here the field is known
        // to be one of the two timestamp aliases.
        if matches!(filter.sort_order.as_deref(), Some("asc")) {
            tasks.sort_by_key(|task| task.timestamp);
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
        tasks
            .into_iter()
            .find(|task| task_matches_key(task, key))
            .map(|task| task.id)
            .ok_or_else(|| Error::NotFound(format!("YouGile task '{key}' not found")))
    }

    async fn resolve_status_update(&self, status: &str) -> Result<serde_json::Map<String, Value>> {
        let columns = self.list_board_columns().await?;
        let column = columns
            .into_iter()
            .find(|column| column.title.eq_ignore_ascii_case(status))
            .ok_or_else(|| {
                Error::InvalidData(format!(
                    "YouGile status '{status}' does not match any column title on board {}",
                    self.board_id
                ))
            })?;

        let mut body = serde_json::Map::new();
        body.insert("columnId".to_string(), json!(column.id));

        match map_status_category_from_name(&column.title).as_str() {
            "done" => {
                body.insert("completed".to_string(), json!(true));
                body.insert("archived".to_string(), json!(false));
                body.insert("deleted".to_string(), json!(false));
            }
            "cancelled" => {
                body.insert("completed".to_string(), json!(false));
                body.insert("archived".to_string(), json!(true));
                body.insert("deleted".to_string(), json!(false));
            }
            _ => {
                body.insert("completed".to_string(), json!(false));
                body.insert("archived".to_string(), json!(false));
                body.insert("deleted".to_string(), json!(false));
            }
        }

        Ok(body)
    }

    async fn load_board_hierarchy(&self) -> Result<Vec<YouGileTask>> {
        self.list_board_tasks(&IssueFilter {
            state: Some("all".to_string()),
            ..IssueFilter::default()
        })
        .await
    }

    async fn rewrite_parent_subtasks(&self, parent_id: &str, subtasks: Vec<String>) -> Result<()> {
        let _: Value = self
            .put_json(
                &format!("/tasks/{parent_id}"),
                &json!({
                    "subtasks": subtasks
                }),
            )
            .await?;
        Ok(())
    }

    async fn get_issue_with_relations(
        &self,
        task_id: &str,
        relation_tasks: &[YouGileTask],
    ) -> Result<Issue> {
        let task: YouGileTask = self.get_json(&format!("/tasks/{task_id}"), &[]).await?;
        let columns = self.list_board_columns().await?;
        let column_titles: HashMap<String, String> = columns
            .iter()
            .map(|column| (column.id.clone(), column.title.clone()))
            .collect();
        let task_by_id = build_task_index(relation_tasks);
        let parent_by_child = build_parent_index(relation_tasks);
        Ok(self
            .map_issue(&task, &column_titles, &task_by_id, &parent_by_child)
            .await)
    }

    async fn sync_parent_link(
        &self,
        child_id: &str,
        desired_parent_key: Option<&str>,
    ) -> Result<Vec<YouGileTask>> {
        let mut tasks = self.load_board_hierarchy().await?;
        let task_by_id = build_task_index(&tasks);
        let parent_by_child = build_parent_index(&tasks);
        let current_parent = parent_by_child.get(child_id).map(|task| task.id.clone());

        let desired_parent = match desired_parent_key {
            Some(parent_key)
                if !parent_key.trim().is_empty() && !parent_key.eq_ignore_ascii_case("none") =>
            {
                Some(
                    resolve_task_id_from_tasks(&tasks, parent_key)
                        .or_else(|| {
                            if looks_like_uuid(parent_key) {
                                Some(parent_key.to_string())
                            } else {
                                None
                            }
                        })
                        .ok_or_else(|| {
                            Error::NotFound(format!(
                                "YouGile parent task '{}' not found on board {}",
                                parent_key, self.board_id
                            ))
                        })?,
                )
            }
            _ => None,
        };

        if current_parent == desired_parent {
            apply_parent_link_to_tasks(&mut tasks, child_id, desired_parent.as_deref());
            return Ok(tasks);
        }

        if let Some(old_parent_id) = current_parent {
            let old_parent = task_by_id.get(&old_parent_id).ok_or_else(|| {
                Error::NotFound(format!(
                    "YouGile parent task '{}' not found while detaching child '{}'",
                    old_parent_id, child_id
                ))
            })?;
            let new_subtasks: Vec<String> = old_parent
                .subtask_ids
                .iter()
                .filter(|id| id.as_str() != child_id)
                .cloned()
                .collect();
            self.rewrite_parent_subtasks(&old_parent_id, new_subtasks)
                .await?;
        }

        if let Some(new_parent_id) = desired_parent.as_deref() {
            let new_parent = task_by_id.get(new_parent_id).ok_or_else(|| {
                Error::NotFound(format!(
                    "YouGile parent task '{}' not found while attaching child '{}'",
                    new_parent_id, child_id
                ))
            })?;
            let mut subtasks = new_parent.subtask_ids.clone();
            if !subtasks.iter().any(|id| id == child_id) {
                subtasks.push(child_id.to_string());
            }
            self.rewrite_parent_subtasks(new_parent_id, subtasks)
                .await?;
        }

        apply_parent_link_to_tasks(&mut tasks, child_id, desired_parent.as_deref());
        Ok(tasks)
    }

    async fn default_column_id(&self) -> Result<Option<String>> {
        let columns = self.list_board_columns().await?;
        Ok(columns.into_iter().next().map(|column| column.id))
    }

    async fn resolve_user(&self, id: &str) -> User {
        if let Some(cached) = self.user_cache.lock().await.get(id).cloned() {
            return cached;
        }

        let user = match self
            .get_json::<YouGileUser>(&format!("/users/{id}"), &[])
            .await
        {
            Ok(user) => map_yougile_user(user),
            Err(error) => {
                warn!(
                    user_id = id,
                    ?error,
                    "Failed to hydrate YouGile user; falling back to id-only user"
                );
                minimal_user(id)
            }
        };

        self.user_cache
            .lock()
            .await
            .insert(id.to_string(), user.clone());
        user
    }

    async fn resolve_users_by_id(&self, ids: &[String]) -> HashMap<String, User> {
        let mut users = HashMap::new();
        for id in ids {
            users.insert(id.clone(), self.resolve_user(id).await);
        }
        users
    }

    async fn map_issue(
        &self,
        task: &YouGileTask,
        columns: &HashMap<String, String>,
        task_by_id: &HashMap<String, &YouGileTask>,
        parent_by_child: &HashMap<String, &YouGileTask>,
    ) -> Issue {
        let status = task
            .column_id
            .as_ref()
            .and_then(|column_id| columns.get(column_id))
            .cloned();
        let state = if is_closed_task(task) {
            "closed"
        } else {
            "open"
        }
        .to_string();
        let status_category = Some(map_status_category(task, status.as_deref()));
        let mut user_ids = task.assigned_ids.clone();
        if let Some(author_id) = &task.created_by {
            user_ids.push(author_id.clone());
        }
        let users = self.resolve_users_by_id(&user_ids).await;
        let updated_at = Some(epoch_millis_to_rfc3339(task.timestamp));

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
            author: task
                .created_by
                .as_ref()
                .and_then(|id| users.get(id).cloned()),
            assignees: task
                .assigned_ids
                .iter()
                .filter_map(|id| users.get(id).cloned())
                .collect(),
            url: None,
            created_at: updated_at.clone(),
            updated_at,
            attachments_count: None,
            parent: parent_by_child
                .get(&task.id)
                .map(|parent| task_display_key(parent)),
            subtasks: task
                .subtask_ids
                .iter()
                .filter_map(|id| task_by_id.get(id))
                .map(|subtask| map_related_issue(subtask, columns))
                .collect(),
            custom_fields: map_custom_fields(task),
        }
    }

    async fn map_comment(&self, message: &YouGileChatMessage) -> Comment {
        Comment {
            id: message.id.to_string(),
            body: message.text.clone(),
            author: Some(self.resolve_user(&message.from_user_id).await),
            created_at: Some(epoch_millis_to_rfc3339(message.id)),
            updated_at: Some(epoch_millis_to_rfc3339(message.edit_timestamp)),
            position: None,
        }
    }
}

fn validate_generic_create_fields(input: &CreateIssueInput) -> Result<()> {
    if !input.labels.is_empty() {
        return Err(Error::InvalidData(
            "YouGile create_issue does not support generic labels. Use provider-specific sticker ids via custom_fields instead.".to_string(),
        ));
    }
    if input.priority.is_some() {
        return Err(Error::InvalidData(
            "YouGile create_issue does not support generic priority. Use a provider-specific priority sticker via custom_fields instead.".to_string(),
        ));
    }
    Ok(())
}

fn validate_generic_update_fields(input: &UpdateIssueInput) -> Result<()> {
    if input.labels.is_some() {
        return Err(Error::InvalidData(
            "YouGile update_issue does not support generic labels. Use provider-specific sticker ids via custom_fields instead.".to_string(),
        ));
    }
    if input.priority.is_some() {
        return Err(Error::InvalidData(
            "YouGile update_issue does not support generic priority. Use a provider-specific priority sticker via custom_fields instead.".to_string(),
        ));
    }
    Ok(())
}

#[async_trait]
impl IssueProvider for YouGileClient {
    async fn get_issues(&self, filter: IssueFilter) -> Result<ProviderResult<Issue>> {
        validate_sort_by(filter.sort_by.as_deref())?;
        validate_assignee_filter(filter.assignee.as_deref())?;
        let columns = self.list_board_columns().await?;
        let column_titles: HashMap<String, String> = columns
            .iter()
            .map(|column| (column.id.clone(), column.title.clone()))
            .collect();

        let all_tasks = self.list_board_tasks(&filter).await?;
        let task_by_id = build_task_index(&all_tasks);
        let parent_by_child = build_parent_index(&all_tasks);
        let total = all_tasks.len() as u32;
        let offset = filter.offset.unwrap_or(0) as usize;
        let limit = filter.limit.unwrap_or(20) as usize;

        let page_tasks: Vec<&YouGileTask> = if offset >= all_tasks.len() {
            Vec::new()
        } else {
            all_tasks.iter().skip(offset).take(limit).collect()
        };
        let has_more = (offset + page_tasks.len()) < total as usize;
        let mut items = Vec::with_capacity(page_tasks.len());
        for task in page_tasks {
            items.push(
                self.map_issue(task, &column_titles, &task_by_id, &parent_by_child)
                    .await,
            );
        }

        let mut result = ProviderResult::new(items).with_pagination(Pagination {
            offset: offset as u32,
            limit: limit as u32,
            total: Some(total),
            has_more,
            next_cursor: None,
        });
        result.sort_info = Some(SortInfo {
            sort_by: Some(
                filter
                    .sort_by
                    .as_deref()
                    .unwrap_or("created_at")
                    .to_string(),
            ),
            sort_order: if matches!(filter.sort_order.as_deref(), Some("asc")) {
                SortOrder::Asc
            } else {
                SortOrder::Desc
            },
            available_sorts: vec!["created_at".to_string(), "updated_at".to_string()],
        });
        Ok(result)
    }

    async fn get_issue(&self, key: &str) -> Result<Issue> {
        let task_id = self.resolve_task_id(key).await?;
        let task: YouGileTask = self.get_json(&format!("/tasks/{task_id}"), &[]).await?;
        let columns = self.list_board_columns().await?;
        let column_titles: HashMap<String, String> = columns
            .iter()
            .map(|column| (column.id.clone(), column.title.clone()))
            .collect();
        let relation_tasks = self
            .list_board_tasks(&IssueFilter {
                state: Some("all".to_string()),
                ..IssueFilter::default()
            })
            .await?;
        let task_by_id = build_task_index(&relation_tasks);
        let parent_by_child = build_parent_index(&relation_tasks);
        Ok(self
            .map_issue(&task, &column_titles, &task_by_id, &parent_by_child)
            .await)
    }

    async fn create_issue(&self, input: CreateIssueInput) -> Result<Issue> {
        validate_generic_create_fields(&input)?;
        if input.issue_type.is_some() {
            warn!("YouGile create_issue ignores issue_type");
        }
        let parent_key = input.parent.clone();

        let column_id = self.default_column_id().await?;

        let mut body = serde_json::Map::new();
        body.insert("title".to_string(), json!(input.title));
        if let Some(description) = input.description {
            body.insert("description".to_string(), json!(description));
        }
        if let Some(column_id) = column_id {
            body.insert("columnId".to_string(), json!(column_id));
        }
        if !input.assignees.is_empty() {
            body.insert("assigned".to_string(), json!(input.assignees));
        }
        if let Some(stickers) = custom_fields_to_stickers(input.custom_fields)? {
            body.insert("stickers".to_string(), stickers);
        }

        let created: Value = self.post_json("/tasks", &body).await?;
        let task_id = created.get("id").and_then(|v| v.as_str()).ok_or_else(|| {
            Error::InvalidData("YouGile create_issue response missing id".to_string())
        })?;
        if let Some(parent_key) = parent_key.as_deref() {
            let relation_tasks = self.sync_parent_link(task_id, Some(parent_key)).await?;
            return self
                .get_issue_with_relations(task_id, &relation_tasks)
                .await;
        }
        self.get_issue(&format!("yougile#{task_id}")).await
    }

    async fn update_issue(&self, key: &str, input: UpdateIssueInput) -> Result<Issue> {
        validate_generic_update_fields(&input)?;
        let task_id = self.resolve_task_id(key).await?;
        let parent_id = input.parent_id.clone();
        let mut body = serde_json::Map::new();

        if let Some(title) = input.title {
            body.insert("title".to_string(), json!(title));
        }
        if let Some(description) = input.description {
            body.insert("description".to_string(), json!(description));
        }
        if let Some(assignees) = input.assignees {
            body.insert("assigned".to_string(), json!(assignees));
        }
        if let Some(status) = input.status.as_deref() {
            body.extend(self.resolve_status_update(status).await?);
        } else if let Some(state) = input.state.as_deref() {
            match state {
                "open" | "opened" => {
                    body.insert("completed".to_string(), json!(false));
                    body.insert("archived".to_string(), json!(false));
                    body.insert("deleted".to_string(), json!(false));
                }
                "closed" => {
                    body.insert("completed".to_string(), json!(true));
                    body.insert("archived".to_string(), json!(false));
                    body.insert("deleted".to_string(), json!(false));
                }
                _ => {}
            }
        }
        if let Some(stickers) = custom_fields_to_stickers(input.custom_fields)? {
            body.insert("stickers".to_string(), stickers);
        }

        if !body.is_empty() {
            let _: Value = self.put_json(&format!("/tasks/{task_id}"), &body).await?;
        }
        if let Some(parent_id) = parent_id.as_deref() {
            let relation_tasks = self.sync_parent_link(&task_id, Some(parent_id)).await?;
            return self
                .get_issue_with_relations(&task_id, &relation_tasks)
                .await;
        }
        self.get_issue(&format!("yougile#{task_id}")).await
    }

    async fn get_comments(&self, issue_key: &str) -> Result<ProviderResult<Comment>> {
        let task_id = self.resolve_task_id(issue_key).await?;
        let messages: YouGileListResponse<YouGileChatMessage> = self
            .get_json(
                &format!("/chats/{task_id}/messages"),
                &[
                    ("limit", "1000".to_string()),
                    ("offset", "0".to_string()),
                    ("includeSystem", "false".to_string()),
                ],
            )
            .await?;
        let mut comments = Vec::new();
        for message in messages
            .content
            .into_iter()
            .filter(|message| !message.deleted)
        {
            comments.push(self.map_comment(&message).await);
        }

        Ok(ProviderResult::new(comments).with_pagination(Pagination {
            offset: messages.paging.offset,
            limit: messages.paging.limit,
            total: None,
            has_more: messages.paging.next,
            next_cursor: None,
        }))
    }

    async fn add_comment(&self, issue_key: &str, body: &str) -> Result<Comment> {
        let task_id = self.resolve_task_id(issue_key).await?;
        let response: Value = self
            .post_json(
                &format!("/chats/{task_id}/messages"),
                &json!({
                    "text": body,
                    "textHtml": format!("<p>{}</p>", escape_html(body)),
                    "label": ""
                }),
            )
            .await?;
        let message_id = response.get("id").and_then(|v| v.as_u64()).ok_or_else(|| {
            Error::InvalidData("YouGile add_comment response missing id".to_string())
        })?;
        let messages: YouGileListResponse<YouGileChatMessage> = self
            .get_json(
                &format!("/chats/{task_id}/messages"),
                &[
                    ("limit", "1000".to_string()),
                    ("offset", "0".to_string()),
                    ("includeSystem", "false".to_string()),
                ],
            )
            .await?;
        let message = messages
            .content
            .into_iter()
            .find(|message| message.id == message_id)
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "YouGile comment '{}' not found after creation",
                    message_id
                ))
            })?;
        Ok(self.map_comment(&message).await)
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
        Ok(map_yougile_user(user))
    }
}

#[derive(Debug)]
struct YouGileRateLimiter {
    state: Mutex<YouGileRateLimiterState>,
    interval: Duration,
}

#[derive(Debug)]
struct YouGileRateLimiterState {
    ready_at: Instant,
}

impl YouGileRateLimiter {
    fn new(interval: Duration) -> Self {
        Self {
            state: Mutex::new(YouGileRateLimiterState {
                ready_at: Instant::now(),
            }),
            interval,
        }
    }

    async fn acquire(&self) {
        let wait_until = {
            let mut state = self.state.lock().await;
            let now = Instant::now();
            let wait_until = state.ready_at.max(now);
            state.ready_at = wait_until + self.interval;
            wait_until
        };

        if wait_until > Instant::now() {
            sleep_until(wait_until).await;
        }
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
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
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
            StatusCode::TOO_MANY_REQUESTS => Error::RateLimited { retry_after },
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

fn resolve_task_id_from_tasks(tasks: &[YouGileTask], key: &str) -> Option<String> {
    tasks
        .iter()
        .find(|task| task_matches_key(task, key))
        .map(|task| task.id.clone())
}

fn is_closed_task(task: &YouGileTask) -> bool {
    task.completed || task.archived || task.deleted
}

/// Advances the paging cursor, refusing to spin forever.
///
/// The loops that call this keep going while the server says `next: true`,
/// stepping by the page size the server reports back. A `limit` of 0 would
/// leave the offset unchanged and turn that into an infinite request loop —
/// a hang is far worse than a surfaced error, so bail out instead.
fn advance_offset(offset: u32, paging: &YouGilePaging, endpoint: &str) -> Result<u32> {
    if paging.limit == 0 {
        return Err(Error::InvalidData(format!(
            "YouGile {endpoint} reported more pages but a page size of 0; refusing to loop forever"
        )));
    }
    // `saturating_add` would pin at u32::MAX and keep re-requesting the same
    // offset forever, so treat an offset that cannot advance as a protocol
    // error rather than silently spinning.
    offset.checked_add(paging.limit).ok_or_else(|| {
        Error::InvalidData(format!(
            "YouGile {endpoint} paged past the maximum offset ({offset} + {}); \
             refusing to loop forever",
            paging.limit
        ))
    })
}

/// Upper bound on pages fetched from one endpoint in a single call.
///
/// A server that always answers `next: true` would otherwise keep the loop
/// running indefinitely even while the offset advances normally. At the 1000
/// items per page this client requests, the cap is far above any real board.
const MAX_PAGES: usize = 1_000;

fn exhausted_pages(pages: usize, endpoint: &str) -> Error {
    Error::InvalidData(format!(
        "YouGile {endpoint} still reported more pages after {pages} requests; \
         refusing to loop forever"
    ))
}

/// Maps the cross-provider `sort_by` value onto the single timestamp YouGile
/// exposes on a task.
///
/// A YouGile task carries exactly one `timestamp`, which this provider
/// surfaces as both `created_at` and `updated_at`. Both sort fields are
/// therefore accepted and resolve to the same ordering; anything else is
/// refused by name rather than silently ignored, so a caller never receives
/// arbitrarily-ordered results believing the sort was applied.
fn validate_sort_by(sort_by: Option<&str>) -> Result<()> {
    match sort_by.map(str::trim) {
        None | Some("") | Some("created_at") | Some("created") | Some("updated_at")
        | Some("updated") => Ok(()),
        Some(other) => Err(Error::InvalidData(format!(
            "unsupported sort_by '{other}' for YouGile; expected one of: created_at, updated_at \
             (both order by the task's single timestamp)"
        ))),
    }
}

/// YouGile's `assignedTo` filter matches on the user's **id**, not a name or
/// email. The shared tool schema describes `assignee` as a username, so a
/// caller following it would get an empty result set that is indistinguishable
/// from "this person has no tasks". Fail with an explanation instead.
fn validate_assignee_filter(assignee: Option<&str>) -> Result<()> {
    let Some(value) = assignee.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(());
    };

    // Reject only what an id can never be — an email or a multi-word name.
    // Ids are otherwise treated as opaque: requiring a UUID here would refuse
    // legitimate identifiers if YouGile ever issues another shape.
    if value.contains('@') || value.split_whitespace().count() > 1 {
        return Err(Error::InvalidData(format!(
            "YouGile filters assignees by user id, and '{value}' looks like a name or email; \
             pass the user's id instead"
        )));
    }
    Ok(())
}

fn matches_state_category(
    task: &YouGileTask,
    state_category: &str,
    status_name: Option<&str>,
) -> bool {
    map_status_category(task, status_name) == state_category
}

fn map_status_category_from_name(status_name: &str) -> String {
    let normalized = status_name
        .trim()
        .to_ascii_lowercase()
        .replace(['-', '_'], " ");

    if normalized.contains("backlog") || normalized.contains("icebox") {
        return "backlog".to_string();
    }
    if matches!(
        normalized.as_str(),
        "todo" | "to do" | "open" | "new" | "incoming" | "planned" | "plan"
    ) || normalized.contains("to do")
        || normalized.contains("todo")
    {
        return "todo".to_string();
    }
    if normalized.contains("review")
        || normalized.contains("progress")
        || normalized.contains("doing")
        || normalized.contains("active")
        || normalized.contains("develop")
        || normalized.contains("test")
        || normalized.contains("qa")
        || normalized.contains("verify")
        || normalized.contains("ready")
        || normalized.contains("wait")
    {
        return "in_progress".to_string();
    }
    if normalized.contains("done")
        || normalized.contains("complete")
        || normalized.contains("resolved")
        || normalized.contains("closed")
        || normalized.contains("shipped")
    {
        return "done".to_string();
    }
    if normalized.contains("cancel")
        || normalized.contains("archive")
        || normalized.contains("reject")
        || normalized.contains("wontfix")
        || normalized.contains("won't")
    {
        return "cancelled".to_string();
    }

    "in_progress".to_string()
}

fn map_status_category(task: &YouGileTask, status_name: Option<&str>) -> String {
    if task.deleted || task.archived {
        return "cancelled".to_string();
    }
    if task.completed {
        return "done".to_string();
    }
    map_status_category_from_name(status_name.unwrap_or_default())
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

fn map_yougile_user(user: YouGileUser) -> User {
    User {
        id: user.id,
        username: user.email.clone(),
        name: Some(user.real_name),
        email: Some(user.email),
        avatar_url: None,
    }
}

fn map_custom_fields(task: &YouGileTask) -> HashMap<String, CustomFieldValue> {
    let mut fields = HashMap::new();

    if let Some(Value::Object(stickers)) = &task.stickers {
        for (id, value) in stickers {
            fields.insert(
                id.clone(),
                CustomFieldValue {
                    name: None,
                    value: value.clone(),
                    display: value.as_str().map(ToOwned::to_owned),
                },
            );
        }
    }

    fields
}

fn map_related_issue(task: &YouGileTask, columns: &HashMap<String, String>) -> Issue {
    let status = task
        .column_id
        .as_ref()
        .and_then(|column_id| columns.get(column_id))
        .cloned();
    let updated_at = Some(epoch_millis_to_rfc3339(task.timestamp));

    Issue {
        key: task_display_key(task),
        title: task.title.clone(),
        description: task.description.clone(),
        state: if is_closed_task(task) {
            "closed"
        } else {
            "open"
        }
        .to_string(),
        status: status.clone(),
        status_category: Some(map_status_category(task, status.as_deref())),
        source: "yougile".to_string(),
        priority: None,
        labels: Vec::new(),
        author: None,
        assignees: Vec::new(),
        url: None,
        created_at: updated_at.clone(),
        updated_at,
        attachments_count: None,
        parent: None,
        subtasks: Vec::new(),
        custom_fields: map_custom_fields(task),
    }
}

fn build_task_index(tasks: &[YouGileTask]) -> HashMap<String, &YouGileTask> {
    tasks.iter().map(|task| (task.id.clone(), task)).collect()
}

fn build_parent_index(tasks: &[YouGileTask]) -> HashMap<String, &YouGileTask> {
    let mut parents = HashMap::new();
    for task in tasks {
        for child_id in &task.subtask_ids {
            parents.insert(child_id.clone(), task);
        }
    }
    parents
}

fn apply_parent_link_to_tasks(tasks: &mut [YouGileTask], child_id: &str, parent_id: Option<&str>) {
    for task in tasks.iter_mut() {
        task.subtask_ids.retain(|id| id != child_id);
    }

    if let Some(parent_id) = parent_id {
        if let Some(parent) = tasks.iter_mut().find(|task| task.id == parent_id) {
            if !parent.subtask_ids.iter().any(|id| id == child_id) {
                parent.subtask_ids.push(child_id.to_string());
            }
        }
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

fn custom_fields_to_stickers(custom_fields: Option<Value>) -> Result<Option<Value>> {
    let Some(value) = custom_fields else {
        return Ok(None);
    };
    let Value::Object(map) = value else {
        return Err(Error::InvalidData(
            "YouGile custom_fields must be a JSON object keyed by sticker id".to_string(),
        ));
    };

    let mut stickers = serde_json::Map::new();
    for (key, value) in map {
        let sticker_value = match value {
            Value::String(s) => Value::String(s),
            Value::Number(n) => Value::String(n.to_string()),
            Value::Bool(b) => Value::String(b.to_string()),
            Value::Null => Value::String("-".to_string()),
            other => {
                return Err(Error::InvalidData(format!(
                    "YouGile custom_fields[{key}] must be a scalar value, got {other}"
                )));
            }
        };
        stickers.insert(key, sticker_value);
    }

    Ok(Some(Value::Object(stickers)))
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::{GET, POST, PUT};
    use httpmock::MockServer;

    #[test]
    fn advance_offset_refuses_to_spin_on_a_zero_page_size() {
        let ok = YouGilePaging {
            limit: 50,
            offset: 0,
            next: true,
        };
        assert_eq!(advance_offset(100, &ok, "/task-list").unwrap(), 150);

        // limit == 0 would leave the offset unchanged and loop forever.
        let stuck = YouGilePaging {
            limit: 0,
            offset: 0,
            next: true,
        };
        let err = advance_offset(100, &stuck, "/task-list").unwrap_err();
        assert!(
            matches!(&err, Error::InvalidData(m) if m.contains("/task-list")),
            "unexpected error: {err:?}"
        );

        // Saturating at u32::MAX would re-request the same offset forever.
        let huge = YouGilePaging {
            limit: 10,
            offset: 0,
            next: true,
        };
        let err = advance_offset(u32::MAX - 1, &huge, "/task-list").unwrap_err();
        assert!(
            matches!(&err, Error::InvalidData(m) if m.contains("maximum offset")),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn validate_sort_by_accepts_both_timestamp_aliases_and_names_the_rest() {
        for ok in [None, Some(""), Some("created_at"), Some("updated_at")] {
            validate_sort_by(ok).unwrap_or_else(|e| panic!("{ok:?} rejected: {e:?}"));
        }
        let err = validate_sort_by(Some("priority")).unwrap_err();
        assert!(
            matches!(&err, Error::InvalidData(m) if m.contains("priority") && m.contains("created_at")),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn validate_assignee_filter_rejects_names_that_would_silently_match_nothing() {
        validate_assignee_filter(None).unwrap();
        validate_assignee_filter(Some("11111111-2222-3333-4444-555555555555")).unwrap();

        // Opaque non-UUID ids must still pass — only names/emails are refused.
        validate_assignee_filter(Some("usr_12345")).unwrap();

        for bad in ["alice@example.com", "Alice Doe"] {
            let err = validate_assignee_filter(Some(bad)).unwrap_err();
            assert!(
                matches!(&err, Error::InvalidData(m) if m.contains(bad) && m.contains("user id")),
                "unexpected error for {bad}: {err:?}"
            );
        }
    }

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
                    "idTaskProject": "DEV-484",
                    "stickers": {
                        "severity": "high"
                    }
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
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api-v2/task-list")
                    .query_param("columnId", "col-1")
                    .query_param("limit", "1000")
                    .query_param("offset", "0")
                    .query_param("includeDeleted", "true");
                then.status(200).json_body_obj(&serde_json::json!({
                    "paging": { "limit": 1000, "offset": 0, "next": false },
                    "content": [
                        {
                            "id": "parent-1",
                            "title": "Parent task",
                            "timestamp": 1709999999000_u64,
                            "columnId": "col-1",
                            "completed": false,
                            "archived": false,
                            "deleted": false,
                            "idTaskProject": "DEV-100",
                            "subtasks": ["task-1"]
                        },
                        {
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
                            "idTaskProject": "DEV-484",
                            "stickers": {
                                "severity": "high"
                            }
                        }
                    ]
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api-v2/users/user-1");
                then.status(200).json_body_obj(&serde_json::json!({
                    "id": "user-1",
                    "email": "assignee@example.com",
                    "realName": "Assignee User"
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api-v2/users/user-2");
                then.status(200).json_body_obj(&serde_json::json!({
                    "id": "user-2",
                    "email": "author@example.com",
                    "realName": "Author User"
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
        assert_eq!(issue.status_category.as_deref(), Some("todo"));
        assert_eq!(
            issue.author.as_ref().unwrap().username,
            "author@example.com"
        );
        assert_eq!(
            issue.author.as_ref().unwrap().name.as_deref(),
            Some("Author User")
        );
        assert_eq!(issue.assignees[0].username, "assignee@example.com");
        assert_eq!(issue.parent.as_deref(), Some("DEV-100"));
        assert_eq!(issue.updated_at.as_deref(), issue.created_at.as_deref());
        assert_eq!(
            issue.custom_fields["severity"].display.as_deref(),
            Some("high")
        );
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
        assert_eq!(issues.items[0].status_category.as_deref(), Some("done"));
        assert_eq!(issues.items[1].status_category.as_deref(), Some("todo"));
        assert_eq!(issues.pagination.unwrap().total, Some(2));
    }

    #[tokio::test]
    async fn get_issues_maps_parent_and_subtasks_from_board_hierarchy() {
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
                        { "id": "col-1", "title": "To Do", "boardId": "board-1" }
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
                            "id": "parent-1",
                            "title": "Epic",
                            "timestamp": 1710000002000_u64,
                            "columnId": "col-1",
                            "completed": false,
                            "archived": false,
                            "deleted": false,
                            "idTaskProject": "DEV-20",
                            "subtasks": ["child-1"]
                        },
                        {
                            "id": "child-1",
                            "title": "Child task",
                            "timestamp": 1710000001000_u64,
                            "columnId": "col-1",
                            "completed": false,
                            "archived": false,
                            "deleted": false,
                            "idTaskProject": "DEV-21"
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
        let parent = issues
            .items
            .iter()
            .find(|issue| issue.key == "DEV-20")
            .unwrap();
        let child = issues
            .items
            .iter()
            .find(|issue| issue.key == "DEV-21")
            .unwrap();

        assert_eq!(parent.subtasks.len(), 1);
        assert_eq!(parent.subtasks[0].key, "DEV-21");
        assert_eq!(child.parent.as_deref(), Some("DEV-20"));
        assert_eq!(child.updated_at.as_deref(), child.created_at.as_deref());
    }

    #[tokio::test]
    async fn create_issue_posts_task_and_fetches_created_issue() {
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
                        { "id": "col-1", "title": "To Do", "boardId": "board-1" }
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
                    .query_param("offset", "0")
                    .query_param("includeDeleted", "true");
                then.status(200).json_body_obj(&serde_json::json!({
                    "paging": { "limit": 1000, "offset": 0, "next": false },
                    "content": [
                        {
                            "id": "task-1",
                            "title": "Create task",
                            "timestamp": 1710000000000_u64,
                            "columnId": "col-1",
                            "description": "body",
                            "completed": false,
                            "archived": false,
                            "deleted": false,
                            "assigned": ["user-1"],
                            "idTaskProject": "DEV-10"
                        }
                    ]
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(POST).path("/api-v2/tasks");
                then.status(201).json_body_obj(&serde_json::json!({
                    "id": "task-1"
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api-v2/tasks/task-1");
                then.status(200).json_body_obj(&serde_json::json!({
                    "id": "task-1",
                    "title": "Create task",
                    "timestamp": 1710000000000_u64,
                    "columnId": "col-1",
                    "description": "body",
                    "completed": false,
                    "archived": false,
                    "deleted": false,
                    "assigned": ["user-1"],
                    "idTaskProject": "DEV-10"
                }));
            })
            .await;

        let client = YouGileClient::with_base_url(
            format!("{}/api-v2", server.base_url()),
            "board-1",
            SecretString::from("token".to_owned()),
        );
        let issue = client
            .create_issue(CreateIssueInput {
                title: "Create task".to_string(),
                description: Some("body".to_string()),
                assignees: vec!["user-1".to_string()],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(issue.key, "DEV-10");
        assert_eq!(issue.status.as_deref(), Some("To Do"));
    }

    #[tokio::test]
    async fn create_issue_with_parent_updates_parent_subtasks() {
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
                        { "id": "col-1", "title": "To Do", "boardId": "board-1" }
                    ]
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(POST).path("/api-v2/tasks");
                then.status(201).json_body_obj(&serde_json::json!({
                    "id": "task-2"
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api-v2/task-list")
                    .query_param("columnId", "col-1")
                    .query_param("limit", "1000")
                    .query_param("offset", "0")
                    .query_param("includeDeleted", "true");
                then.status(200).json_body_obj(&serde_json::json!({
                    "paging": { "limit": 1000, "offset": 0, "next": false },
                    "content": [
                        {
                            "id": "parent-1",
                            "title": "Parent task",
                            "timestamp": 1710000000000_u64,
                            "columnId": "col-1",
                            "completed": false,
                            "archived": false,
                            "deleted": false,
                            "idTaskProject": "DEV-700",
                            "subtasks": ["task-2"]
                        },
                        {
                            "id": "task-2",
                            "title": "New child",
                            "timestamp": 1710000001000_u64,
                            "columnId": "col-1",
                            "completed": false,
                            "archived": false,
                            "deleted": false,
                            "idTaskProject": "DEV-701"
                        }
                    ]
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(PUT)
                    .path("/api-v2/tasks/parent-1")
                    .json_body_obj(&serde_json::json!({
                        "subtasks": ["task-2"]
                    }));
                then.status(200).json_body_obj(&serde_json::json!({
                    "id": "parent-1"
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api-v2/tasks/task-2");
                then.status(200).json_body_obj(&serde_json::json!({
                    "id": "task-2",
                    "title": "New child",
                    "timestamp": 1710000001000_u64,
                    "columnId": "col-1",
                    "completed": false,
                    "archived": false,
                    "deleted": false,
                    "idTaskProject": "DEV-701"
                }));
            })
            .await;

        let client = YouGileClient::with_base_url(
            format!("{}/api-v2", server.base_url()),
            "board-1",
            SecretString::from("token".to_owned()),
        );
        let issue = client
            .create_issue(CreateIssueInput {
                title: "New child".to_string(),
                parent: Some("DEV-700".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(issue.key, "DEV-701");
        assert_eq!(issue.parent.as_deref(), Some("DEV-700"));
    }

    #[tokio::test]
    async fn update_issue_maps_status_to_column_and_refetches() {
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
                    .query_param("offset", "0")
                    .query_param("includeDeleted", "true");
                then.status(200).json_body_obj(&serde_json::json!({
                    "paging": { "limit": 1000, "offset": 0, "next": false },
                    "content": [
                        {
                            "id": "task-1",
                            "title": "Updated task",
                            "timestamp": 1710000000000_u64,
                            "columnId": "col-2",
                            "completed": false,
                            "archived": false,
                            "deleted": false,
                            "idTaskProject": "DEV-11"
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
                    .query_param("offset", "0")
                    .query_param("includeDeleted", "true");
                then.status(200).json_body_obj(&serde_json::json!({
                    "paging": { "limit": 1000, "offset": 0, "next": false },
                    "content": [
                        {
                            "id": "task-1",
                            "title": "Updated task",
                            "timestamp": 1710000000000_u64,
                            "columnId": "col-2",
                            "completed": false,
                            "archived": false,
                            "deleted": false,
                            "idTaskProject": "DEV-11"
                        }
                    ]
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(PUT)
                    .path("/api-v2/tasks/task-1")
                    .json_body_obj(&serde_json::json!({
                        "title": "Updated task",
                        "columnId": "col-2",
                        "completed": true,
                        "archived": false,
                        "deleted": false
                    }));
                then.status(200).json_body_obj(&serde_json::json!({
                    "id": "task-1"
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api-v2/tasks/task-1");
                then.status(200).json_body_obj(&serde_json::json!({
                    "id": "task-1",
                    "title": "Updated task",
                    "timestamp": 1710000000000_u64,
                    "columnId": "col-2",
                    "completed": false,
                    "archived": false,
                    "deleted": false,
                    "idTaskProject": "DEV-11"
                }));
            })
            .await;

        let client = YouGileClient::with_base_url(
            format!("{}/api-v2", server.base_url()),
            "board-1",
            SecretString::from("token".to_owned()),
        );
        let issue = client
            .update_issue(
                "yougile#task-1",
                UpdateIssueInput {
                    title: Some("Updated task".to_string()),
                    status: Some("Done".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(issue.key, "DEV-11");
        assert_eq!(issue.status.as_deref(), Some("Done"));
    }

    #[tokio::test]
    async fn update_issue_status_reopens_task_when_moved_to_active_column() {
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
                    .query_param("offset", "0")
                    .query_param("includeDeleted", "true");
                then.status(200).json_body_obj(&serde_json::json!({
                    "paging": { "limit": 1000, "offset": 0, "next": false },
                    "content": [
                        {
                            "id": "task-1",
                            "title": "Reopened task",
                            "timestamp": 1710000000000_u64,
                            "columnId": "col-1",
                            "completed": false,
                            "archived": false,
                            "deleted": false,
                            "idTaskProject": "DEV-12"
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
                    .query_param("offset", "0")
                    .query_param("includeDeleted", "true");
                then.status(200).json_body_obj(&serde_json::json!({
                    "paging": { "limit": 1000, "offset": 0, "next": false },
                    "content": []
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(PUT)
                    .path("/api-v2/tasks/task-1")
                    .json_body_obj(&serde_json::json!({
                        "columnId": "col-1",
                        "completed": false,
                        "archived": false,
                        "deleted": false
                    }));
                then.status(200).json_body_obj(&serde_json::json!({
                    "id": "task-1"
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api-v2/tasks/task-1");
                then.status(200).json_body_obj(&serde_json::json!({
                    "id": "task-1",
                    "title": "Reopened task",
                    "timestamp": 1710000000000_u64,
                    "columnId": "col-1",
                    "completed": false,
                    "archived": false,
                    "deleted": false,
                    "idTaskProject": "DEV-12"
                }));
            })
            .await;

        let client = YouGileClient::with_base_url(
            format!("{}/api-v2", server.base_url()),
            "board-1",
            SecretString::from("token".to_owned()),
        );
        let issue = client
            .update_issue(
                "yougile#task-1",
                UpdateIssueInput {
                    status: Some("To Do".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(issue.key, "DEV-12");
        assert_eq!(issue.state, "open");
        assert_eq!(issue.status.as_deref(), Some("To Do"));
    }

    #[tokio::test]
    async fn update_issue_state_closed_clears_cancelled_flags() {
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
                        { "id": "col-1", "title": "To Do", "boardId": "board-1" }
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
                    .query_param("offset", "0")
                    .query_param("includeDeleted", "true");
                then.status(200).json_body_obj(&serde_json::json!({
                    "paging": { "limit": 1000, "offset": 0, "next": false },
                    "content": [
                        {
                            "id": "task-1",
                            "title": "Updated task",
                            "timestamp": 1710000000000_u64,
                            "columnId": "col-1",
                            "completed": false,
                            "archived": true,
                            "deleted": false,
                            "idTaskProject": "DEV-11"
                        }
                    ]
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(PUT)
                    .path("/api-v2/tasks/task-1")
                    .json_body_obj(&serde_json::json!({
                        "completed": true,
                        "archived": false,
                        "deleted": false
                    }));
                then.status(200).json_body_obj(&serde_json::json!({
                    "id": "task-1"
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api-v2/tasks/task-1");
                then.status(200).json_body_obj(&serde_json::json!({
                    "id": "task-1",
                    "title": "Updated task",
                    "timestamp": 1710000000000_u64,
                    "columnId": "col-1",
                    "completed": true,
                    "archived": false,
                    "deleted": false,
                    "idTaskProject": "DEV-11"
                }));
            })
            .await;

        let client = YouGileClient::with_base_url(
            format!("{}/api-v2", server.base_url()),
            "board-1",
            SecretString::from("token".to_owned()),
        );
        let issue = client
            .update_issue(
                "yougile#task-1",
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
    async fn update_issue_parent_id_attaches_task_to_parent() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api-v2/task-list")
                    .query_param("columnId", "col-1")
                    .query_param("limit", "1000")
                    .query_param("offset", "0")
                    .query_param("includeDeleted", "true");
                then.status(200).json_body_obj(&serde_json::json!({
                    "paging": { "limit": 1000, "offset": 0, "next": false },
                    "content": [
                        {
                            "id": "parent-1",
                            "title": "Parent task",
                            "timestamp": 1710000000000_u64,
                            "columnId": "col-1",
                            "completed": false,
                            "archived": false,
                            "deleted": false,
                            "idTaskProject": "DEV-800"
                        },
                        {
                            "id": "task-1",
                            "title": "Child task",
                            "timestamp": 1710000001000_u64,
                            "columnId": "col-1",
                            "completed": false,
                            "archived": false,
                            "deleted": false,
                            "idTaskProject": "DEV-801"
                        }
                    ]
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(PUT)
                    .path("/api-v2/tasks/parent-1")
                    .json_body_obj(&serde_json::json!({
                        "subtasks": ["task-1"]
                    }));
                then.status(200).json_body_obj(&serde_json::json!({
                    "id": "parent-1"
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api-v2/tasks/task-1");
                then.status(200).json_body_obj(&serde_json::json!({
                    "id": "task-1",
                    "title": "Child task",
                    "timestamp": 1710000001000_u64,
                    "columnId": "col-1",
                    "completed": false,
                    "archived": false,
                    "deleted": false,
                    "idTaskProject": "DEV-801"
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
        let issue = client
            .update_issue(
                "yougile#task-1",
                UpdateIssueInput {
                    parent_id: Some("DEV-800".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(issue.key, "DEV-801");
        assert_eq!(issue.parent.as_deref(), Some("DEV-800"));
    }

    #[tokio::test]
    async fn update_issue_parent_id_none_detaches_task_from_parent() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api-v2/task-list")
                    .query_param("columnId", "col-1")
                    .query_param("limit", "1000")
                    .query_param("offset", "0")
                    .query_param("includeDeleted", "true");
                then.status(200).json_body_obj(&serde_json::json!({
                    "paging": { "limit": 1000, "offset": 0, "next": false },
                    "content": [
                        {
                            "id": "parent-1",
                            "title": "Parent task",
                            "timestamp": 1710000000000_u64,
                            "columnId": "col-1",
                            "completed": false,
                            "archived": false,
                            "deleted": false,
                            "idTaskProject": "DEV-900",
                            "subtasks": ["task-1"]
                        },
                        {
                            "id": "task-1",
                            "title": "Child task",
                            "timestamp": 1710000001000_u64,
                            "columnId": "col-1",
                            "completed": false,
                            "archived": false,
                            "deleted": false,
                            "idTaskProject": "DEV-901"
                        }
                    ]
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(PUT)
                    .path("/api-v2/tasks/parent-1")
                    .json_body_obj(&serde_json::json!({
                        "subtasks": []
                    }));
                then.status(200).json_body_obj(&serde_json::json!({
                    "id": "parent-1"
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api-v2/tasks/task-1");
                then.status(200).json_body_obj(&serde_json::json!({
                    "id": "task-1",
                    "title": "Child task",
                    "timestamp": 1710000001000_u64,
                    "columnId": "col-1",
                    "completed": false,
                    "archived": false,
                    "deleted": false,
                    "idTaskProject": "DEV-901"
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
        let issue = client
            .update_issue(
                "yougile#task-1",
                UpdateIssueInput {
                    parent_id: Some("none".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(issue.key, "DEV-901");
        assert_eq!(issue.parent, None);
    }

    #[tokio::test]
    async fn create_issue_rejects_generic_labels_and_priority() {
        let client = YouGileClient::new("board-1", SecretString::from("token".to_owned()));

        let err = client
            .create_issue(CreateIssueInput {
                title: "Task".to_string(),
                labels: vec!["bug".to_string()],
                priority: Some("high".to_string()),
                ..Default::default()
            })
            .await
            .unwrap_err();

        assert!(matches!(err, Error::InvalidData(msg) if msg.contains("generic labels")));
    }

    #[tokio::test]
    async fn update_issue_rejects_generic_labels_and_priority() {
        let client = YouGileClient::new("board-1", SecretString::from("token".to_owned()));

        let err = client
            .update_issue(
                "yougile#task-1",
                UpdateIssueInput {
                    labels: Some(vec!["bug".to_string()]),
                    priority: Some("high".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();

        assert!(matches!(err, Error::InvalidData(msg) if msg.contains("generic labels")));
    }

    #[tokio::test]
    async fn get_comments_reads_task_chat_messages() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api-v2/chats/task-1/messages")
                    .query_param("limit", "1000")
                    .query_param("offset", "0")
                    .query_param("includeSystem", "false");
                then.status(200).json_body_obj(&serde_json::json!({
                    "paging": { "limit": 1000, "offset": 0, "next": false },
                    "content": [
                        {
                            "id": 1710000000000_u64,
                            "fromUserId": "user-1",
                            "text": "First comment",
                            "textHtml": "<p>First comment</p>",
                            "label": "",
                            "editTimestamp": 1710000000000_u64,
                            "reactions": {}
                        }
                    ]
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api-v2/users/user-1");
                then.status(200).json_body_obj(&serde_json::json!({
                    "id": "user-1",
                    "email": "commenter@example.com",
                    "realName": "Comment Author"
                }));
            })
            .await;

        let client = YouGileClient::with_base_url(
            format!("{}/api-v2", server.base_url()),
            "board-1",
            SecretString::from("token".to_owned()),
        );
        let comments = client.get_comments("yougile#task-1").await.unwrap();
        assert_eq!(comments.items.len(), 1);
        assert_eq!(comments.items[0].body, "First comment");
        assert_eq!(comments.items[0].author.as_ref().unwrap().id, "user-1");
        assert_eq!(
            comments.items[0].author.as_ref().unwrap().username,
            "commenter@example.com"
        );
    }

    #[tokio::test]
    async fn add_comment_posts_chat_message_and_returns_created_comment() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(POST).path("/api-v2/chats/task-1/messages");
                then.status(201).json_body_obj(&serde_json::json!({
                    "id": 1710000000001_u64
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/api-v2/chats/task-1/messages")
                    .query_param("limit", "1000")
                    .query_param("offset", "0")
                    .query_param("includeSystem", "false");
                then.status(200).json_body_obj(&serde_json::json!({
                    "paging": { "limit": 1000, "offset": 0, "next": false },
                    "content": [
                        {
                            "id": 1710000000001_u64,
                            "fromUserId": "user-2",
                            "text": "Hello",
                            "textHtml": "<p>Hello</p>",
                            "label": "",
                            "editTimestamp": 1710000000001_u64,
                            "reactions": {}
                        }
                    ]
                }));
            })
            .await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api-v2/users/user-2");
                then.status(200).json_body_obj(&serde_json::json!({
                    "id": "user-2",
                    "email": "writer@example.com",
                    "realName": "Comment Writer"
                }));
            })
            .await;

        let client = YouGileClient::with_base_url(
            format!("{}/api-v2", server.base_url()),
            "board-1",
            SecretString::from("token".to_owned()),
        );
        let comment = IssueProvider::add_comment(&client, "yougile#task-1", "Hello")
            .await
            .unwrap();
        assert_eq!(comment.body, "Hello");
        assert_eq!(comment.author.as_ref().unwrap().id, "user-2");
        assert_eq!(
            comment.author.as_ref().unwrap().name.as_deref(),
            Some("Comment Writer")
        );
    }

    #[tokio::test]
    async fn rate_limiter_spaces_requests() {
        let limiter = YouGileRateLimiter::new(Duration::from_millis(25));
        let start = std::time::Instant::now();

        limiter.acquire().await;
        limiter.acquire().await;
        limiter.acquire().await;

        assert!(
            start.elapsed() >= Duration::from_millis(45),
            "expected spaced acquires, got {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn map_status_category_uses_column_name_heuristics() {
        let task = YouGileTask {
            id: "task-1".to_string(),
            title: "Task".to_string(),
            timestamp: 0,
            column_id: Some("col-1".to_string()),
            description: None,
            archived: false,
            completed: false,
            deleted: false,
            subtask_ids: Vec::new(),
            assigned_ids: Vec::new(),
            created_by: None,
            id_task_common: None,
            id_task_project: None,
            stickers: None,
        };

        assert_eq!(map_status_category(&task, Some("Backlog")), "backlog");
        assert_eq!(map_status_category(&task, Some("To Do")), "todo");
        assert_eq!(
            map_status_category(&task, Some("Code Review")),
            "in_progress"
        );
        assert_eq!(map_status_category(&task, Some("Done")), "done");
        assert_eq!(map_status_category(&task, Some("Cancelled")), "cancelled");
    }

    #[tokio::test]
    async fn parse_json_response_preserves_retry_after_on_http_429() {
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/api-v2/test-rate-limit");
                then.status(429)
                    .header("Retry-After", "7")
                    .body("Too Many Requests");
            })
            .await;

        let response = reqwest::Client::new()
            .get(format!("{}/api-v2/test-rate-limit", server.base_url()))
            .send()
            .await
            .unwrap();
        let err = parse_json_response::<Value>(response).await.unwrap_err();

        assert!(matches!(
            err,
            Error::RateLimited {
                retry_after: Some(7)
            }
        ));
    }
}
