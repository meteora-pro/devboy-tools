//! Jira API response types.
//!
//! These types represent the raw JSON responses from Jira API v2/v3.
//! They are deserialized and then mapped to unified types.

use serde::{Deserialize, Serialize};

// =============================================================================
// User
// =============================================================================

/// Jira user representation.
#[derive(Debug, Clone, Deserialize)]
pub struct JiraUser {
    /// Account ID (Cloud only)
    #[serde(default, rename = "accountId")]
    pub account_id: Option<String>,
    /// Username (Self-Hosted only)
    #[serde(default)]
    pub name: Option<String>,
    /// Display name
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
    /// Email address
    #[serde(default, rename = "emailAddress")]
    pub email_address: Option<String>,
}

// =============================================================================
// Issue
// =============================================================================

/// Jira issue representation.
#[derive(Debug, Clone, Deserialize)]
pub struct JiraIssue {
    /// Issue ID
    pub id: String,
    /// Issue key (e.g., "PROJ-123")
    pub key: String,
    /// Issue fields
    pub fields: JiraIssueFields,
}

/// Jira issue fields.
#[derive(Debug, Clone, Deserialize)]
pub struct JiraIssueFields {
    /// Summary (title)
    #[serde(default)]
    pub summary: Option<String>,
    /// Description — plain text (v2) or ADF document (v3)
    #[serde(default)]
    pub description: Option<serde_json::Value>,
    /// Status
    #[serde(default)]
    pub status: Option<JiraStatus>,
    /// Priority
    #[serde(default)]
    pub priority: Option<JiraPriority>,
    /// Assignee
    #[serde(default)]
    pub assignee: Option<JiraUser>,
    /// Reporter (author)
    #[serde(default)]
    pub reporter: Option<JiraUser>,
    /// Labels
    #[serde(default)]
    pub labels: Vec<String>,
    /// Created timestamp
    #[serde(default)]
    pub created: Option<String>,
    /// Updated timestamp
    #[serde(default)]
    pub updated: Option<String>,
}

/// Jira issue status.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraStatus {
    /// Status name
    pub name: String,
    /// Status category (new, indeterminate, done)
    #[serde(default)]
    pub status_category: Option<JiraStatusCategory>,
}

/// Jira status category.
#[derive(Debug, Clone, Deserialize)]
pub struct JiraStatusCategory {
    /// Category key: "new", "indeterminate", "done"
    pub key: String,
}

/// Jira issue priority.
#[derive(Debug, Clone, Deserialize)]
pub struct JiraPriority {
    /// Priority name
    pub name: String,
}

// =============================================================================
// Search Response
// =============================================================================

/// Search response from Self-Hosted Jira (API v2, GET /search).
#[derive(Debug, Clone, Deserialize)]
pub struct JiraSearchResponse {
    /// Issues
    pub issues: Vec<JiraIssue>,
    /// Starting index
    #[serde(default, rename = "startAt")]
    pub start_at: Option<u32>,
    /// Max results per page
    #[serde(default, rename = "maxResults")]
    pub max_results: Option<u32>,
    /// Total number of results
    #[serde(default)]
    pub total: Option<u32>,
}

/// Search response from Jira Cloud (API v3, GET /search/jql).
#[derive(Debug, Clone, Deserialize)]
pub struct JiraCloudSearchResponse {
    /// Issues
    pub issues: Vec<JiraIssue>,
    /// Token for next page
    #[serde(default, rename = "nextPageToken")]
    pub next_page_token: Option<String>,
}

// =============================================================================
// Comment
// =============================================================================

/// Jira comment representation.
#[derive(Debug, Clone, Deserialize)]
pub struct JiraComment {
    /// Comment ID
    pub id: String,
    /// Comment body — plain text (v2) or ADF document (v3)
    #[serde(default)]
    pub body: Option<serde_json::Value>,
    /// Comment author
    #[serde(default)]
    pub author: Option<JiraUser>,
    /// Created timestamp
    #[serde(default)]
    pub created: Option<String>,
    /// Updated timestamp
    #[serde(default)]
    pub updated: Option<String>,
}

