//! Linear GraphQL client implementation.

use async_trait::async_trait;
use devboy_core::{
    Comment, CreateIssueInput, Error, Issue, IssueFilter, IssueProvider, MergeRequestProvider,
    Pagination, PipelineProvider, Provider, ProviderResult, Result, SortInfo, SortOrder,
    UpdateIssueInput, User,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Map, Value, json};
use tracing::debug;

use crate::DEFAULT_LINEAR_URL;
use crate::types::{
    GraphQlResponse, LinearComment, LinearCommentCreateData, LinearIssue, LinearIssueCommentsData,
    LinearIssueCreateData, LinearIssueData, LinearIssueLabelsData, LinearIssueUpdateData,
    LinearIssuesData, LinearUser, LinearUsersData, LinearWorkflowStatesData, Viewer, ViewerData,
};

const VIEWER_QUERY: &str = r#"
query Viewer {
  viewer {
    id
    name
    displayName
    email
  }
}
"#;

const ISSUE_BY_ID_QUERY: &str = r#"
query IssueById($id: String!) {
  issue(id: $id) {
    id
    identifier
    title
    description
    priority
    url
    createdAt
    updatedAt
    state {
      name
      type
    }
    labels {
      nodes {
        name
      }
    }
    assignee {
      id
      name
      displayName
      email
      avatarUrl
    }
    parent {
      identifier
    }
  }
}
"#;

const ISSUES_QUERY: &str = r#"
query Issues($first: Int!, $after: String, $filter: IssueFilter) {
  issues(first: $first, after: $after, filter: $filter) {
    nodes {
      id
      identifier
      title
      description
      priority
      url
      createdAt
      updatedAt
      state {
        name
        type
      }
      labels {
        nodes {
          name
        }
      }
      assignee {
        id
        name
        displayName
        email
        avatarUrl
      }
      parent {
        identifier
      }
    }
    pageInfo {
      hasNextPage
      endCursor
    }
  }
}
"#;

const USERS_QUERY: &str = r#"
query Users($first: Int!, $filter: UserFilter) {
  users(first: $first, filter: $filter) {
    nodes {
      id
      name
      displayName
      email
      avatarUrl
    }
  }
}
"#;

const ISSUE_LABELS_QUERY: &str = r#"
query IssueLabels($first: Int!, $filter: IssueLabelFilter) {
  issueLabels(first: $first, filter: $filter) {
    nodes {
      id
      name
    }
  }
}
"#;

const ISSUE_CREATE_MUTATION: &str = r#"
mutation IssueCreate($input: IssueCreateInput!) {
  issueCreate(input: $input) {
    success
    issue {
      id
      identifier
      title
      description
      priority
      url
      createdAt
      updatedAt
      state {
        name
        type
      }
      labels {
        nodes {
          name
        }
      }
      assignee {
        id
        name
        displayName
        email
        avatarUrl
      }
      parent {
        identifier
      }
    }
  }
}
"#;

const WORKFLOW_STATES_QUERY: &str = r#"
query WorkflowStates($first: Int!, $filter: WorkflowStateFilter) {
  workflowStates(first: $first, filter: $filter) {
    nodes {
      id
      name
      type
    }
  }
}
"#;

const ISSUE_UPDATE_MUTATION: &str = r#"
mutation IssueUpdate($id: String!, $input: IssueUpdateInput!) {
  issueUpdate(id: $id, input: $input) {
    success
    issue {
      id
      identifier
      title
      description
      priority
      url
      createdAt
      updatedAt
      state {
        name
        type
      }
      labels {
        nodes {
          name
        }
      }
      assignee {
        id
        name
        displayName
        email
        avatarUrl
      }
      parent {
        identifier
      }
    }
  }
}
"#;

const ISSUE_COMMENTS_QUERY: &str = r#"
query IssueComments($id: String!, $first: Int!, $after: String) {
  issue(id: $id) {
    comments(first: $first, after: $after) {
      nodes {
        id
        body
        createdAt
        updatedAt
        user {
          id
          name
          displayName
          email
          avatarUrl
        }
      }
      pageInfo {
        hasNextPage
        endCursor
      }
    }
  }
}
"#;

const COMMENT_CREATE_MUTATION: &str = r#"
mutation CommentCreate($input: CommentCreateInput!) {
  commentCreate(input: $input) {
    success
    comment {
      id
      body
      createdAt
      updatedAt
      user {
        id
        name
        displayName
        email
        avatarUrl
      }
    }
  }
}
"#;

pub struct LinearClient {
    base_url: String,
    team_id: String,
    team_key: Option<String>,
    token: SecretString,
    http: reqwest::Client,
}

impl LinearClient {
    pub fn new(team_id: impl Into<String>, token: SecretString) -> Self {
        Self::with_base_url(DEFAULT_LINEAR_URL, team_id, token)
    }

