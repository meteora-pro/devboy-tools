//! ClickUp API response types.
//!
//! These types represent the raw JSON responses from ClickUp API v2.
//! They are deserialized and then mapped to unified types.

use serde::{Deserialize, Serialize};

// =============================================================================
// User
// =============================================================================

/// ClickUp user representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpUser {
    /// Id.
    pub id: u64,
    /// Username.
    pub username: String,
    #[serde(default)]
    /// Email.
    pub email: Option<String>,
    #[serde(default, rename = "profilePicture")]
    /// Profile picture.
    pub profile_picture: Option<String>,
}

// =============================================================================
// Task (Issue)
// =============================================================================

/// ClickUp task representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpTask {
    /// Id.
    pub id: String,
    #[serde(default)]
    /// Custom id.
    pub custom_id: Option<String>,
    /// Name.
    pub name: String,
    #[serde(default)]
    /// Description.
    pub description: Option<String>,
    #[serde(default)]
    /// Text content.
    pub text_content: Option<String>,
    /// Status.
    pub status: ClickUpStatus,
    #[serde(default)]
    /// Priority.
    pub priority: Option<ClickUpPriority>,
    #[serde(default)]
    /// Tags.
    pub tags: Vec<ClickUpTag>,
    #[serde(default)]
    /// Assignees.
    pub assignees: Vec<ClickUpUser>,
    #[serde(default)]
    /// Creator.
    pub creator: Option<ClickUpUser>,
    /// Url.
    pub url: String,
    #[serde(default)]
    /// Date created.
    pub date_created: Option<String>,
    #[serde(default)]
    /// Date updated.
    pub date_updated: Option<String>,
    #[serde(default)]
    /// Parent.
    pub parent: Option<String>,
    #[serde(default)]
    /// Subtasks.
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
}

/// ClickUp task attachment entry as returned on the task payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpAttachment {
    /// Attachment id.
    pub id: String,
    /// File title / original filename.
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
    /// MIME type.
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
    /// Status.
    pub status: String,
    #[serde(default, rename = "type")]
    /// Status type.
    pub status_type: Option<String>,
}

/// ClickUp task priority.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpPriority {
    /// Id.
    pub id: String,
    /// Priority.
    pub priority: String,
    #[serde(default)]
    /// Color.
    pub color: Option<String>,
}

/// ClickUp tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpTag {
    /// Name.
    pub name: String,
}

/// ClickUp linked task (non-dependency relationship).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpLinkedTask {
    /// Task id.
    pub task_id: String,
    /// Link id.
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
    /// Tasks.
    pub tasks: Vec<ClickUpTask>,
}

// =============================================================================
// Comment
// =============================================================================

/// ClickUp comment representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpComment {
    /// Id.
    pub id: String,
    /// Comment text.
    pub comment_text: String,
    #[serde(default)]
    /// User.
    pub user: Option<ClickUpUser>,
    #[serde(default)]
    /// Date.
    pub date: Option<String>,
}

/// Response from GET /task/{task_id}/comment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpCommentList {
    /// Comments.
    pub comments: Vec<ClickUpComment>,
}

// =============================================================================
// List (for status resolution)
// =============================================================================

/// ClickUp list status (from GET /list/{list_id}).
#[derive(Debug, Clone, Deserialize)]
pub struct ClickUpListStatus {
    /// Status.
    pub status: String,
    #[serde(default, rename = "type")]
    /// Status type.
    pub status_type: Option<String>,
    #[serde(default)]
    /// Color.
    pub color: Option<String>,
    #[serde(default)]
    /// Orderindex.
    pub orderindex: Option<u32>,
}

/// Partial response from GET /list/{list_id} (only statuses needed).
#[derive(Debug, Clone, Deserialize)]
pub struct ClickUpListInfo {
    /// Statuses.
    pub statuses: Vec<ClickUpListStatus>,
}

/// Response from POST /task/{task_id}/dependency.
#[derive(Debug, Clone, Deserialize)]
pub struct ClickUpDependencyResponse {
    #[serde(default)]
    /// Dependency.
    pub dependency: Option<serde_json::Value>,
}

/// Response from POST /task/{task_id}/link/{other_task_id}.
#[derive(Debug, Clone, Deserialize)]
pub struct ClickUpLinkResponse {
    #[serde(default)]
    /// Link.
    pub link: Option<serde_json::Value>,
}

// =============================================================================
// Create/Update types
// =============================================================================

/// Request body for creating a task.
#[derive(Debug, Clone, Serialize)]
pub struct CreateTaskRequest {
    /// Name.
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Description.
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Markdown content.
    pub markdown_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Parent.
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Status.
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Priority.
    pub priority: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Tags.
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Assignees.
    pub assignees: Option<Vec<u64>>,
}

/// Request body for updating a task.
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateTaskRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Name.
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Description.
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Markdown content.
    pub markdown_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Status.
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Priority.
    pub priority: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Parent.
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Tags.
    pub tags: Option<Vec<String>>,
}

/// Request body for creating a comment.
#[derive(Debug, Clone, Serialize)]
pub struct CreateCommentRequest {
    /// Comment text.
    pub comment_text: String,
}

/// Response from POST /task/{task_id}/comment.
/// ClickUp returns a minimal response (no comment_text, id and date may be numbers).
#[derive(Debug, Clone, Deserialize)]
pub struct CreateCommentResponse {
    #[serde(deserialize_with = "value_to_string")]
    /// Id.
    pub id: String,
    #[serde(default, deserialize_with = "option_value_to_string")]
    /// Date.
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
