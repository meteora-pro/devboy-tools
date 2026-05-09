//! Jira API response types.
//!
//! These types represent the raw JSON responses from Jira API v2/v3.
//! They are deserialized and then mapped to unified types.

use serde::{Deserialize, Serialize};

// =============================================================================
// User
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
pub struct JiraUser {
    /// Account ID (Cloud only)
    #[serde(default, rename = "accountId")]
    pub account_id: Option<String>,
    /// Username (Self-Hosted only)
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(default, rename = "emailAddress")]
    pub email_address: Option<String>,
}

// =============================================================================
// Issue
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
pub struct JiraIssue {
    pub id: String,
    /// Issue key (e.g., "PROJ-123")
    pub key: String,
    pub fields: JiraIssueFields,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JiraIssueFields {
    #[serde(default)]
    pub summary: Option<String>,
    /// Description — plain text (v2) or ADF document (v3)
    #[serde(default)]
    pub description: Option<serde_json::Value>,
    #[serde(default)]
    pub status: Option<JiraStatus>,
    #[serde(default)]
    pub priority: Option<JiraPriority>,
    #[serde(default)]
    pub assignee: Option<JiraUser>,
    /// Reporter (author)
    #[serde(default)]
    pub reporter: Option<JiraUser>,
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
    #[serde(default)]
    pub filename: Option<String>,
    /// Direct download URL (`content` in the Jira API).
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
    /// Creation timestamp (ISO 8601).
    #[serde(default)]
    pub created: Option<String>,
    /// Author — Jira uses `author` inside the attachment object.
    #[serde(default)]
    pub author: Option<JiraUser>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraStatus {
    pub name: String,
    /// Status category (new, indeterminate, done)
    #[serde(default)]
    pub status_category: Option<JiraStatusCategory>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JiraStatusCategory {
    /// Category key: "new", "indeterminate", "done"
    pub key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JiraPriority {
    pub name: String,
}

// =============================================================================
// Issue Links
// =============================================================================

#[derive(Debug, Clone, Deserialize)]
pub struct JiraIssueLink {
    /// Link ID
    #[serde(default)]
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub link_type: JiraIssueLinkType,
    /// Inward issue (e.g., "is blocked by" this issue)
    #[serde(default, rename = "inwardIssue")]
    pub inward_issue: Option<Box<JiraIssue>>,
    /// Outward issue (e.g., "blocks" this issue)
    #[serde(default, rename = "outwardIssue")]
    pub outward_issue: Option<Box<JiraIssue>>,
}

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
    pub issues: Vec<JiraIssue>,
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
    pub project: ProjectKey,
    /// Summary (title)
    pub summary: String,
    /// Issue type
    pub issuetype: IssueType,
    /// Description — plain text (v2) or ADF (v3)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<PriorityName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<serde_json::Value>,
    /// Components (issue #197).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<Vec<ComponentRef>>,
    /// Parent issue reference. Required by Jira when `issuetype` is a
    /// sub-task or any "is_subtask" hierarchical type — the API rejects
    /// the request with a 400 otherwise (issue #214). Serialised as
    /// `{ "key": "PROJ-1" }`, matching Jira's `fields.parent` shape.
    /// Uses [`IssueKeyRef`] (not [`ProjectKey`]) because the value is an
    /// **issue** key (e.g. `PROJ-1`), not a project key (e.g. `PROJ`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<IssueKeyRef>,
}

/// Component reference used in create/update issue payloads (issue #197).
///
/// We serialise by **name** (`{ "name": "..." }`) to line up with the
/// Jira schema enricher, which enumerates component *names* in tool
/// schemas (`JiraComponent.name`). Jira accepts either `{id}` or `{name}`
/// in `fields.components`, so name-based references work across Cloud +
/// Self-Hosted without forcing callers to resolve ids first.
///
/// This is addressed in Copilot review on PR #205.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentRef {
    pub name: String,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<PriorityName>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<serde_json::Value>,
    /// Components (issue #197). `None` means do not touch.
    /// `Some(vec![])` clears all components.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub components: Option<Vec<ComponentRef>>,
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
    /// Owning structure id. Left non-optional and **without**
    /// `#[serde(default)]` so a missing / renamed field from the API
    /// fails loudly rather than silently deserialising to `0` — the
    /// caller uses this id for cross-structure scope checks.
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

// =============================================================================
// Jira Project Versions (issue #238) — /rest/api/2/version + /project/{key}/versions
// =============================================================================

/// Version DTO returned by the Jira REST API.
///
/// Field set is the union of Cloud (v3) and Server/DC (v2) — both use
/// the same path family. `issuesStatusForFixVersion` only appears when
/// the caller passes `?expand=issuesstatus`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraVersionDto {
    pub id: String,
    pub name: String,
    /// Project key (e.g., "PROJ"). Server returns this directly; Cloud
    /// returns `projectId` separately, so we accept either.
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub project_id: Option<u64>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub released: bool,
    #[serde(default)]
    pub archived: bool,
    /// Computed by the server: true when releaseDate is in the past
    /// and the version is still unreleased.
    #[serde(default)]
    pub overdue: Option<bool>,
    /// Returned only with `?expand=issuesstatus` (Cloud) — keys are
    /// status categories (`unmapped`, `toDo`, `inProgress`, `done`).
    #[serde(default)]
    pub issues_status_for_fix_version: Option<JiraVersionIssueStatusCounts>,
    /// Server/DC returns this directly on the version DTO. Callers that
    /// need a total fixed-issue count have to hit
    /// `/version/{id}/relatedIssueCounts` separately.
    #[serde(default)]
    pub issues_unresolved_count: Option<u32>,
}

/// Issue counts by status category (Cloud `?expand=issuesstatus`).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct JiraVersionIssueStatusCounts {
    #[serde(default)]
    pub unmapped: u32,
    #[serde(default)]
    pub to_do: u32,
    #[serde(default)]
    pub in_progress: u32,
    #[serde(default)]
    pub done: u32,
}

impl JiraVersionIssueStatusCounts {
    pub fn total(&self) -> u32 {
        self.unmapped
            .saturating_add(self.to_do)
            .saturating_add(self.in_progress)
            .saturating_add(self.done)
    }
}

/// POST /rest/api/2/version payload.
///
/// `project` and `project_id` are mutually-exclusive routing keys —
/// callers should fill exactly one (Server/DC accepts both, Cloud
/// historically prefers `projectId`). Optional date / state fields
/// are skipped when `None` so creation defaults match the UI.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CreateVersionPayload {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub released: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
}

/// PUT /rest/api/2/version/{id} payload — partial update; only fields
/// explicitly set are sent (`#[serde(skip_serializing_if = "Option::is_none")]`),
/// so unspecified fields are preserved server-side.
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpdateVersionPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub released: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
}