    pub fn with_base_url(
        base_url: impl Into<String>,
        team_id: impl Into<String>,
        token: SecretString,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            team_id: team_id.into(),
            team_key: None,
            token,
            http: reqwest::Client::builder()
                .user_agent("devboy-tools")
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    pub fn with_team_key(mut self, team_key: impl Into<String>) -> Self {
        self.team_key = Some(team_key.into());
        self
    }

    pub fn team_id(&self) -> &str {
        &self.team_id
    }

    pub fn team_key(&self) -> Option<&str> {
        self.team_key.as_deref()
    }

    pub(crate) async fn viewer_with_token(&self, token: &SecretString) -> Result<Viewer> {
        let data: ViewerData = self.graphql(VIEWER_QUERY, json!({}), token).await?;
        Ok(data.viewer)
    }

    async fn graphql<T: serde::de::DeserializeOwned>(
        &self,
        query: &str,
        variables: Value,
        token: &SecretString,
    ) -> Result<T> {
        let body = json!({
            "query": query,
            "variables": variables,
        });

        debug!(url = %self.base_url, "linear graphql request");

        let response = self
            .http
            .post(&self.base_url)
            .header("Authorization", token.expose_secret())
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(Error::Unauthorized("Invalid Linear API token".to_string()));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(Error::RateLimited { retry_after: None });
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(Error::Api {
                status: status.as_u16(),
                message: text,
            });
        }

        let gql_response: GraphQlResponse<T> = response
            .json()
            .await
            .map_err(|e| Error::InvalidData(e.to_string()))?;

        if !gql_response.errors.is_empty() {
            let rate_limited = gql_response.errors.iter().any(|e| {
                e.extensions.as_ref().and_then(|x| x.code.as_deref()) == Some("RATELIMITED")
            });
            let message = gql_response
                .errors
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            return if rate_limited {
                let _ = message;
                Err(Error::RateLimited { retry_after: None })
            } else {
                Err(Error::Api {
                    status: 200,
                    message,
                })
            };
        }

        gql_response
            .data
            .ok_or_else(|| Error::InvalidData("Linear API returned no data".to_string()))
    }

    async fn list_issues_page(
        &self,
        first: u32,
        after: Option<&str>,
        filter: Value,
    ) -> Result<LinearIssuesData> {
        let variables = json!({
            "first": first,
            "after": after,
            "filter": filter,
        });
        self.graphql(ISSUES_QUERY, variables, &self.token).await
    }

    async fn get_issue_by_native_id(&self, id: &str) -> Result<Option<Issue>> {
        Ok(self
            .get_linear_issue_by_native_id(id)
            .await?
            .as_ref()
            .map(map_issue))
    }

    async fn get_linear_issue_by_native_id(&self, id: &str) -> Result<Option<LinearIssue>> {
        let data: LinearIssueData = self
            .graphql(ISSUE_BY_ID_QUERY, json!({ "id": id }), &self.token)
            .await?;
        Ok(data.issue)
    }

    async fn get_issue_by_identifier(&self, identifier: &str) -> Result<Option<Issue>> {
        Ok(self
            .get_linear_issue_by_identifier(identifier)
            .await?
            .as_ref()
            .map(map_issue))
    }

    async fn get_linear_issue_by_identifier(
        &self,
        identifier: &str,
    ) -> Result<Option<LinearIssue>> {
        let number = parse_linear_identifier(identifier)
            .map(|(_, number)| number)
            .ok_or_else(|| {
                Error::InvalidData(format!(
                    "Linear issue key '{identifier}' must be a UUID or team-key identifier like ENG-123"
                ))
            })?;

        let filter = json!({
            "and": [
                {
                    "team": {
                        "id": {
                            "eq": self.team_id
                        }
                    }
                },
                {
                    "number": {
                        "eq": number
                    }
                }
            ]
        });

        let data = self.list_issues_page(1, None, filter).await?;
        Ok(data.issues.nodes.into_iter().next())
    }

    async fn resolve_assignee_id(&self, assignee: &str) -> Result<String> {
        if looks_like_uuid(assignee) {
            return Ok(assignee.to_string());
        }

        let filter = json!({
            "or": [
                {
                    "name": {
                        "eqIgnoreCase": assignee
                    }
                },
                {
                    "displayName": {
                        "eqIgnoreCase": assignee
                    }
                },
                {
                    "email": {
                        "eqIgnoreCase": assignee
                    }
                }
            ]
        });
        let variables = json!({
            "first": 10,
            "filter": filter,
        });
        let data: LinearUsersData = self.graphql(USERS_QUERY, variables, &self.token).await?;
        let user = data
            .users
            .nodes
            .into_iter()
            .find(|user| {
                user.name.eq_ignore_ascii_case(assignee)
                    || user
                        .display_name
                        .as_deref()
                        .is_some_and(|display| display.eq_ignore_ascii_case(assignee))
                    || user
                        .email
                        .as_deref()
                        .is_some_and(|email| email.eq_ignore_ascii_case(assignee))
            })
            .ok_or_else(|| Error::NotFound(format!("Linear user not found: {assignee}")))?;
        Ok(user.id)
    }

    async fn resolve_label_ids(&self, labels: &[String]) -> Result<Vec<String>> {
        if labels.is_empty() {
            return Ok(Vec::new());
        }

        let variables = json!({
            "first": 100,
            "filter": {
                "team": {
                    "id": {
                        "eq": self.team_id
                    }
                },
                "name": {
                    "in": labels
                }
            }
        });
        let data: LinearIssueLabelsData = self
            .graphql(ISSUE_LABELS_QUERY, variables, &self.token)
            .await?;

        let mut ids = Vec::with_capacity(labels.len());
        let mut missing = Vec::new();
        for wanted in labels {
            if let Some(label) = data
                .issue_labels
                .nodes
                .iter()
                .find(|label| label.name.eq_ignore_ascii_case(wanted))
            {
                ids.push(label.id.clone());
            } else {
                missing.push(wanted.clone());
            }
        }

        if !missing.is_empty() {
            return Err(Error::NotFound(format!(
                "Linear labels not found for team {}: {}",
                self.team_id,
                missing.join(", ")
            )));
        }

        Ok(ids)
    }

    async fn resolve_parent_id(&self, parent: &str) -> Result<String> {
        if looks_like_uuid(parent) {
            return Ok(parent.to_string());
        }

        let issue = self
            .get_linear_issue_by_identifier(parent)
            .await?
            .ok_or_else(|| Error::NotFound(format!("Linear parent issue not found: {parent}")))?;
        Ok(issue.id)
    }

