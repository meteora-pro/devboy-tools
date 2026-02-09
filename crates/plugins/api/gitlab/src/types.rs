//! GitLab API response and request types.
//!
//! These types represent the raw JSON responses from GitLab REST API v4.
//! They are deserialized and then mapped to unified types.

use serde::{Deserialize, Serialize};

// =============================================================================
// User
// =============================================================================

/// GitLab user representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabUser {
    pub id: u64,
    pub username: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub web_url: Option<String>,
}

// =============================================================================
// Issue
// =============================================================================

/// GitLab issue representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabIssue {
    pub id: u64,
    pub iid: u64,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub state: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub author: Option<GitLabUser>,
    #[serde(default)]
    pub assignees: Vec<GitLabUser>,
    pub web_url: String,
    pub created_at: String,
    pub updated_at: String,
}

// =============================================================================
// Merge Request
// =============================================================================

/// GitLab merge request representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabMergeRequest {
    pub id: u64,
    pub iid: u64,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub state: String,
    pub source_branch: String,
    pub target_branch: String,
    #[serde(default)]
    pub author: Option<GitLabUser>,
    #[serde(default)]
    pub assignees: Vec<GitLabUser>,
    #[serde(default)]
    pub reviewers: Vec<GitLabUser>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub work_in_progress: bool,
    #[serde(default)]
    pub merged_at: Option<String>,
    pub web_url: String,
    #[serde(default)]
    pub sha: Option<String>,
    #[serde(default)]
    pub diff_refs: Option<GitLabDiffRefs>,
    pub created_at: String,
    pub updated_at: String,
}

/// GitLab diff refs (SHA references for code positions).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabDiffRefs {
    pub base_sha: String,
    pub head_sha: String,
    pub start_sha: String,
}

// =============================================================================
// Notes and Discussions
// =============================================================================

/// GitLab note (comment) representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabNote {
    pub id: u64,
    pub body: String,
    #[serde(default)]
    pub author: Option<GitLabUser>,
    pub created_at: String,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub system: bool,
    #[serde(default)]
    pub resolvable: bool,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub resolved_by: Option<GitLabUser>,
    #[serde(default)]
    pub position: Option<GitLabNotePosition>,
}

/// GitLab discussion (thread of notes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabDiscussion {
    pub id: String,
    #[serde(default)]
    pub notes: Vec<GitLabNote>,
}

/// GitLab note position (for inline code comments).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabNotePosition {
    pub position_type: String,
    #[serde(default)]
    pub new_path: Option<String>,
    #[serde(default)]
    pub old_path: Option<String>,
    #[serde(default)]
    pub new_line: Option<u32>,
    #[serde(default)]
    pub old_line: Option<u32>,
}

// =============================================================================
// Diffs
// =============================================================================

/// GitLab diff representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabDiff {
    pub old_path: String,
    pub new_path: String,
    #[serde(default)]
    pub new_file: bool,
    #[serde(default)]
    pub renamed_file: bool,
    #[serde(default)]
    pub deleted_file: bool,
    #[serde(default)]
    pub diff: String,
}

/// GitLab MR changes response (MR + diffs in one call).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabMergeRequestChanges {
    #[serde(default)]
    pub changes: Vec<GitLabDiff>,
}

// =============================================================================
// Request types
// =============================================================================

/// Request body for creating an issue.
#[derive(Debug, Clone, Serialize)]
pub struct CreateIssueRequest {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// GitLab expects comma-separated string for labels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee_ids: Option<Vec<u64>>,
}

/// Request body for updating an issue.
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateIssueRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// GitLab uses state_event: "close" or "reopen"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_event: Option<String>,
    /// GitLab expects comma-separated string for labels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee_ids: Option<Vec<u64>>,
}

/// Request body for creating a note (comment).
#[derive(Debug, Clone, Serialize)]
pub struct CreateNoteRequest {
    pub body: String,
}

/// Request body for creating a discussion on a merge request.
#[derive(Debug, Clone, Serialize)]
pub struct CreateDiscussionRequest {
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<DiscussionPosition>,
}

/// Position object for creating inline discussion on MR.
#[derive(Debug, Clone, Serialize)]
pub struct DiscussionPosition {
    pub position_type: String,
    pub base_sha: String,
    pub start_sha: String,
    pub head_sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_line: Option<u32>,
}
