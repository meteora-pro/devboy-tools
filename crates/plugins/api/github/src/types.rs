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
    /// Id.
    pub id: u64,
    /// Login.
    pub login: String,
    #[serde(default)]
    /// Name.
    pub name: Option<String>,
    #[serde(default)]
    /// Email.
    pub email: Option<String>,
    #[serde(default)]
    /// Avatar url.
    pub avatar_url: Option<String>,
}

// =============================================================================
// Issue
// =============================================================================

/// GitHub issue representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubIssue {
    /// Id.
    pub id: u64,
    /// Number.
    pub number: u64,
    /// Title.
    pub title: String,
    #[serde(default)]
    /// Body.
    pub body: Option<String>,
    /// State.
    pub state: String,
    /// Html url.
    pub html_url: String,
    #[serde(default)]
    /// User.
    pub user: Option<GitHubUser>,
    #[serde(default)]
    /// Assignees.
    pub assignees: Vec<GitHubUser>,
    #[serde(default)]
    /// Labels.
    pub labels: Vec<GitHubLabel>,
    /// Created at.
    pub created_at: String,
    /// Updated at.
    pub updated_at: String,
    #[serde(default)]
    /// Closed at.
    pub closed_at: Option<String>,
    /// PRs are also returned by /issues endpoint, this field distinguishes them
    #[serde(default)]
    pub pull_request: Option<serde_json::Value>,
}

/// GitHub label representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubLabel {
    /// Id.
    pub id: u64,
    /// Name.
    pub name: String,
    #[serde(default)]
    /// Color.
    pub color: Option<String>,
    #[serde(default)]
    /// Description.
    pub description: Option<String>,
}

// =============================================================================
// Pull Request
// =============================================================================

/// GitHub pull request representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubPullRequest {
    /// Id.
    pub id: u64,
    /// Number.
    pub number: u64,
    /// Title.
    pub title: String,
    #[serde(default)]
    /// Body.
    pub body: Option<String>,
    /// State.
    pub state: String,
    /// Html url.
    pub html_url: String,
    #[serde(default)]
    /// Draft.
    pub draft: bool,
    #[serde(default)]
    /// Merged.
    pub merged: bool,
    #[serde(default)]
    /// Merged at.
    pub merged_at: Option<String>,
    #[serde(default)]
    /// User.
    pub user: Option<GitHubUser>,
    #[serde(default)]
    /// Assignees.
    pub assignees: Vec<GitHubUser>,
    #[serde(default)]
    /// Requested reviewers.
    pub requested_reviewers: Vec<GitHubUser>,
    #[serde(default)]
    /// Labels.
    pub labels: Vec<GitHubLabel>,
    /// Head.
    pub head: GitHubBranchRef,
    /// Base.
    pub base: GitHubBranchRef,
    /// Created at.
    pub created_at: String,
    /// Updated at.
    pub updated_at: String,
}

/// GitHub branch reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubBranchRef {
    #[serde(rename = "ref")]
    /// Ref name.
    pub ref_name: String,
    /// Sha.
    pub sha: String,
}

// =============================================================================
// Comments
// =============================================================================

/// GitHub issue/PR comment (general comments, not code review).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubComment {
    /// Id.
    pub id: u64,
    /// Body.
    pub body: String,
    #[serde(default)]
    /// User.
    pub user: Option<GitHubUser>,
    /// Created at.
    pub created_at: String,
    #[serde(default)]
    /// Updated at.
    pub updated_at: Option<String>,
}

/// GitHub review comment (code review comment).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubReviewComment {
    /// Id.
    pub id: u64,
    /// Body.
    pub body: String,
    #[serde(default)]
    /// User.
    pub user: Option<GitHubUser>,
    /// Created at.
    pub created_at: String,
    #[serde(default)]
    /// Updated at.
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
    /// Id.
    pub id: u64,
    #[serde(default)]
    /// User.
    pub user: Option<GitHubUser>,
    #[serde(default)]
    /// Body.
    pub body: Option<String>,
    /// APPROVED, CHANGES_REQUESTED, COMMENTED, PENDING, DISMISSED
    pub state: String,
    #[serde(default)]
    /// Submitted at.
    pub submitted_at: Option<String>,
}

// =============================================================================
// Files (Diffs)
// =============================================================================

/// GitHub pull request file (diff).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubFile {
    /// Sha.
    pub sha: String,
    /// Filename.
    pub filename: String,
    /// added, removed, modified, renamed, copied, changed, unchanged
    pub status: String,
    /// Additions.
    pub additions: u32,
    /// Deletions.
    pub deletions: u32,
    /// Changes.
    pub changes: u32,
    #[serde(default)]
    /// Patch.
    pub patch: Option<String>,
    #[serde(default)]
    /// Previous filename.
    pub previous_filename: Option<String>,
}

// =============================================================================
// Create/Update types
// =============================================================================

/// Request body for creating an issue.
#[derive(Debug, Clone, Serialize)]
pub struct CreateIssueRequest {
    /// Title.
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Body.
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    /// Labels.
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    /// Assignees.
    pub assignees: Vec<String>,
}

/// Request body for updating an issue.
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateIssueRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Title.
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Body.
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// State.
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Labels.
    pub labels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Assignees.
    pub assignees: Option<Vec<String>>,
}

/// Request body for creating a pull request.
#[derive(Debug, Clone, Serialize)]
pub struct CreatePullRequestRequest {
    /// Title.
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Body.
    pub body: Option<String>,
    /// Source branch (head)
    pub head: String,
    /// Target branch (base)
    pub base: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Draft.
    pub draft: Option<bool>,
}

/// Request body for updating a pull request.
#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdatePullRequestRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Title.
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Body.
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// State.
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Draft.
    pub draft: Option<bool>,
}

/// Request body for creating a comment.
#[derive(Debug, Clone, Serialize)]
pub struct CreateCommentRequest {
    /// Body.
    pub body: String,
}

/// Request body for creating a review comment.
#[derive(Debug, Clone, Serialize)]
pub struct CreateReviewCommentRequest {
    /// Body.
    pub body: String,
    /// Commit id.
    pub commit_id: String,
    /// Path.
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Line.
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Side.
    pub side: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// In reply to.
    pub in_reply_to: Option<u64>,
}
