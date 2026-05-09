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
    /// Id.
    pub id: u64,
    /// Username.
    pub username: String,
    #[serde(default)]
    /// Name.
    pub name: Option<String>,
    #[serde(default)]
    /// Avatar url.
    pub avatar_url: Option<String>,
    #[serde(default)]
    /// Web url.
    pub web_url: Option<String>,
}

// =============================================================================
// Issue
// =============================================================================

/// GitLab issue representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabIssue {
    /// Id.
    pub id: u64,
    /// Iid.
    pub iid: u64,
    /// Title.
    pub title: String,
    #[serde(default)]
    /// Description.
    pub description: Option<String>,
    /// State.
    pub state: String,
    #[serde(default)]
    /// Labels.
    pub labels: Vec<String>,
    #[serde(default)]
    /// Author.
    pub author: Option<GitLabUser>,
    #[serde(default)]
    /// Assignees.
    pub assignees: Vec<GitLabUser>,
    /// Web url.
    pub web_url: String,
    /// Created at.
    pub created_at: String,
    /// Updated at.
    pub updated_at: String,
}

// =============================================================================
// Merge Request
// =============================================================================

/// GitLab merge request representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabMergeRequest {
    /// Id.
    pub id: u64,
    /// Iid.
    pub iid: u64,
    /// Title.
    pub title: String,
    #[serde(default)]
    /// Description.
    pub description: Option<String>,
    /// State.
    pub state: String,
    /// Source branch.
    pub source_branch: String,
    /// Target branch.
    pub target_branch: String,
    #[serde(default)]
    /// Author.
    pub author: Option<GitLabUser>,
    #[serde(default)]
    /// Assignees.
    pub assignees: Vec<GitLabUser>,
    #[serde(default)]
    /// Reviewers.
    pub reviewers: Vec<GitLabUser>,
    #[serde(default)]
    /// Labels.
    pub labels: Vec<String>,
    #[serde(default)]
    /// Draft.
    pub draft: bool,
    #[serde(default)]
    /// Work in progress.
    pub work_in_progress: bool,
    #[serde(default)]
    /// Merged at.
    pub merged_at: Option<String>,
    /// Web url.
    pub web_url: String,
    #[serde(default)]
    /// Sha.
    pub sha: Option<String>,
    #[serde(default)]
    /// Diff refs.
    pub diff_refs: Option<GitLabDiffRefs>,
    /// Created at.
    pub created_at: String,
    /// Updated at.
    pub updated_at: String,
}

/// GitLab diff refs (SHA references for code positions).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabDiffRefs {
    /// Base sha.
    pub base_sha: String,
    /// Head sha.
    pub head_sha: String,
    /// Start sha.
    pub start_sha: String,
}

// =============================================================================
// Notes and Discussions
// =============================================================================

/// GitLab note (comment) representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabNote {
    /// Id.
    pub id: u64,
    /// Body.
    pub body: String,
    #[serde(default)]
    /// Author.
    pub author: Option<GitLabUser>,
    /// Created at.
    pub created_at: String,
    #[serde(default)]
    /// Updated at.
    pub updated_at: Option<String>,
    #[serde(default)]
    /// System.
    pub system: bool,
    #[serde(default)]
    /// Resolvable.
    pub resolvable: bool,
    #[serde(default)]
    /// Resolved.
    pub resolved: bool,
    #[serde(default)]
    /// Resolved by.
    pub resolved_by: Option<GitLabUser>,
    #[serde(default)]
    /// Position.
    pub position: Option<GitLabNotePosition>,
}

/// GitLab discussion (thread of notes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabDiscussion {
    /// Id.
    pub id: String,
    #[serde(default)]
    /// Notes.
    pub notes: Vec<GitLabNote>,
}

/// GitLab note position (for inline code comments).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabNotePosition {
    /// Position type.
    pub position_type: String,
    #[serde(default)]
    /// New path.
    pub new_path: Option<String>,
    #[serde(default)]
    /// Old path.
    pub old_path: Option<String>,
    #[serde(default)]
    /// New line.
    pub new_line: Option<u32>,
    #[serde(default)]
    /// Old line.
    pub old_line: Option<u32>,
}

// =============================================================================
// Diffs
// =============================================================================

/// GitLab diff representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabDiff {
    /// Old path.
    pub old_path: String,
    /// New path.
    pub new_path: String,
    #[serde(default)]
    /// New file.
    pub new_file: bool,
    #[serde(default)]
    /// Renamed file.
    pub renamed_file: bool,
    #[serde(default)]
    /// Deleted file.
    pub deleted_file: bool,
    #[serde(default)]
    /// Diff.
    pub diff: String,
}

/// GitLab MR changes response (MR + diffs in one call).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitLabMergeRequestChanges {
    #[serde(default)]
    /// Changes.
    pub changes: Vec<GitLabDiff>,
}

// =============================================================================
// Request types
// =============================================================================

/// Request body for creating an issue.
#[derive(Debug, Clone, Serialize)]
pub struct CreateIssueRequest {
    /// Title.
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Description.
    pub description: Option<String>,
    /// GitLab expects comma-separated string for labels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Assignee ids.
    pub assignee_ids: Option<Vec<u64>>,
}

/// Request body for updating an issue.
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateIssueRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Title.
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Description.
    pub description: Option<String>,
    /// GitLab uses state_event: "close" or "reopen"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_event: Option<String>,
    /// GitLab expects comma-separated string for labels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Assignee ids.
    pub assignee_ids: Option<Vec<u64>>,
}

/// Request body for updating a merge request.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateMergeRequestRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Title.
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Description.
    pub description: Option<String>,
    /// GitLab uses state_event: "close" or "reopen"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_event: Option<String>,
    /// Comma-separated labels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<String>,
}

/// Request body for creating a merge request.
#[derive(Debug, Clone, Serialize)]
pub struct CreateMergeRequestRequest {
    /// Source branch.
    pub source_branch: String,
    /// Target branch.
    pub target_branch: String,
    /// Title.
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Description.
    pub description: Option<String>,
    /// GitLab expects comma-separated string for labels
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Reviewer ids.
    pub reviewer_ids: Option<Vec<u64>>,
}

/// Request body for creating a note (comment).
#[derive(Debug, Clone, Serialize)]
pub struct CreateNoteRequest {
    /// Body.
    pub body: String,
}

/// Request body for creating a discussion on a merge request.
#[derive(Debug, Clone, Serialize)]
pub struct CreateDiscussionRequest {
    /// Body.
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Position.
    pub position: Option<DiscussionPosition>,
}

/// Position object for creating inline discussion on MR.
#[derive(Debug, Clone, Serialize)]
pub struct DiscussionPosition {
    /// Position type.
    pub position_type: String,
    /// Base sha.
    pub base_sha: String,
    /// Start sha.
    pub start_sha: String,
    /// Head sha.
    pub head_sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// New path.
    pub new_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Old path.
    pub old_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// New line.
    pub new_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Old line.
    pub old_line: Option<u32>,
}
