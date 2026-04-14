//! Provider traits for external services.
//!
//! These traits define the interface for interacting with issue trackers
//! and merge request systems like GitLab, GitHub, ClickUp, and Jira.

use async_trait::async_trait;

use crate::asset::{AssetCapabilities, AssetMeta};
use crate::error::{Error, Result};
use crate::types::{
    Comment, CreateCommentInput, CreateIssueInput, CreateMergeRequestInput, Discussion, FileDiff,
    GetPipelineInput, GetUsersOptions, Issue, IssueFilter, IssueRelations, IssueStatus,
    JobLogOptions, JobLogOutput, MeetingFilter, MeetingNote, MeetingTranscript, MergeRequest,
    MrFilter, PipelineInfo, ProviderResult, Release, UpdateIssueInput, UpdateMergeRequestInput,
    User,
};

/// Provider for working with issues.
///
/// Implementations include GitLab, GitHub, ClickUp, and Jira providers.
#[async_trait]
pub trait IssueProvider: Send + Sync {
    /// Get a list of issues with optional filters.
    async fn get_issues(&self, filter: IssueFilter) -> Result<ProviderResult<Issue>>;

    /// Get a single issue by key (e.g., "gitlab#123", "gh#456").
    async fn get_issue(&self, key: &str) -> Result<Issue>;

    /// Create a new issue.
    async fn create_issue(&self, input: CreateIssueInput) -> Result<Issue>;

    /// Update an existing issue.
    async fn update_issue(&self, key: &str, input: UpdateIssueInput) -> Result<Issue>;

    /// Get comments for an issue.
    async fn get_comments(&self, issue_key: &str) -> Result<ProviderResult<Comment>>;

    /// Add a comment to an issue.
    async fn add_comment(&self, issue_key: &str, body: &str) -> Result<Comment>;

    /// Get available statuses for the issue tracker.
    /// Default returns ProviderUnsupported — override in providers that support statuses.
    async fn get_statuses(&self) -> Result<ProviderResult<IssueStatus>> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "get_statuses".to_string(),
        })
    }

    /// Link two issues together.
    async fn link_issues(
        &self,
        _source_key: &str,
        _target_key: &str,
        _link_type: &str,
    ) -> Result<()> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "link_issues".to_string(),
        })
    }

    /// Remove a link between two issues.
    async fn unlink_issues(
        &self,
        _source_key: &str,
        _target_key: &str,
        _link_type: &str,
    ) -> Result<()> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "unlink_issues".to_string(),
        })
    }

    /// Get users from the issue tracker (Jira only).
    async fn get_users(&self, _options: GetUsersOptions) -> Result<ProviderResult<User>> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "get_users".to_string(),
        })
    }

    /// Upload a file attachment to an issue. Returns the download URL.
    /// Default returns ProviderUnsupported — override in providers that support attachments.
    async fn upload_attachment(
        &self,
        _issue_key: &str,
        _filename: &str,
        _data: &[u8],
    ) -> Result<String> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "upload_attachment".to_string(),
        })
    }

    /// List attachments currently attached to an issue (body + comments).
    ///
    /// Returns provider-agnostic [`AssetMeta`] values. Default returns
    /// ProviderUnsupported; providers that can parse or fetch their own
    /// attachment listings override this.
    async fn get_issue_attachments(&self, _issue_key: &str) -> Result<Vec<AssetMeta>> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "get_issue_attachments".to_string(),
        })
    }

    /// Download the raw bytes of an attachment belonging to an issue.
    ///
    /// `asset_id` is the provider-specific identifier returned from
    /// [`IssueProvider::get_issue_attachments`] (ClickUp attachment id,
    /// Jira attachment id, GitLab upload URL, etc.).
    async fn download_attachment(&self, _issue_key: &str, _asset_id: &str) -> Result<Vec<u8>> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "download_attachment".to_string(),
        })
    }

    /// Delete an attachment from an issue.
    ///
    /// Not all providers expose a delete endpoint for attachments (ClickUp
    /// doesn't, GitLab file uploads are immutable) — the default returns
    /// `ProviderUnsupported` and callers can consult [`asset_capabilities`]
    /// beforehand.
    async fn delete_attachment(&self, _issue_key: &str, _asset_id: &str) -> Result<()> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "delete_attachment".to_string(),
        })
    }

    /// Describe which asset operations this provider supports for each
    /// context. Used by the enricher to surface per-provider capabilities
    /// in tool schemas so agents can adapt their behaviour before making
    /// calls that would fail with `ProviderUnsupported`.
    fn asset_capabilities(&self) -> AssetCapabilities {
        AssetCapabilities::default()
    }

    /// Set custom fields on an issue. Each entry: {"id": "field_id", "value": <value>}.
    /// Default is no-op — override in providers that support custom fields (e.g., ClickUp).
    async fn set_custom_fields(
        &self,
        _issue_key: &str,
        _fields: &[serde_json::Value],
    ) -> Result<()> {
        Ok(()) // No-op by default
    }

    /// Get issue relations (parent, subtasks, linked issues).
    async fn get_issue_relations(&self, _issue_key: &str) -> Result<IssueRelations> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "get_issue_relations".to_string(),
        })
    }

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
    async fn get_merge_requests(&self, _filter: MrFilter) -> Result<ProviderResult<MergeRequest>> {
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
    async fn get_discussions(&self, _mr_key: &str) -> Result<ProviderResult<Discussion>> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "get_discussions".to_string(),
        })
    }

    /// Get file diffs for a merge request.
    async fn get_diffs(&self, _mr_key: &str) -> Result<ProviderResult<FileDiff>> {
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

    /// Update an existing merge request / pull request.
    async fn update_merge_request(
        &self,
        _key: &str,
        _input: UpdateMergeRequestInput,
    ) -> Result<MergeRequest> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "update_merge_request".to_string(),
        })
    }

    /// Get releases/tags for the repository.
    async fn get_releases(&self) -> Result<ProviderResult<Release>> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "get_releases".to_string(),
        })
    }

    /// List attachments on a merge request (body + discussions).
    async fn get_mr_attachments(&self, _mr_key: &str) -> Result<Vec<AssetMeta>> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "get_mr_attachments".to_string(),
        })
    }

    /// Download an attachment from a merge request.
    async fn download_mr_attachment(&self, _mr_key: &str, _asset_id: &str) -> Result<Vec<u8>> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "download_mr_attachment".to_string(),
        })
    }

    /// Delete an attachment from a merge request.
    async fn delete_mr_attachment(&self, _mr_key: &str, _asset_id: &str) -> Result<()> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "delete_mr_attachment".to_string(),
        })
    }
}

