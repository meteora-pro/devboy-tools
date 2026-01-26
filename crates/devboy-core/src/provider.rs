//! Provider trait for git hosting services.

use async_trait::async_trait;

use crate::error::Result;
use crate::types::{Issue, MergeRequest, User};

/// Trait for git hosting providers (GitLab, GitHub, etc.)
#[async_trait]
pub trait Provider: Send + Sync {
    /// Get the provider name (e.g., "gitlab", "github")
    fn name(&self) -> &str;

    /// Get issues from the provider
    async fn get_issues(&self, state: Option<&str>) -> Result<Vec<Issue>>;

    /// Get a single issue by ID
    async fn get_issue(&self, id: u64) -> Result<Issue>;

    /// Get merge requests / pull requests
    async fn get_merge_requests(&self, state: Option<&str>) -> Result<Vec<MergeRequest>>;

    /// Get a single merge request by ID
    async fn get_merge_request(&self, id: u64) -> Result<MergeRequest>;

    /// Get current authenticated user
    async fn get_current_user(&self) -> Result<User>;
}