    async fn resolve_workflow_state_id(&self, state: &str) -> Result<String> {
        let state = state.trim();
        if state.is_empty() {
            return Err(Error::InvalidData(
                "Linear state/status must not be empty".to_string(),
            ));
        }

        let target_name = match state.to_ascii_lowercase().as_str() {
            "open" | "opened" => None,
            "closed" => Some("completed"),
            "cancelled" | "canceled" => Some("canceled"),
            "backlog" => Some("backlog"),
            "todo" => Some("unstarted"),
            "in_progress" | "in progress" | "started" => Some("started"),
            "done" | "completed" => Some("completed"),
            other => {
                return self.resolve_workflow_state_id_by_name(other).await;
            }
        };

        let variables = json!({
            "first": 100,
            "filter": {
                "team": {
                    "id": {
                        "eq": self.team_id
                    }
                }
            }
        });
        let data: LinearWorkflowStatesData = self
            .graphql(WORKFLOW_STATES_QUERY, variables, &self.token)
            .await?;

        let state_id = if let Some(target_type) = target_name {
            data.workflow_states
                .nodes
                .into_iter()
                .find(|node| node.r#type.as_deref() == Some(target_type))
                .map(|node| node.id)
        } else {
            data.workflow_states
                .nodes
                .into_iter()
                .find(|node| {
                    !matches!(node.r#type.as_deref(), Some("completed") | Some("canceled"))
                })
                .map(|node| node.id)
        };

        state_id.ok_or_else(|| {
            Error::NotFound(format!(
                "Linear workflow state not found for '{}' in team {}",
                state, self.team_id
            ))
        })
    }

    async fn resolve_workflow_state_id_by_name(&self, state_name: &str) -> Result<String> {
        let variables = json!({
            "first": 100,
            "filter": {
                "team": {
                    "id": {
                        "eq": self.team_id
                    }
                },
                "name": {
                    "eqIgnoreCase": state_name
                }
            }
        });
        let data: LinearWorkflowStatesData = self
            .graphql(WORKFLOW_STATES_QUERY, variables, &self.token)
            .await?;
        data.workflow_states
            .nodes
            .into_iter()
            .find(|node| node.name.eq_ignore_ascii_case(state_name))
            .map(|node| node.id)
            .ok_or_else(|| {
                Error::NotFound(format!(
                    "Linear workflow state not found by name '{}' in team {}",
                    state_name, self.team_id
                ))
            })
    }

    fn map_create_priority(priority: Option<&str>) -> Result<Option<i32>> {
        let Some(priority) = priority.map(str::trim).filter(|p| !p.is_empty()) else {
            return Ok(None);
        };

        match priority.to_ascii_lowercase().as_str() {
            "none" | "no priority" => Ok(Some(0)),
            "urgent" => Ok(Some(1)),
            "high" => Ok(Some(2)),
            "normal" | "medium" => Ok(Some(3)),
            "low" => Ok(Some(4)),
            other => match other.parse::<i32>() {
                Ok(value @ 0..=4) => Ok(Some(value)),
                _ => Err(Error::InvalidData(format!(
                    "Unsupported Linear priority '{priority}'. Expected urgent/high/normal/low or 0-4"
                ))),
            },
        }
    }
}

fn parse_linear_identifier(key: &str) -> Option<(&str, i64)> {
    let (prefix, number) = key.rsplit_once('-')?;
    let number = number.parse().ok()?;
    if prefix.is_empty() {
        return None;
    }
    Some((prefix, number))
}

fn looks_like_uuid(key: &str) -> bool {
    let mut hex_count = 0usize;
    let mut hyphen_count = 0usize;
    for ch in key.chars() {
        if ch == '-' {
            hyphen_count += 1;
        } else if ch.is_ascii_hexdigit() {
            hex_count += 1;
        } else {
            return false;
        }
    }
    hyphen_count == 4 && hex_count >= 32
}

fn map_user(user: Option<&LinearUser>) -> Option<User> {
    user.map(|u| User {
        id: u.id.clone(),
        username: u.display_name.clone().unwrap_or_else(|| u.name.clone()),
        name: Some(u.name.clone()),
        email: u.email.clone(),
        avatar_url: u.avatar_url.clone(),
    })
}

fn map_priority(priority: Option<i32>) -> Option<String> {
    priority.and_then(|p| match p {
        0 => None,
        1 => Some("urgent".to_string()),
        2 => Some("high".to_string()),
        3 => Some("normal".to_string()),
        4 => Some("low".to_string()),
        other => Some(other.to_string()),
    })
}

fn map_issue(issue: &LinearIssue) -> Issue {
    Issue {
        custom_fields: std::collections::HashMap::new(),
        key: if issue.identifier.is_empty() {
            format!("linear#{}", issue.id)
        } else {
            issue.identifier.clone()
        },
        title: issue.title.clone(),
        description: issue.description.clone(),
        state: issue
            .state
            .as_ref()
            .map(|state| state.name.clone())
            .filter(|name| !name.is_empty())
            .or_else(|| issue.state.as_ref().and_then(|state| state.r#type.clone()))
            .unwrap_or_else(|| "unknown".to_string()),
        source: "linear".to_string(),
        priority: map_priority(issue.priority),
        labels: issue
            .labels
            .nodes
            .iter()
            .map(|label| label.name.clone())
            .collect(),
        author: None,
        assignees: map_user(issue.assignee.as_ref()).into_iter().collect(),
        url: issue.url.clone(),
        created_at: issue.created_at.clone(),
        updated_at: issue.updated_at.clone(),
        attachments_count: None,
        parent: issue
            .parent
            .as_ref()
            .map(|parent| parent.identifier.clone()),
        subtasks: Vec::new(),
    }
}

fn map_comment(comment: &LinearComment) -> Comment {
    Comment {
        id: comment.id.clone(),
        body: comment.body.clone().unwrap_or_default(),
        author: map_user(comment.user.as_ref()),
        created_at: comment.created_at.clone(),
        updated_at: comment.updated_at.clone(),
        position: None,
    }
}

fn map_state_category(category: &str) -> Option<&'static str> {
    match category {
        "backlog" => Some("backlog"),
        "todo" => Some("unstarted"),
        "in_progress" => Some("started"),
        "done" => Some("completed"),
        "cancelled" => Some("canceled"),
        _ => None,
    }
}

fn build_issue_filter(team_id: &str, filter: &IssueFilter) -> Result<Value> {
    if filter.native_query.is_some() {
        return Err(Error::ProviderUnsupported {
            provider: "linear".to_string(),
            operation: "get_issues(native_query)".to_string(),
        });
    }

    let mut clauses = vec![json!({
        "team": {
            "id": {
                "eq": team_id
            }
        }
    })];

    if let Some(state) = filter
        .state
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        match state.to_ascii_lowercase().as_str() {
            "open" | "opened" => clauses.push(json!({
                "state": {
                    "type": {
                        "nin": ["completed", "canceled"]
                    }
                }
            })),
            "closed" => clauses.push(json!({
                "state": {
                    "type": {
                        "in": ["completed", "canceled"]
                    }
                }
            })),
            "all" => {}
            _ => clauses.push(json!({
                "state": {
                    "name": {
                        "eqIgnoreCase": state
                    }
                }
            })),
        }
    }

    if let Some(category) = filter
        .state_category
        .as_deref()
        .and_then(map_state_category)
    {
        clauses.push(json!({
            "state": {
                "type": {
                    "eq": category
                }
            }
        }));
    }

    if let Some(search) = filter
        .search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        clauses.push(json!({
            "or": [
                {
                    "title": {
                        "containsIgnoreCase": search
                    }
                },
                {
                    "description": {
                        "containsIgnoreCase": search
                    }
                }
            ]
        }));
    }

    if let Some(labels) = filter.labels.as_ref().filter(|labels| !labels.is_empty()) {
        if matches!(filter.labels_operator.as_deref(), Some("and")) {
            clauses.push(json!({
                "and": labels.iter().map(|label| json!({
                    "labels": {
                        "name": {
                            "eq": label
                        }
                    }
                })).collect::<Vec<_>>()
            }));
        } else {
            clauses.push(json!({
                "labels": {
                    "name": {
                        "in": labels
                    }
                }
            }));
        }
    }

    if let Some(assignee) = filter
        .assignee
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        clauses.push(json!({
            "or": [
                {
                    "assignee": {
                        "name": {
                            "eqIgnoreCase": assignee
                        }
                    }
                },
                {
                    "assignee": {
                        "displayName": {
                            "eqIgnoreCase": assignee
                        }
                    }
                },
                {
                    "assignee": {
                        "email": {
                            "eqIgnoreCase": assignee
                        }
                    }
                }
            ]
        }));
    }

