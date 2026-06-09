//! ClickUp API response types.
//!
//! These types represent the raw JSON responses from ClickUp API v2.
//! They are deserialized and then mapped to unified types.

use serde::{Deserialize, Serialize};

// =============================================================================
// User
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpUser {
    pub id: u64,
    pub username: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default, rename = "profilePicture")]
    pub profile_picture: Option<String>,
}

// =============================================================================
// Task (Issue)
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpTask {
    pub id: String,
    #[serde(default)]
    pub custom_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub text_content: Option<String>,
    pub status: ClickUpStatus,
    #[serde(default)]
    pub priority: Option<ClickUpPriority>,
    #[serde(default)]
    pub tags: Vec<ClickUpTag>,
    #[serde(default)]
    pub assignees: Vec<ClickUpUser>,
    #[serde(default)]
    pub creator: Option<ClickUpUser>,
    pub url: String,
    #[serde(default)]
    pub date_created: Option<String>,
    #[serde(default)]
    pub date_updated: Option<String>,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub subtasks: Option<Vec<ClickUpTask>>,
    /// Dependencies (blocking/waiting relationships).
    /// Uses `serde_json::Value` for flexible parsing of undocumented API shape.
    #[serde(default)]
    pub dependencies: Option<Vec<serde_json::Value>>,
    /// Linked tasks (non-dependency relationships).
    #[serde(default)]
    pub linked_tasks: Option<Vec<ClickUpLinkedTask>>,
    /// Attachments uploaded to the task.
    ///
    /// The ClickUp API returns this under `attachments` on the task object.
    /// It may be absent for older tasks or tasks without uploads.
    #[serde(default)]
    pub attachments: Vec<ClickUpAttachment>,
    /// Custom fields configured on the list this task lives in. Each
    /// entry has a stable id, a human-readable name, and an arbitrary
    /// JSON value (string for text, number for numeric, object for
    /// dropdown / labels). Empty for tasks in lists without custom
    /// fields.
    #[serde(default)]
    pub custom_fields: Vec<ClickUpCustomField>,
}

/// ClickUp custom-field entry as returned on the task payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpCustomField {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Field type — `"text"`, `"number"`, `"drop_down"`, `"labels"`,
    /// `"users"`, `"date"`, …
    #[serde(default, rename = "type")]
    pub field_type: Option<String>,
    /// Raw value as returned by ClickUp. Shape varies by `field_type`.
    /// Absent when the user hasn't set the field.
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    /// Field configuration embedded in the task payload. For
    /// `drop_down` / `labels` fields this carries the `options` list,
    /// which lets us resolve the opaque `value` (an order index or
    /// option id) back to its human-readable label inline — no extra
    /// metadata fetch required.
    #[serde(default)]
    pub type_config: Option<ClickUpFieldTypeConfig>,
}

/// `type_config` block embedded per custom field on a ClickUp task.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClickUpFieldTypeConfig {
    /// Selectable options for `drop_down` / `labels` fields.
    #[serde(default)]
    pub options: Vec<ClickUpFieldOptionInline>,
}

/// A single `drop_down`/`labels` option as embedded in a task payload.
/// `drop_down` options carry `name`; `labels` options carry `label` —
/// both are accepted so either field type resolves.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClickUpFieldOptionInline {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default, alias = "label")]
    pub name: Option<String>,
    #[serde(default)]
    pub orderindex: Option<u32>,
}

/// ClickUp task attachment entry as returned on the task payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpAttachment {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Direct download URL.
    #[serde(default)]
    pub url: Option<String>,
    /// Size in bytes (API sometimes returns a string).
    #[serde(default)]
    pub size: Option<serde_json::Value>,
    /// File extension (e.g. "png").
    #[serde(default)]
    pub extension: Option<String>,
    #[serde(default)]
    pub mimetype: Option<String>,
    /// Creation timestamp (epoch ms as string in ClickUp's responses).
    #[serde(default)]
    pub date: Option<String>,
    /// Author display info, if present.
    #[serde(default)]
    pub user: Option<ClickUpUser>,
}

/// ClickUp task status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpStatus {
    pub status: String,
    #[serde(default, rename = "type")]
    pub status_type: Option<String>,
}

/// ClickUp task priority.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpPriority {
    pub id: String,
    pub priority: String,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpTag {
    pub name: String,
}