/// Response from GET /issue/{key}/comment.
#[derive(Debug, Clone, Deserialize)]
pub struct JiraCommentsResponse {
    /// Comments
    pub comments: Vec<JiraComment>,
}

// =============================================================================
// Transitions
// =============================================================================

/// Jira transition representation.
#[derive(Debug, Clone, Deserialize)]
pub struct JiraTransition {
    /// Transition ID
    pub id: String,
    /// Transition name
    pub name: String,
    /// Target status
    pub to: JiraStatus,
}

/// Response from GET /issue/{key}/transitions.
#[derive(Debug, Clone, Deserialize)]
pub struct JiraTransitionsResponse {
    /// Available transitions
    pub transitions: Vec<JiraTransition>,
}

// =============================================================================
// Create/Update types
// =============================================================================

/// Request body for creating an issue.
#[derive(Debug, Clone, Serialize)]
pub struct CreateIssuePayload {
    /// Issue fields
    pub fields: CreateIssueFields,
}

/// Fields for creating an issue.
#[derive(Debug, Clone, Serialize)]
pub struct CreateIssueFields {
    /// Project
    pub project: ProjectKey,
    /// Summary (title)
    pub summary: String,
    /// Issue type
    pub issuetype: IssueType,
    /// Description — plain text (v2) or ADF (v3)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<serde_json::Value>,
    /// Labels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    /// Priority
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<PriorityName>,
    /// Assignee
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<serde_json::Value>,
}

/// Project key reference.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectKey {
    /// Project key (e.g., "PROJ")
    pub key: String,
}

/// Issue type reference.
#[derive(Debug, Clone, Serialize)]
pub struct IssueType {
    /// Issue type name
    pub name: String,
}

/// Priority name reference.
#[derive(Debug, Clone, Serialize)]
pub struct PriorityName {
    /// Priority name
    pub name: String,
}

/// Request body for updating an issue.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateIssuePayload {
    /// Issue fields to update
    pub fields: UpdateIssueFields,
}

/// Fields for updating an issue.
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateIssueFields {
    /// Summary (title)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Description — plain text (v2) or ADF (v3)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<serde_json::Value>,
    /// Labels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    /// Priority
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<PriorityName>,
    /// Assignee
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<serde_json::Value>,
}

/// Request body for transitioning an issue.
#[derive(Debug, Clone, Serialize)]
pub struct TransitionPayload {
    /// Transition to execute
    pub transition: TransitionId,
}

/// Transition ID reference.
#[derive(Debug, Clone, Serialize)]
pub struct TransitionId {
    /// Transition ID
    pub id: String,
}

/// Response from POST /issue (create issue).
#[derive(Debug, Clone, Deserialize)]
pub struct CreateIssueResponse {
    /// Issue ID
    pub id: String,
    /// Issue key (e.g., "PROJ-123")
    pub key: String,
}

/// Request body for adding a comment.
#[derive(Debug, Clone, Serialize)]
pub struct AddCommentPayload {
    /// Comment body — plain text (v2) or ADF (v3)
    pub body: serde_json::Value,
}

// =============================================================================
// Project Statuses
// =============================================================================

/// Response from GET /project/{key}/statuses.
/// Returns statuses grouped by issue type.
#[derive(Debug, Clone, Deserialize)]
pub struct JiraIssueTypeStatuses {
    /// Issue type name (e.g., "Task", "Bug")
    #[serde(default)]
    pub name: Option<String>,
    /// Statuses available for this issue type
    #[serde(default)]
    pub statuses: Vec<JiraProjectStatus>,
}

/// A status within a project, including its category.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraProjectStatus {
    /// Status name
    pub name: String,
    /// Status ID
    #[serde(default)]
    pub id: Option<String>,
    /// Status category
    #[serde(default)]
    pub status_category: Option<JiraStatusCategory>,
}