    if clauses.len() == 1 {
        return Ok(clauses.remove(0));
    }

    let mut root = Map::new();
    root.insert("and".to_string(), Value::Array(clauses));
    Ok(Value::Object(root))
}

#[async_trait]
impl IssueProvider for LinearClient {
    async fn get_issues(&self, filter: IssueFilter) -> Result<ProviderResult<Issue>> {
        let offset = filter.offset.unwrap_or(0);
        let limit = filter.limit.unwrap_or(50).max(1);
        let total_needed = offset.saturating_add(limit);
        let gql_filter = build_issue_filter(&self.team_id, &filter)?;

        let mut after: Option<String> = None;
        let mut fetched = 0u32;
        let mut issues = Vec::new();
        let mut has_more = false;
        let mut next_cursor = None;

        while fetched < total_needed {
            let remaining = total_needed.saturating_sub(fetched).max(1);
            let page_size = remaining.min(100);
            let data = self
                .list_issues_page(page_size, after.as_deref(), gql_filter.clone())
                .await?;

            let page_info = data.issues.page_info;
            let page_nodes = data.issues.nodes;
            if page_nodes.is_empty() {
                has_more = false;
                next_cursor = None;
                break;
            }

            for issue in page_nodes {
                if fetched >= offset && issues.len() < limit as usize {
                    issues.push(map_issue(&issue));
                }
                fetched = fetched.saturating_add(1);
                if fetched >= total_needed {
                    break;
                }
            }

            has_more = page_info.has_next_page;
            next_cursor = page_info.end_cursor.clone();
            if !has_more {
                break;
            }
            after = page_info.end_cursor;
        }

        Ok(ProviderResult {
            items: issues,
            pagination: Some(Pagination {
                offset,
                limit,
                total: None,
                has_more,
                next_cursor,
            }),
            sort_info: Some(SortInfo {
                sort_by: filter.sort_by.clone(),
                sort_order: match filter.sort_order.as_deref() {
                    Some("asc") => SortOrder::Asc,
                    _ => SortOrder::Desc,
                },
                available_sorts: vec![
                    "created_at".to_string(),
                    "updated_at".to_string(),
                    "priority".to_string(),
                ],
            }),
        })
    }

    async fn get_issue(&self, key: &str) -> Result<Issue> {
        let issue = if looks_like_uuid(key) {
            self.get_issue_by_native_id(key).await?
        } else {
            self.get_issue_by_identifier(key).await?
        };

        issue.ok_or_else(|| Error::NotFound(format!("Linear issue not found: {key}")))
    }

