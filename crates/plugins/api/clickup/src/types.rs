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

/// ClickUp task representation.
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

/// ClickUp tag.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClickUpTag {
    pub name: String,
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
// Comment
// =============================================================================

/// ClickUp comment representation.
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
}

/// Partial response from GET /list/{list_id} (only statuses needed).
#[derive(Debug, Clone, Deserialize)]
pub struct ClickUpListInfo {
    pub statuses: Vec<ClickUpListStatus>,
}

// =============================================================================
// Create/Update types
// =============================================================================

/// Request body for creating a task.
#[derive(Debug, Clone, Serialize)]
pub struct CreateTaskRequest {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignees: Option<Vec<u64>>,
}

/// Request body for updating a task.
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateTaskRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
}

/// Request body for creating a comment.
#[derive(Debug, Clone, Serialize)]
pub struct CreateCommentRequest {
    pub comment_text: String,
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
