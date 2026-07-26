use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct GraphQlResponse<T> {
    pub data: Option<T>,
    #[serde(default)]
    pub errors: Vec<GraphQlError>,
}

#[derive(Debug, Deserialize)]
pub struct GraphQlError {
    pub message: String,
    #[serde(default)]
    pub extensions: Option<GraphQlErrorExtensions>,
}

#[derive(Debug, Deserialize)]
pub struct GraphQlErrorExtensions {
    #[serde(default)]
    pub code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ViewerData {
    pub viewer: Viewer,
}

#[derive(Debug, Deserialize)]
pub struct Viewer {
    pub id: String,
    pub name: String,
    #[serde(rename = "displayName")]
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LinearIssueData {
    pub issue: Option<LinearIssue>,
}

#[derive(Debug, Deserialize)]
pub struct LinearIssueCreateData {
    #[serde(rename = "issueCreate")]
    pub issue_create: LinearIssueCreatePayload,
}

#[derive(Debug, Deserialize)]
pub struct LinearIssueCreatePayload {
    pub success: bool,
    pub issue: Option<LinearIssue>,
}

#[derive(Debug, Deserialize)]
pub struct LinearIssueUpdateData {
    #[serde(rename = "issueUpdate")]
    pub issue_update: LinearIssueUpdatePayload,
}

#[derive(Debug, Deserialize)]
pub struct LinearIssueUpdatePayload {
    pub success: bool,
    pub issue: Option<LinearIssue>,
}

#[derive(Debug, Deserialize)]
pub struct LinearIssueCommentsData {
    pub issue: Option<LinearIssueComments>,
}

#[derive(Debug, Deserialize)]
pub struct LinearIssueComments {
    pub comments: LinearCommentConnection,
}

#[derive(Debug, Deserialize)]
pub struct LinearCommentConnection {
    #[serde(default)]
    pub nodes: Vec<LinearComment>,
    #[serde(rename = "pageInfo")]
    pub page_info: LinearPageInfo,
}

#[derive(Debug, Deserialize)]
pub struct LinearCommentCreateData {
    #[serde(rename = "commentCreate")]
    pub comment_create: LinearCommentCreatePayload,
}

#[derive(Debug, Deserialize)]
pub struct LinearCommentCreatePayload {
    pub success: bool,
    pub comment: Option<LinearComment>,
}

#[derive(Debug, Deserialize)]
pub struct LinearIssuesData {
    pub issues: LinearIssueConnection,
}

#[derive(Debug, Deserialize)]
pub struct LinearUsersData {
    pub users: LinearUserConnection,
}

#[derive(Debug, Deserialize)]
pub struct LinearUserConnection {
    #[serde(default)]
    pub nodes: Vec<LinearUser>,
}

#[derive(Debug, Deserialize)]
pub struct LinearIssueLabelsData {
    #[serde(rename = "issueLabels")]
    pub issue_labels: LinearLabelConnectionWithIds,
}

#[derive(Debug, Deserialize)]
pub struct LinearWorkflowStatesData {
    #[serde(rename = "workflowStates")]
    pub workflow_states: LinearWorkflowStateConnection,
}

#[derive(Debug, Deserialize, Default)]
pub struct LinearWorkflowStateConnection {
    #[serde(default)]
    pub nodes: Vec<LinearWorkflowStateWithId>,
}

#[derive(Debug, Deserialize)]
pub struct LinearWorkflowStateWithId {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub r#type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LinearComment {
    pub id: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(rename = "createdAt")]
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(rename = "updatedAt")]
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub user: Option<LinearUser>,
}

#[derive(Debug, Deserialize, Default)]
pub struct LinearLabelConnectionWithIds {
    #[serde(default)]
    pub nodes: Vec<LinearLabelWithId>,
}

#[derive(Debug, Deserialize)]
pub struct LinearLabelWithId {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct LinearIssueConnection {
    #[serde(default)]
    pub nodes: Vec<LinearIssue>,
    #[serde(rename = "pageInfo")]
    pub page_info: LinearPageInfo,
}

#[derive(Debug, Deserialize)]
pub struct LinearPageInfo {
    #[serde(rename = "hasNextPage")]
    pub has_next_page: bool,
    #[serde(rename = "endCursor")]
    #[serde(default)]
    pub end_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LinearIssue {
    pub id: String,
    pub identifier: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(rename = "createdAt")]
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(rename = "updatedAt")]
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub state: Option<LinearWorkflowState>,
    #[serde(default)]
    pub labels: LinearLabelConnection,
    #[serde(default)]
    pub assignee: Option<LinearUser>,
    #[serde(default)]
    pub parent: Option<LinearIssueParent>,
    #[serde(default)]
    pub team: Option<LinearTeamRef>,
}

#[derive(Debug, Deserialize)]
pub struct LinearWorkflowState {
    pub name: String,
    #[serde(default)]
    pub r#type: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct LinearLabelConnection {
    #[serde(default)]
    pub nodes: Vec<LinearLabel>,
}

#[derive(Debug, Deserialize)]
pub struct LinearLabel {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct LinearUser {
    pub id: String,
    pub name: String,
    #[serde(rename = "displayName")]
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(rename = "avatarUrl")]
    #[serde(default)]
    pub avatar_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LinearIssueParent {
    pub identifier: String,
}

#[derive(Debug, Deserialize)]
pub struct LinearTeamRef {
    pub id: String,
}