    async fn create_issue(&self, input: CreateIssueInput) -> Result<Issue> {
        let assignee_id = match input.assignees.first() {
            Some(assignee) => Some(self.resolve_assignee_id(assignee).await?),
            None => None,
        };
        let label_ids = self.resolve_label_ids(&input.labels).await?;
        let parent_id = match input.parent.as_deref() {
            Some(parent) => Some(self.resolve_parent_id(parent).await?),
            None => None,
        };
        let priority = Self::map_create_priority(input.priority.as_deref())?;

        let mut payload = Map::new();
        payload.insert("teamId".to_string(), Value::String(self.team_id.clone()));
        payload.insert("title".to_string(), Value::String(input.title));

        if let Some(description) = input.description {
            payload.insert("description".to_string(), Value::String(description));
        }
        if let Some(priority) = priority {
            payload.insert("priority".to_string(), Value::Number(priority.into()));
        }
        if let Some(assignee_id) = assignee_id {
            payload.insert("assigneeId".to_string(), Value::String(assignee_id));
        }
        if let Some(parent_id) = parent_id {
            payload.insert("parentId".to_string(), Value::String(parent_id));
        }
        if let Some(project_id) = input.project_id {
            payload.insert("projectId".to_string(), Value::String(project_id));
        }
        if !label_ids.is_empty() {
            payload.insert(
                "labelIds".to_string(),
                Value::Array(label_ids.into_iter().map(Value::String).collect()),
            );
        }

        let data: LinearIssueCreateData = self
            .graphql(
                ISSUE_CREATE_MUTATION,
                json!({
                    "input": Value::Object(payload),
                }),
                &self.token,
            )
            .await?;
        if !data.issue_create.success {
            return Err(Error::Api {
                status: 200,
                message: "Linear issueCreate returned success=false".to_string(),
            });
        }

        let issue = data.issue_create.issue.ok_or_else(|| {
            Error::InvalidData("Linear issueCreate returned no issue payload".to_string())
        })?;
        Ok(map_issue(&issue))
    }

    async fn update_issue(&self, key: &str, input: UpdateIssueInput) -> Result<Issue> {
        let issue_id = if looks_like_uuid(key) {
            key.to_string()
        } else {
            self.get_linear_issue_by_identifier(key)
                .await?
                .ok_or_else(|| Error::NotFound(format!("Linear issue not found: {key}")))?
                .id
        };

        let assignee_id = match input.assignees.as_ref() {
            Some(assignees) if assignees.is_empty() => Some(Value::Null),
            Some(assignees) => Some(Value::String(
                self.resolve_assignee_id(&assignees[0]).await?,
            )),
            None => None,
        };
        let label_ids = match input.labels.as_ref() {
            Some(labels) => Some(
                self.resolve_label_ids(labels)
                    .await?
                    .into_iter()
                    .map(Value::String)
                    .collect::<Vec<_>>(),
            ),
            None => None,
        };
        let parent_id = match input.parent_id.as_deref() {
            Some("none") | Some("") => Some(Value::Null),
            Some(parent) => Some(Value::String(self.resolve_parent_id(parent).await?)),
            None => None,
        };
        let priority = Self::map_create_priority(input.priority.as_deref())?;
        let state_id = match input.status.as_deref().or(input.state.as_deref()) {
            Some(state) => Some(self.resolve_workflow_state_id(state).await?),
            None => None,
        };

        let mut payload = Map::new();
        if let Some(title) = input.title {
            payload.insert("title".to_string(), Value::String(title));
        }
        if let Some(description) = input.description {
            payload.insert("description".to_string(), Value::String(description));
        }
        if let Some(priority) = priority {
            payload.insert("priority".to_string(), Value::Number(priority.into()));
        }
        if let Some(assignee_id) = assignee_id {
            payload.insert("assigneeId".to_string(), assignee_id);
        }
        if let Some(parent_id) = parent_id {
            payload.insert("parentId".to_string(), parent_id);
        }
        if let Some(label_ids) = label_ids {
            payload.insert("labelIds".to_string(), Value::Array(label_ids));
        }
        if let Some(state_id) = state_id {
            payload.insert("stateId".to_string(), Value::String(state_id));
        }

        if payload.is_empty() {
            return self.get_issue(key).await;
        }

        let data: LinearIssueUpdateData = self
            .graphql(
                ISSUE_UPDATE_MUTATION,
                json!({
                    "id": issue_id,
                    "input": Value::Object(payload),
                }),
                &self.token,
            )
            .await?;
        if !data.issue_update.success {
            return Err(Error::Api {
                status: 200,
                message: "Linear issueUpdate returned success=false".to_string(),
            });
        }

        let issue = data.issue_update.issue.ok_or_else(|| {
            Error::InvalidData("Linear issueUpdate returned no issue payload".to_string())
        })?;
        Ok(map_issue(&issue))
    }

    async fn get_comments(&self, issue_key: &str) -> Result<ProviderResult<Comment>> {
        let issue_id = if looks_like_uuid(issue_key) {
            issue_key.to_string()
        } else {
            self.get_linear_issue_by_identifier(issue_key)
                .await?
                .ok_or_else(|| Error::NotFound(format!("Linear issue not found: {issue_key}")))?
                .id
        };

        let mut after: Option<String> = None;
        let mut comments = Vec::new();

        loop {
            let data: LinearIssueCommentsData = self
                .graphql(
                    ISSUE_COMMENTS_QUERY,
                    json!({
                        "id": issue_id,
                        "first": 100,
                        "after": after,
                    }),
                    &self.token,
                )
                .await?;
            let issue = data.issue.ok_or_else(|| {
                Error::NotFound(format!(
                    "Linear issue not found when fetching comments: {issue_key}"
                ))
            })?;

            let page_info = issue.comments.page_info;
            comments.extend(issue.comments.nodes.iter().map(map_comment));

            if !page_info.has_next_page {
                break;
            }
            after = page_info.end_cursor;
            if after.is_none() {
                break;
            }
        }

        Ok(comments.into())
    }

    async fn add_comment(&self, issue_key: &str, body: &str) -> Result<Comment> {
        let issue_id = if looks_like_uuid(issue_key) {
            issue_key.to_string()
        } else {
            self.get_linear_issue_by_identifier(issue_key)
                .await?
                .ok_or_else(|| Error::NotFound(format!("Linear issue not found: {issue_key}")))?
                .id
        };

        let data: LinearCommentCreateData = self
            .graphql(
                COMMENT_CREATE_MUTATION,
                json!({
                    "input": {
                        "issueId": issue_id,
                        "body": body,
                    }
                }),
                &self.token,
            )
            .await?;
        if !data.comment_create.success {
            return Err(Error::Api {
                status: 200,
                message: "Linear commentCreate returned success=false".to_string(),
            });
        }

        let comment = data.comment_create.comment.ok_or_else(|| {
            Error::InvalidData("Linear commentCreate returned no comment payload".to_string())
        })?;
        Ok(map_comment(&comment))
    }

