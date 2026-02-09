//! Common types used across providers.
//!
//! These types are provider-agnostic and represent unified data structures
//! that can be populated from GitLab, GitHub, ClickUp, or Jira APIs.

use serde::{Deserialize, Serialize};

// =============================================================================
// User
// =============================================================================

/// Represents a user from a git hosting service.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct User {
    /// User ID (internal to the provider)
    pub id: String,
    /// Username / login
    pub username: String,
    /// Display name
    pub name: Option<String>,
    /// Email address
    pub email: Option<String>,
    /// Avatar URL
    pub avatar_url: Option<String>,
}

// =============================================================================
// Issue
// =============================================================================

/// Represents an issue from an issue tracker.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Issue {
    /// Unique key (e.g., "gitlab#123", "gh#456", "CU-abc", "PROJ-123")
    pub key: String,
    /// Issue title
    pub title: String,
    /// Issue description / body
    pub description: Option<String>,
    /// State (e.g., "opened", "closed")
    pub state: String,
    /// Source provider name (e.g., "gitlab", "github", "clickup", "jira")
    pub source: String,
    /// Priority (e.g., "urgent", "high", "normal", "low")
    pub priority: Option<String>,
    /// Labels / tags
    pub labels: Vec<String>,
    /// Author
    pub author: Option<User>,
    /// Assignees
    pub assignees: Vec<User>,
    /// Web URL for the issue
    pub url: Option<String>,
    /// Created at timestamp (ISO 8601)
    pub created_at: Option<String>,
    /// Updated at timestamp (ISO 8601)
    pub updated_at: Option<String>,
}

/// Filter parameters for listing issues.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueFilter {
    /// Filter by state (e.g., "opened", "closed", "all")
    pub state: Option<String>,
    /// Search query for title and description
    pub search: Option<String>,
    /// Filter by labels
    pub labels: Option<Vec<String>>,
    /// Filter by assignee username
    pub assignee: Option<String>,
    /// Maximum number of results
    pub limit: Option<u32>,
    /// Number of results to skip (offset)
    pub offset: Option<u32>,
    /// Sort by field (e.g., "created_at", "updated_at", "priority")
    pub sort_by: Option<String>,
    /// Sort order ("asc" or "desc")
    pub sort_order: Option<String>,
}

/// Input for creating a new issue.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateIssueInput {
    /// Issue title
    pub title: String,
    /// Issue description / body
    pub description: Option<String>,
    /// Labels to add
    pub labels: Vec<String>,
    /// Assignee usernames
    pub assignees: Vec<String>,
    /// Priority
    pub priority: Option<String>,
}

/// Input for updating an existing issue.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateIssueInput {
    /// New title
    pub title: Option<String>,
    /// New description
    pub description: Option<String>,
    /// New state
    pub state: Option<String>,
    /// New labels (replaces existing)
    pub labels: Option<Vec<String>>,
    /// New assignees (replaces existing)
    pub assignees: Option<Vec<String>>,
    /// New priority
    pub priority: Option<String>,
}

// =============================================================================
// Merge Request
// =============================================================================

/// Represents a merge request / pull request.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MergeRequest {
    /// Unique key (e.g., "mr#123", "pr#456")
    pub key: String,
    /// MR title
    pub title: String,
    /// MR description / body
    pub description: Option<String>,
    /// State (e.g., "opened", "closed", "merged")
    pub state: String,
    /// Source provider name
    pub source: String,
    /// Source branch
    pub source_branch: String,
    /// Target branch
    pub target_branch: String,
    /// Author
    pub author: Option<User>,
    /// Assignees
    pub assignees: Vec<User>,
    /// Reviewers
    pub reviewers: Vec<User>,
    /// Labels / tags
    pub labels: Vec<String>,
    /// Is draft/WIP
    pub draft: bool,
    /// Web URL for the MR
    pub url: Option<String>,
    /// Created at timestamp (ISO 8601)
    pub created_at: Option<String>,
    /// Updated at timestamp (ISO 8601)
    pub updated_at: Option<String>,
}

/// Filter parameters for listing merge requests.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MrFilter {
    /// Filter by state (e.g., "opened", "closed", "merged", "all")
    pub state: Option<String>,
    /// Filter by source branch
    pub source_branch: Option<String>,
    /// Filter by target branch
    pub target_branch: Option<String>,
    /// Filter by author username
    pub author: Option<String>,
    /// Filter by labels
    pub labels: Option<Vec<String>>,
    /// Maximum number of results
    pub limit: Option<u32>,
}

// =============================================================================
// Discussion and Comments
// =============================================================================

/// Represents a discussion thread on a merge request.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Discussion {
    /// Discussion ID
    pub id: String,
    /// Is the discussion resolved
    pub resolved: bool,
    /// Who resolved it
    pub resolved_by: Option<User>,
    /// Comments in this discussion
    pub comments: Vec<Comment>,
    /// Code position (if this is a code review comment)
    pub position: Option<CodePosition>,
}

/// Represents a comment on an issue or merge request.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Comment {
    /// Comment ID
    pub id: String,
    /// Comment body / text
    pub body: String,
    /// Author
    pub author: Option<User>,
    /// Created at timestamp (ISO 8601)
    pub created_at: Option<String>,
    /// Updated at timestamp (ISO 8601)
    pub updated_at: Option<String>,
    /// Code position (for inline comments)
    pub position: Option<CodePosition>,
}

/// Position in code for inline comments.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CodePosition {
    /// File path
    pub file_path: String,
    /// Line number
    pub line: u32,
    /// Line type ("old" for deleted, "new" for added)
    pub line_type: String,
    /// Commit SHA
    pub commit_sha: Option<String>,
}

/// Input for creating a comment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateCommentInput {
    /// Comment body / text
    pub body: String,
    /// Code position for inline comments
    pub position: Option<CodePosition>,
    /// Discussion ID to reply to
    pub discussion_id: Option<String>,
}

// =============================================================================
// File Diff
// =============================================================================

/// Represents a file diff in a merge request.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FileDiff {
    /// File path (new path if renamed)
    pub file_path: String,
    /// Old file path (if renamed)
    pub old_path: Option<String>,
    /// Is new file
    pub new_file: bool,
    /// Is deleted file
    pub deleted_file: bool,
    /// Is renamed file
    pub renamed_file: bool,
    /// Diff content (unified diff format)
    pub diff: String,
    /// Number of added lines
    pub additions: Option<u32>,
    /// Number of deleted lines
    pub deletions: Option<u32>,
}

// =============================================================================
// Pagination
// =============================================================================

/// Pagination information for list responses.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Pagination {
    /// Current offset
    pub offset: u32,
    /// Page size / limit
    pub limit: u32,
    /// Total count of items
    pub total: Option<u32>,
    /// Whether there are more items
    pub has_more: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_issue_default() {
        let issue = Issue::default();
        assert!(issue.key.is_empty());
        assert!(issue.title.is_empty());
        assert!(issue.state.is_empty());
    }

    #[test]
    fn test_issue_serialization() {
        let issue = Issue {
            key: "gitlab#123".to_string(),
            title: "Test issue".to_string(),
            state: "opened".to_string(),
            source: "gitlab".to_string(),
            ..Default::default()
        };

        let json = serde_json::to_string(&issue).unwrap();
        let parsed: Issue = serde_json::from_str(&json).unwrap();

        assert_eq!(issue, parsed);
    }

    #[test]
    fn test_filter_default() {
        let filter = IssueFilter::default();
        assert!(filter.state.is_none());
        assert!(filter.limit.is_none());
    }
}
