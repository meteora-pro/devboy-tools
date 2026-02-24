//! Provider traits for external services.
//!
//! These traits define the interface for interacting with issue trackers
//! and merge request systems like GitLab, GitHub, ClickUp, and Jira.

use async_trait::async_trait;

use crate::error::{Error, Result};
use crate::types::{
    Comment, CreateCommentInput, CreateIssueInput, CreateMergeRequestInput, Discussion, FileDiff,
    Issue, IssueFilter, MergeRequest, MrFilter, UpdateIssueInput, User,
};

/// Provider for working with issues.
///
/// Implementations include GitLab, GitHub, ClickUp, and Jira providers.
#[async_trait]
pub trait IssueProvider: Send + Sync {
    /// Get a list of issues with optional filters.
    async fn get_issues(&self, filter: IssueFilter) -> Result<Vec<Issue>>;

    /// Get a single issue by key (e.g., "gitlab#123", "gh#456").
    async fn get_issue(&self, key: &str) -> Result<Issue>;

    /// Create a new issue.
    async fn create_issue(&self, input: CreateIssueInput) -> Result<Issue>;

    /// Update an existing issue.
    async fn update_issue(&self, key: &str, input: UpdateIssueInput) -> Result<Issue>;

    /// Get comments for an issue.
    async fn get_comments(&self, issue_key: &str) -> Result<Vec<Comment>>;

    /// Add a comment to an issue.
    async fn add_comment(&self, issue_key: &str, body: &str) -> Result<Comment>;

    /// Get the provider name for logging (e.g., "gitlab", "github").
    fn provider_name(&self) -> &'static str;
}

/// Provider for working with merge requests / pull requests.
///
/// Only `provider_name()` is required. All other methods have default implementations
/// that return `Error::ProviderUnsupported`, so providers like ClickUp and Jira
/// only need to override the methods they actually support.
#[async_trait]
pub trait MergeRequestProvider: Send + Sync {
    /// Get the provider name for logging.
    fn provider_name(&self) -> &'static str;

    /// Get a list of merge requests with optional filters.
    async fn get_merge_requests(&self, _filter: MrFilter) -> Result<Vec<MergeRequest>> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "get_merge_requests".to_string(),
        })
    }

    /// Get a single merge request by key (e.g., "mr#123", "pr#456").
    async fn get_merge_request(&self, _key: &str) -> Result<MergeRequest> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "get_merge_request".to_string(),
        })
    }

    /// Get discussions/comments for a merge request.
    async fn get_discussions(&self, _mr_key: &str) -> Result<Vec<Discussion>> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "get_discussions".to_string(),
        })
    }

    /// Get file diffs for a merge request.
    async fn get_diffs(&self, _mr_key: &str) -> Result<Vec<FileDiff>> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "get_diffs".to_string(),
        })
    }

    /// Add a comment to a merge request.
    async fn add_comment(&self, _mr_key: &str, _input: CreateCommentInput) -> Result<Comment> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "add_merge_request_comment".to_string(),
        })
    }

    /// Create a new merge request / pull request.
    async fn create_merge_request(&self, _input: CreateMergeRequestInput) -> Result<MergeRequest> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "create_merge_request".to_string(),
        })
    }
}

/// Combined provider trait for services that support both issues and merge requests.
///
/// This is implemented by GitLab and GitHub providers.
#[async_trait]
pub trait Provider: IssueProvider + MergeRequestProvider {
    /// Get the current authenticated user.
    async fn get_current_user(&self) -> Result<User>;
}
