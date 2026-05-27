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
    GraphQlResponse, LinearIssue, LinearIssueData, LinearIssuesData, LinearUser, Viewer, ViewerData,
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

    fn unsupported<T>(&self, operation: &str) -> Result<T> {
        Err(Error::ProviderUnsupported {
            provider: "linear".to_string(),
            operation: operation.to_string(),
        })
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
        let data: LinearIssueData = self
            .graphql(ISSUE_BY_ID_QUERY, json!({ "id": id }), &self.token)
            .await?;
        Ok(data.issue.as_ref().map(map_issue))
    }

    async fn get_issue_by_identifier(&self, identifier: &str) -> Result<Option<Issue>> {
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
        Ok(data.issues.nodes.first().map(map_issue))
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

    async fn create_issue(&self, _input: CreateIssueInput) -> Result<Issue> {
        self.unsupported("create_issue")
    }

    async fn update_issue(&self, _key: &str, _input: UpdateIssueInput) -> Result<Issue> {
        self.unsupported("update_issue")
    }

    async fn get_comments(&self, _issue_key: &str) -> Result<ProviderResult<Comment>> {
        self.unsupported("get_comments")
    }

    async fn add_comment(&self, _issue_key: &str, _body: &str) -> Result<Comment> {
        self.unsupported("add_comment")
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
}