    fn provider_name(&self) -> &'static str {
        "linear"
    }
}

#[async_trait]
impl MergeRequestProvider for LinearClient {
    fn provider_name(&self) -> &'static str {
        "linear"
    }
}

#[async_trait]
impl PipelineProvider for LinearClient {
    fn provider_name(&self) -> &'static str {
        "linear"
    }
}

#[async_trait]
impl Provider for LinearClient {
    async fn get_current_user(&self) -> Result<User> {
        let viewer = self.viewer_with_token(&self.token).await?;
        Ok(User {
            id: viewer.id,
            username: viewer
                .display_name
                .clone()
                .unwrap_or_else(|| viewer.name.clone()),
            name: Some(viewer.name),
            email: viewer.email,
            avatar_url: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use serde_json::json;

    fn linear_issue(identifier: &str, title: &str, state: &str) -> Value {
        json!({
            "id": format!("id-{identifier}"),
            "identifier": identifier,
            "title": title,
            "description": format!("Description for {identifier}"),
            "priority": 2,
            "url": format!("https://linear.app/acme/issue/{identifier}/{}", title.replace(' ', "-").to_lowercase()),
            "createdAt": "2026-05-01T10:00:00.000Z",
            "updatedAt": "2026-05-02T10:00:00.000Z",
            "state": {
                "name": state,
                "type": "started"
            },
            "labels": {
                "nodes": [
                    { "name": "bug" }
                ]
            },
            "assignee": {
                "id": "u1",
                "name": "Alice Doe",
                "displayName": "alice",
                "email": "alice@example.com",
                "avatarUrl": "https://example.com/alice.png"
            },
            "parent": {
                "identifier": "ENG-1"
            }
        })
    }

    fn linear_comment(id: &str, body: &str) -> Value {
        json!({
            "id": id,
            "body": body,
            "createdAt": "2026-05-04T10:00:00.000Z",
            "updatedAt": "2026-05-05T10:00:00.000Z",
            "user": {
                "id": "u1",
                "name": "Alice Doe",
                "displayName": "alice",
                "email": "alice@example.com",
                "avatarUrl": "https://example.com/alice.png"
            }
        })
    }

    #[tokio::test]
    async fn get_current_user_reads_viewer() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("query Viewer");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "data": {
                        "viewer": {
                            "id": "u1",
                            "name": "Alice",
                            "displayName": "alice",
                            "email": "alice@example.com"
                        }
                    }
                }));
        });

        let client = LinearClient::with_base_url(
            format!("{}/graphql", server.base_url()),
            "team-1",
            SecretString::from("lin_api_test".to_owned()),
        );

        let user = client.get_current_user().await.unwrap();
        assert_eq!(user.id, "u1");
        assert_eq!(user.username, "alice");
        assert_eq!(user.name.as_deref(), Some("Alice"));
        assert_eq!(user.email.as_deref(), Some("alice@example.com"));
        mock.assert();
    }

    #[tokio::test]
    async fn get_issue_by_identifier_uses_team_scoped_issue_filter() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("query Issues")
                .body_includes(r#""first":1"#)
                .body_includes(r#""team":{"id":{"eq":"team-1"}}"#)
                .body_includes(r#""number":{"eq":42}"#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "data": {
                        "issues": {
                            "nodes": [
                                linear_issue("ENG-42", "Fix login", "In Progress")
                            ],
                            "pageInfo": {
                                "hasNextPage": false,
                                "endCursor": null
                            }
                        }
                    }
                }));
        });

        let client = LinearClient::with_base_url(
            format!("{}/graphql", server.base_url()),
            "team-1",
            SecretString::from("lin_api_test".to_owned()),
        );

        let issue = client.get_issue("ENG-42").await.unwrap();
        assert_eq!(issue.key, "ENG-42");
        assert_eq!(issue.title, "Fix login");
        assert_eq!(issue.state, "In Progress");
        assert_eq!(issue.priority.as_deref(), Some("high"));
        assert_eq!(issue.labels, vec!["bug".to_string()]);
        assert_eq!(issue.parent.as_deref(), Some("ENG-1"));
        assert_eq!(issue.assignees.len(), 1);
        assert_eq!(issue.assignees[0].username, "alice");

        mock.assert();
    }

    #[tokio::test]
    async fn get_issue_by_native_id_uses_issue_query() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("query IssueById")
                .body_includes(r#""id":"3d1b0f7a-8f3a-4b2a-9c1a-2b6a0c4b9a11""#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "data": {
                        "issue": linear_issue("ENG-7", "Fetch by uuid", "Backlog")
                    }
                }));
        });

        let client = LinearClient::with_base_url(
            format!("{}/graphql", server.base_url()),
            "team-1",
            SecretString::from("lin_api_test".to_owned()),
        );

        let uuid = "3d1b0f7a-8f3a-4b2a-9c1a-2b6a0c4b9a11";
        let issue = client.get_issue(uuid).await.unwrap();
        assert_eq!(issue.key, "ENG-7");

        mock.assert();
    }

    #[tokio::test]
    async fn get_issues_applies_filters_and_reports_pagination() {
        let server = MockServer::start();
        let page_1 = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("query Issues")
                .body_includes(r#""first":3"#)
                .body_includes(r#""state":{"type":{"nin":["completed","canceled"]}}"#)
                .body_includes(r#""title":{"containsIgnoreCase":"login"}"#)
                .body_includes(r#""labels":{"name":{"eq":"bug"}}"#)
                .body_includes(r#""displayName":{"eqIgnoreCase":"alice"}"#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "data": {
                        "issues": {
                            "nodes": [
                                linear_issue("ENG-1", "One", "Backlog"),
                                linear_issue("ENG-2", "Two", "In Progress"),
                                linear_issue("ENG-3", "Three", "In Progress")
                            ],
                            "pageInfo": {
                                "hasNextPage": true,
                                "endCursor": "cursor-1"
                            }
                        }
                    }
                }));
        });
        let page_2 = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes(r#""after":"cursor-1""#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "data": {
                        "issues": {
                            "nodes": [
                                linear_issue("ENG-4", "Four", "Done")
                            ],
                            "pageInfo": {
                                "hasNextPage": false,
                                "endCursor": "cursor-2"
                            }
                        }
                    }
                }));
        });

        let client = LinearClient::with_base_url(
            format!("{}/graphql", server.base_url()),
            "team-1",
            SecretString::from("lin_api_test".to_owned()),
        );

        let result = client
            .get_issues(IssueFilter {
                state: Some("open".to_string()),
                search: Some("login".to_string()),
                labels: Some(vec!["bug".to_string(), "api".to_string()]),
                labels_operator: Some("and".to_string()),
                assignee: Some("alice".to_string()),
                limit: Some(2),
                offset: Some(1),
                sort_by: Some("updated_at".to_string()),
                sort_order: Some("desc".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(result.items.len(), 2);
        assert_eq!(result.items[0].key, "ENG-2");
        assert_eq!(result.items[1].key, "ENG-3");

        let pagination = result.pagination.unwrap();
        assert_eq!(pagination.offset, 1);
        assert_eq!(pagination.limit, 2);
        assert!(pagination.has_more);
        assert_eq!(pagination.next_cursor.as_deref(), Some("cursor-1"));

        page_1.assert();
        assert_eq!(page_2.calls(), 0);
    }

    #[tokio::test]
    async fn create_issue_resolves_inputs_and_returns_created_issue() {
        let server = MockServer::start();
        let users = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("query Users")
                .body_includes(r#""displayName":{"eqIgnoreCase":"alice"}"#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "data": {
                        "users": {
                            "nodes": [
                                {
                                    "id": "user-1",
                                    "name": "Alice Doe",
                                    "displayName": "alice",
                                    "email": "alice@example.com",
                                    "avatarUrl": "https://example.com/alice.png"
                                }
                            ]
                        }
                    }
                }));
        });
        let labels = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("query IssueLabels")
                .body_includes(r#""team":{"id":{"eq":"team-1"}}"#)
                .body_includes(r#""name":{"in":["bug","backend"]}"#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "data": {
                        "issueLabels": {
                            "nodes": [
                                { "id": "label-1", "name": "bug" },
                                { "id": "label-2", "name": "backend" }
                            ]
                        }
                    }
                }));
        });
        let parent = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("query Issues")
                .body_includes(r#""number":{"eq":1}"#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "data": {
                        "issues": {
                            "nodes": [
                                linear_issue("ENG-1", "Parent issue", "Backlog")
                            ],
                            "pageInfo": {
                                "hasNextPage": false,
                                "endCursor": null
                            }
                        }
                    }
                }));
        });
        let create = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("mutation IssueCreate")
                .body_includes(r#""teamId":"team-1""#)
                .body_includes(r#""title":"Create login flow""#)
                .body_includes(r#""description":"Ship the first pass""#)
                .body_includes(r#""priority":1"#)
                .body_includes(r#""assigneeId":"user-1""#)
                .body_includes(r#""parentId":"id-ENG-1""#)
                .body_includes(r#""projectId":"project-1""#)
                .body_includes(r#""labelIds":["label-1","label-2"]"#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "data": {
                        "issueCreate": {
                            "success": true,
                            "issue": linear_issue("ENG-99", "Create login flow", "Backlog")
                        }
                    }
                }));
        });

        let client = LinearClient::with_base_url(
            format!("{}/graphql", server.base_url()),
            "team-1",
            SecretString::from("lin_api_test".to_owned()),
        );

        let issue = client
            .create_issue(CreateIssueInput {
                title: "Create login flow".to_string(),
                description: Some("Ship the first pass".to_string()),
                labels: vec!["bug".to_string(), "backend".to_string()],
                assignees: vec!["alice".to_string()],
                priority: Some("urgent".to_string()),
                parent: Some("ENG-1".to_string()),
                project_id: Some("project-1".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(issue.key, "ENG-99");
        assert_eq!(issue.title, "Create login flow");
        assert_eq!(issue.priority.as_deref(), Some("high"));
        assert_eq!(issue.parent.as_deref(), Some("ENG-1"));
        assert_eq!(issue.assignees.len(), 1);
        assert_eq!(issue.assignees[0].username, "alice");
        assert_eq!(issue.labels, vec!["bug".to_string()]);

        users.assert();
        labels.assert();
        parent.assert();
        create.assert();
    }

    #[tokio::test]
    async fn update_issue_resolves_status_and_clears_optional_fields() {
        let server = MockServer::start();
        let issue_lookup = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("query Issues")
                .body_includes(r#""number":{"eq":42}"#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "data": {
                        "issues": {
                            "nodes": [
                                linear_issue("ENG-42", "Original issue", "Backlog")
                            ],
                            "pageInfo": {
                                "hasNextPage": false,
                                "endCursor": null
                            }
                        }
                    }
                }));
        });
        let workflow_states = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("query WorkflowStates")
                .body_includes(r#""name":{"eqIgnoreCase":"in review"}"#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "data": {
                        "workflowStates": {
                            "nodes": [
                                {
                                    "id": "workflow-2",
                                    "name": "In Review",
                                    "type": "started"
                                }
                            ]
                        }
                    }
                }));
        });
        let update = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("mutation IssueUpdate")
                .body_includes(r#""id":"id-ENG-42""#)
                .body_includes(r#""title":"Updated issue title""#)
                .body_includes(r#""description":"Updated description""#)
                .body_includes(r#""priority":4"#)
                .body_includes(r#""assigneeId":null"#)
                .body_includes(r#""parentId":null"#)
                .body_includes(r#""labelIds":[]"#)
                .body_includes(r#""stateId":"workflow-2""#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "data": {
                        "issueUpdate": {
                            "success": true,
                            "issue": {
                                "id": "id-ENG-42",
                                "identifier": "ENG-42",
                                "title": "Updated issue title",
                                "description": "Updated description",
                                "priority": 4,
                                "url": "https://linear.app/acme/issue/ENG-42/updated-issue-title",
                                "createdAt": "2026-05-01T10:00:00.000Z",
                                "updatedAt": "2026-05-03T10:00:00.000Z",
                                "state": {
                                    "name": "In Review",
                                    "type": "started"
                                },
                                "labels": {
                                    "nodes": []
                                },
                                "assignee": null,
                                "parent": null
                            }
                        }
                    }
                }));
        });

        let client = LinearClient::with_base_url(
            format!("{}/graphql", server.base_url()),
            "team-1",
            SecretString::from("lin_api_test".to_owned()),
        );

        let issue = client
            .update_issue(
                "ENG-42",
                UpdateIssueInput {
                    title: Some("Updated issue title".to_string()),
                    description: Some("Updated description".to_string()),
                    state: Some("closed".to_string()),
                    status: Some("In Review".to_string()),
                    labels: Some(vec![]),
                    assignees: Some(vec![]),
                    priority: Some("low".to_string()),
                    parent_id: Some("none".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(issue.key, "ENG-42");
        assert_eq!(issue.title, "Updated issue title");
        assert_eq!(issue.description.as_deref(), Some("Updated description"));
        assert_eq!(issue.state, "In Review");
        assert_eq!(issue.priority.as_deref(), Some("low"));
        assert!(issue.labels.is_empty());
        assert!(issue.assignees.is_empty());
        assert_eq!(issue.parent, None);

        issue_lookup.assert();
        workflow_states.assert();
        update.assert();
    }

    #[tokio::test]
    async fn get_comments_paginates_and_maps_authors() {
        let server = MockServer::start();
        let issue_lookup = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("query Issues")
                .body_includes(r#""number":{"eq":42}"#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "data": {
                        "issues": {
                            "nodes": [
                                linear_issue("ENG-42", "Issue for comments", "Backlog")
                            ],
                            "pageInfo": {
                                "hasNextPage": false,
                                "endCursor": null
                            }
                        }
                    }
                }));
        });
        let comments_page_1 = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("query IssueComments")
                .body_includes(r#""id":"id-ENG-42""#)
                .body_includes(r#""first":100"#)
                .body_includes(r#""after":null"#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "data": {
                        "issue": {
                            "comments": {
                                "nodes": [
                                    linear_comment("comment-1", "First comment")
                                ],
                                "pageInfo": {
                                    "hasNextPage": true,
                                    "endCursor": "cursor-1"
                                }
                            }
                        }
                    }
                }));
        });
        let comments_page_2 = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes(r#""after":"cursor-1""#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "data": {
                        "issue": {
                            "comments": {
                                "nodes": [
                                    linear_comment("comment-2", "Second comment")
                                ],
                                "pageInfo": {
                                    "hasNextPage": false,
                                    "endCursor": "cursor-2"
                                }
                            }
                        }
                    }
                }));
        });

        let client = LinearClient::with_base_url(
            format!("{}/graphql", server.base_url()),
            "team-1",
            SecretString::from("lin_api_test".to_owned()),
        );

        let comments = client.get_comments("ENG-42").await.unwrap();
        assert_eq!(comments.items.len(), 2);
        assert_eq!(comments.items[0].id, "comment-1");
        assert_eq!(comments.items[0].body, "First comment");
        assert_eq!(
            comments.items[0]
                .author
                .as_ref()
                .map(|u| u.username.as_str()),
            Some("alice")
        );
        assert_eq!(comments.items[1].id, "comment-2");

        issue_lookup.assert();
        comments_page_1.assert();
        comments_page_2.assert();
    }

    #[tokio::test]
    async fn add_comment_resolves_issue_and_returns_comment() {
        let server = MockServer::start();
        let issue_lookup = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("query Issues")
                .body_includes(r#""number":{"eq":42}"#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "data": {
                        "issues": {
                            "nodes": [
                                linear_issue("ENG-42", "Issue for add comment", "Backlog")
                            ],
                            "pageInfo": {
                                "hasNextPage": false,
                                "endCursor": null
                            }
                        }
                    }
                }));
        });
        let create = server.mock(|when, then| {
            when.method(POST)
                .path("/graphql")
                .body_includes("mutation CommentCreate")
                .body_includes(r#""issueId":"id-ENG-42""#)
                .body_includes(r#""body":"Need a follow-up here.""#);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "data": {
                        "commentCreate": {
                            "success": true,
                            "comment": linear_comment("comment-3", "Need a follow-up here.")
                        }
                    }
                }));
        });

        let client = LinearClient::with_base_url(
            format!("{}/graphql", server.base_url()),
            "team-1",
            SecretString::from("lin_api_test".to_owned()),
        );

        let comment =
            devboy_core::IssueProvider::add_comment(&client, "ENG-42", "Need a follow-up here.")
                .await
                .unwrap();
        assert_eq!(comment.id, "comment-3");
        assert_eq!(comment.body, "Need a follow-up here.");
        assert_eq!(
            comment.author.as_ref().map(|u| u.username.as_str()),
            Some("alice")
        );

        issue_lookup.assert();
        create.assert();
    }
}