/// ClickUp linked task (non-dependency relationship).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpLinkedTask {
    pub task_id: String,
    pub link_id: String,
    /// Dependency type: "blocked_by", "blocking", or null (plain link).
    #[serde(default)]
    pub link_type: Option<String>,
}

// =============================================================================
// Task List Response
// =============================================================================

/// Response from GET /list/{list_id}/task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpTaskList {
    pub tasks: Vec<ClickUpTask>,
}

// =============================================================================
// Team (workspace) — used to look up assignees by email/username.
// =============================================================================

/// Response from GET /api/v2/team — list workspaces the auth user is in,
/// each with embedded members.
#[derive(Debug, Clone, Deserialize)]
pub struct ClickUpTeamsResponse {
    #[serde(default)]
    pub teams: Vec<ClickUpTeam>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClickUpTeam {
    pub id: String,
    #[serde(default)]
    pub members: Vec<ClickUpTeamMember>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClickUpTeamMember {
    pub user: ClickUpUser,
}

// =============================================================================
// Comment
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpComment {
    pub id: String,
    pub comment_text: String,
    #[serde(default)]
    pub user: Option<ClickUpUser>,
    #[serde(default)]
    pub date: Option<String>,
}

/// Response from GET /task/{task_id}/comment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpCommentList {
    pub comments: Vec<ClickUpComment>,
}

// =============================================================================
// List (for status resolution)
// =============================================================================

/// ClickUp list status (from GET /list/{list_id}).
#[derive(Debug, Clone, Deserialize)]
pub struct ClickUpListStatus {
    pub status: String,
    #[serde(default, rename = "type")]
    pub status_type: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub orderindex: Option<u32>,
}

/// Partial response from GET /list/{list_id} (only statuses needed).
#[derive(Debug, Clone, Deserialize)]
pub struct ClickUpListInfo {
    pub statuses: Vec<ClickUpListStatus>,
}

/// Response from POST /task/{task_id}/dependency.
#[derive(Debug, Clone, Deserialize)]
pub struct ClickUpDependencyResponse {
    #[serde(default)]
    pub dependency: Option<serde_json::Value>,
}

/// Response from POST /task/{task_id}/link/{other_task_id}.
#[derive(Debug, Clone, Deserialize)]
pub struct ClickUpLinkResponse {
    #[serde(default)]
    pub link: Option<serde_json::Value>,
}

// =============================================================================
// Create/Update types
// =============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct CreateTaskRequest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignees: Option<Vec<u64>>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateTaskRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Assignee diff for PUT /task/:id. ClickUp does NOT accept a flat
    /// `assignees: [...]` array on update — it silently 200's and drops
    /// the field. The supported shape is `{ add: [u64], rem: [u64] }`.
    /// `None` leaves assignees untouched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignees: Option<AssigneeDiff>,
}

/// Diff envelope for PUT /task/:id `assignees` field.
/// Both arrays are sent only when non-empty so ClickUp doesn't see noise.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AssigneeDiff {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub add: Vec<u64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rem: Vec<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateCommentRequest {
    /// Plain-text body. ClickUp requires this field and uses it as the
    /// fallback / notification text. It is run through a lossy auto-formatter
    /// for rendering, so the structured `comment` array below is what actually
    /// drives clean rich-text display.
    pub comment_text: String,
    /// Structured rich-text runs (Quill Delta shape). When present, ClickUp
    /// renders these instead of auto-formatting `comment_text`, so inline
    /// code, code blocks and lists display correctly without fragmenting
    /// prose. Built from the markdown body by
    /// [`crate::comment_format::markdown_to_comment_blocks`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub comment: Vec<crate::comment_format::CommentBlock>,
}

/// Response from POST /task/{task_id}/comment.
/// ClickUp returns a minimal response (no comment_text, id and date may be numbers).
#[derive(Debug, Clone, Deserialize)]
pub struct CreateCommentResponse {
    #[serde(deserialize_with = "value_to_string")]
    pub id: String,
    #[serde(default, deserialize_with = "option_value_to_string")]
    pub date: Option<String>,
}

/// Deserialize a value that may be a string or a number into String.
fn value_to_string<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::String(s) => Ok(s),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        other => Ok(other.to_string()),
    }
}

/// Deserialize an optional value that may be a string or a number into Option<String>.
fn option_value_to_string<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.map(|v| match v {
        serde_json::Value::String(s) => s,
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }))
}