/// Provider for CI/CD pipeline status and job logs.
///
/// Implemented by GitLab (Pipelines API) and GitHub (Actions API).
/// All methods have default implementations returning `ProviderUnsupported`.
#[async_trait]
pub trait PipelineProvider: Send + Sync {
    /// Get the provider name for logging.
    fn provider_name(&self) -> &'static str;

    /// Get pipeline status for a branch or MR/PR.
    async fn get_pipeline(&self, _input: GetPipelineInput) -> Result<PipelineInfo> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "get_pipeline".to_string(),
        })
    }

    /// Get job logs with search, pagination, or smart extraction.
    async fn get_job_logs(&self, _job_id: &str, _options: JobLogOptions) -> Result<JobLogOutput> {
        Err(Error::ProviderUnsupported {
            provider: self.provider_name().to_string(),
            operation: "get_job_logs".to_string(),
        })
    }
}

/// Combined provider trait for services that support issues, merge requests, and pipelines.
///
/// This is implemented by GitLab and GitHub providers.
#[async_trait]
pub trait Provider: IssueProvider + MergeRequestProvider + PipelineProvider {
    /// Get the current authenticated user.
    async fn get_current_user(&self) -> Result<User>;
}

/// Provider for meeting notes and transcripts.
///
/// Implementations include Fireflies.ai.
#[async_trait]
pub trait MeetingNotesProvider: Send + Sync {
    /// Get the provider name for logging (e.g., "fireflies").
    fn provider_name(&self) -> &'static str;

    /// Get a list of meeting notes with optional filters.
    async fn get_meetings(&self, filter: MeetingFilter) -> Result<ProviderResult<MeetingNote>>;

    /// Get the full transcript for a meeting.
    async fn get_transcript(&self, meeting_id: &str) -> Result<MeetingTranscript>;

    /// Search meetings by keyword across titles, action items, keywords, and topics.
    async fn search_meetings(
        &self,
        query: &str,
        filter: MeetingFilter,
    ) -> Result<ProviderResult<MeetingNote>>;
}
