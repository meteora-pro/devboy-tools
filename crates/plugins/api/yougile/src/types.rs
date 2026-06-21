//! Shared YouGile wire and helper types.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Board descriptor used by the YouGile provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct YouGileBoardRef {
    pub id: String,
    pub title: String,
}

/// Column descriptor used by the YouGile provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct YouGileColumnRef {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub board_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct YouGilePaging {
    pub limit: u32,
    pub offset: u32,
    pub next: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct YouGileListResponse<T> {
    pub paging: YouGilePaging,
    pub content: Vec<T>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct YouGileColumn {
    pub id: String,
    pub title: String,
    #[serde(rename = "boardId")]
    pub board_id: String,
    #[serde(default)]
    pub deleted: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct YouGileTask {
    pub id: String,
    pub title: String,
    pub timestamp: u64,
    #[serde(rename = "columnId")]
    pub column_id: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub completed: bool,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default, rename = "subtasks")]
    pub subtask_ids: Vec<String>,
    #[serde(default, rename = "assigned")]
    pub assigned_ids: Vec<String>,
    #[serde(default, rename = "createdBy")]
    pub created_by: Option<String>,
    #[serde(default, rename = "idTaskCommon")]
    pub id_task_common: Option<String>,
    #[serde(default, rename = "idTaskProject")]
    pub id_task_project: Option<String>,
    #[serde(default)]
    pub stickers: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct YouGileUser {
    pub id: String,
    pub email: String,
    #[serde(rename = "realName")]
    pub real_name: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct YouGileChatMessage {
    pub id: u64,
    #[serde(rename = "fromUserId")]
    pub from_user_id: String,
    pub text: String,
    #[serde(default, rename = "textHtml")]
    pub text_html: Option<String>,
    #[serde(default)]
    pub label: String,
    #[serde(rename = "editTimestamp")]
    pub edit_timestamp: u64,
    #[serde(default)]
    pub deleted: bool,
}
