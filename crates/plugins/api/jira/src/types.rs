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
    /// Parent issue (for subtasks)
    #[serde(default)]
    pub parent: Option<Box<JiraIssue>>,
    /// Subtasks / child issues
    #[serde(default)]
    pub subtasks: Vec<JiraIssue>,
    /// Issue links
    #[serde(default)]
    pub issuelinks: Vec<JiraIssueLink>,
    /// Attachments on the issue (present when the caller requests
    /// `fields=attachment` or uses `fields=*all`).
    #[serde(default)]
    pub attachment: Vec<JiraAttachment>,
}

/// Jira attachment as returned inside `fields.attachment`.
#[derive(Debug, Clone, Deserialize)]
pub struct JiraAttachment {
    /// Attachment id (numeric string).
    pub id: String,
    /// Original filename.
    #[serde(default)]
    pub filename: Option<String>,
    /// Direct download URL (`content` in the Jira API).
    #[serde(default)]
    pub content: Option<String>,
    /// Size in bytes.
    #[serde(default)]
    pub size: Option<u64>,
    /// MIME type.
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
    /// Creation timestamp (ISO 8601).
    #[serde(default)]
    pub created: Option<String>,
    /// Author — Jira uses `author` inside the attachment object.
    #[serde(default)]
    pub author: Option<JiraUser>,
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
// Issue Links
// =============================================================================

/// Jira issue link representation.
#[derive(Debug, Clone, Deserialize)]
pub struct JiraIssueLink {
    /// Link ID
    #[serde(default)]
    pub id: Option<String>,
    /// Link type
    #[serde(rename = "type")]
    pub link_type: JiraIssueLinkType,
    /// Inward issue (e.g., "is blocked by" this issue)
    #[serde(default, rename = "inwardIssue")]
    pub inward_issue: Option<Box<JiraIssue>>,
    /// Outward issue (e.g., "blocks" this issue)
    #[serde(default, rename = "outwardIssue")]
    pub outward_issue: Option<Box<JiraIssue>>,
}

/// Jira issue link type.
#[derive(Debug, Clone, Deserialize)]
pub struct JiraIssueLinkType {
    /// Link type name (e.g., "Blocks", "Relates")
    pub name: String,
    /// Inward description (e.g., "is blocked by")
    #[serde(default)]
    pub inward: Option<String>,
    /// Outward description (e.g., "blocks")
    #[serde(default)]
    pub outward: Option<String>,
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

// =============================================================================
// Issue Link types
// =============================================================================

/// Request body for creating an issue link.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateIssueLinkPayload {
    /// Link type
    #[serde(rename = "type")]
    pub link_type: IssueLinkTypeName,
    /// Inward issue (target)
    pub inward_issue: IssueKeyRef,
    /// Outward issue (source)
    pub outward_issue: IssueKeyRef,
}

/// Issue link type name reference.
#[derive(Debug, Clone, Serialize)]
pub struct IssueLinkTypeName {
    /// Link type name (e.g., "Blocks", "Relates")
    pub name: String,
}

/// Issue key reference for linking.
#[derive(Debug, Clone, Serialize)]
pub struct IssueKeyRef {
    /// Issue key (e.g., "PROJ-123")
    pub key: String,
}

// =============================================================================
// Jira Structure Plugin API types (/rest/structure/2.0/)
// =============================================================================

/// Structure info from GET /rest/structure/2.0/structure
#[derive(Debug, Clone, Deserialize)]
pub struct JiraStructure {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Response from GET /rest/structure/2.0/structure
#[derive(Debug, Clone, Deserialize)]
pub struct JiraStructureListResponse {
    pub structures: Vec<JiraStructure>,
}

/// A single row in the forest (compact format from API)
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraForestRow {
    pub id: u64,
    #[serde(default)]
    pub item_id: Option<String>,
    #[serde(default)]
    pub item_type: Option<String>,
}

/// Forest response from POST /rest/structure/2.0/forest/{id}/spec
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraForestResponse {
    pub version: u64,
    #[serde(default)]
    pub rows: Vec<JiraForestRow>,
    #[serde(default)]
    pub depths: Vec<u32>,
    #[serde(default)]
    pub total_count: Option<u64>,
}

/// Response from forest modification operations (add/move)
#[derive(Debug, Clone, Deserialize)]
pub struct JiraForestModifyResponse {
    pub version: u64,
    #[serde(default)]
    pub rows: Vec<JiraForestRow>,
}

/// Structure view from /rest/structure/2.0/view
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraStructureView {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub structure_id: u64,
    #[serde(default)]
    pub columns: Vec<JiraStructureViewColumn>,
    #[serde(default)]
    pub group_by: Option<String>,
    #[serde(default)]
    pub sort_by: Option<String>,
    #[serde(default)]
    pub filter: Option<String>,
}

/// Column definition in a structure view
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JiraStructureViewColumn {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub field: Option<String>,
    #[serde(default)]
    pub formula: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
}

/// Response from GET /rest/structure/2.0/view?structureId={id}
#[derive(Debug, Clone, Deserialize)]
pub struct JiraStructureViewListResponse {
    pub views: Vec<JiraStructureView>,
}

/// Batch value response from POST /rest/structure/2.0/value
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraStructureValueEntry {
    pub row_id: u64,
    #[serde(default)]
    pub column_id: Option<String>,
    #[serde(default)]
    pub value: serde_json::Value,
}

/// Response from POST /rest/structure/2.0/value
#[derive(Debug, Clone, Deserialize)]
pub struct JiraStructureValuesResponse {
    pub values: Vec<JiraStructureValueEntry>,
}
