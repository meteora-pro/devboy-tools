//! GitHub API response types.
//!
//! These types represent the raw JSON responses from GitHub API.
//! They are deserialized and then mapped to unified types.

use serde::{Deserialize, Serialize};

// =============================================================================
// User
// =============================================================================

/// GitHub user representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubUser {
    pub id: u64,
    pub login: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

// =============================================================================
// Issue
// =============================================================================

/// GitHub issue representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubIssue {
    pub id: u64,
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    pub state: String,
    pub html_url: String,
    #[serde(default)]
    pub user: Option<GitHubUser>,
    #[serde(default)]
    pub assignees: Vec<GitHubUser>,
    #[serde(default)]
    pub labels: Vec<GitHubLabel>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub closed_at: Option<String>,
    /// PRs are also returned by /issues endpoint, this field distinguishes them
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
}

/// GitHub label representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubLabel {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

// =============================================================================
// Pull Request
// =============================================================================

/// GitHub pull request representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubPullRequest {
    pub id: u64,
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    pub state: String,
    pub html_url: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub merged: bool,
    #[serde(default)]
    pub merged_at: Option<String>,
    #[serde(default)]
    pub user: Option<GitHubUser>,
    #[serde(default)]
    pub assignees: Vec<GitHubUser>,
    #[serde(default)]
    pub requested_reviewers: Vec<GitHubUser>,
    #[serde(default)]
    pub labels: Vec<GitHubLabel>,
    pub head: GitHubBranchRef,
    pub base: GitHubBranchRef,
    pub created_at: String,
    pub updated_at: String,
}

/// GitHub branch reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubBranchRef {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub sha: String,
}

// =============================================================================
// Comments
// =============================================================================

/// GitHub issue/PR comment (general comments, not code review).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubComment {
    pub id: u64,
    pub body: String,
    #[serde(default)]
    pub user: Option<GitHubUser>,
    pub created_at: String,
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// GitHub review comment (code review comment).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubReviewComment {
    pub id: u64,
    pub body: String,
    #[serde(default)]
    pub user: Option<GitHubUser>,
    pub created_at: String,
    #[serde(default)]
    pub updated_at: Option<String>,
    /// File path
    pub path: String,
    /// Line number (can be null for outdated comments)
    #[serde(default)]
    pub line: Option<u32>,
    /// Original line (for moved/changed lines)
    #[serde(default)]
    pub original_line: Option<u32>,
    /// Position in diff (deprecated but still used)
    #[serde(default)]
    pub position: Option<u32>,
    /// Side: LEFT (old) or RIGHT (new)
    #[serde(default)]
    pub side: Option<String>,
    /// Diff hunk context
    #[serde(default)]
    pub diff_hunk: Option<String>,
    /// Commit SHA
    #[serde(default)]
    pub commit_id: Option<String>,
    /// Original commit SHA
    #[serde(default)]
    pub original_commit_id: Option<String>,
    /// ID of comment this is replying to
    #[serde(default)]
    pub in_reply_to_id: Option<u64>,
}

// =============================================================================
// Reviews
// =============================================================================

/// GitHub pull request review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubReview {
    pub id: u64,
    #[serde(default)]
    pub user: Option<GitHubUser>,
    #[serde(default)]
    pub body: Option<String>,
    /// APPROVED, CHANGES_REQUESTED, COMMENTED, PENDING, DISMISSED
    pub state: String,
    #[serde(default)]
    pub submitted_at: Option<String>,
}

// =============================================================================
// Files (Diffs)
// =============================================================================

/// GitHub pull request file (diff).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubFile {
    pub sha: String,
    pub filename: String,
    /// added, removed, modified, renamed, copied, changed, unchanged
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
    pub changes: u32,
    #[serde(default)]
    pub patch: Option<String>,
    #[serde(default)]
    pub previous_filename: Option<String>,
}

// =============================================================================
// Create/Update types
// =============================================================================

/// Request body for creating an issue.
#[derive(Debug, Clone, Serialize)]
pub struct CreateIssueRequest {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assignees: Vec<String>,
}

/// Request body for updating an issue.
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateIssueRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignees: Option<Vec<String>>,
}

/// Request body for creating a comment.
#[derive(Debug, Clone, Serialize)]
pub struct CreateCommentRequest {
    pub body: String,
}

/// Request body for creating a review comment.
#[derive(Debug, Clone, Serialize)]
pub struct CreateReviewCommentRequest {
    pub body: String,
    pub commit_id: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<u64>,
}
