//! Provider traits for external services.
//!
//! These traits define the interface for interacting with issue trackers
//! and merge request systems like GitLab, GitHub, ClickUp, and Jira.

use async_trait::async_trait;

use crate::asset::{AssetCapabilities, AssetMeta};
use crate::error::{Error, Result};
#[cfg(test)]
use crate::types::JobLogMode;
use crate::types::{
    Comment, CreateCommentInput, CreateIssueInput, CreateMergeRequestInput, Discussion, FileDiff,
    GetChatsParams, GetMessagesParams, GetPipelineInput, GetUsersOptions, Issue, IssueFilter,
    IssueRelations, IssueStatus, JobLogOptions, JobLogOutput, MeetingFilter, MeetingNote,
    MeetingTranscript, MergeRequest, MessengerChat, MessengerMessage, MrFilter, PipelineInfo,
    ProviderResult, Release, SearchMessagesParams, SendMessageParams, UpdateIssueInput,
    UpdateMergeRequestInput, User,
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

/// Provider for team messenger systems.
///
/// Implementations include Slack.
#[async_trait]
pub trait MessengerProvider: Send + Sync {
    /// Get the provider name for logging (e.g. "slack").
    fn provider_name(&self) -> &'static str;

    /// Get available chats, channels, groups, or DMs.
    async fn get_chats(&self, params: GetChatsParams) -> Result<ProviderResult<MessengerChat>>;

    /// Get message history for a specific chat.
    async fn get_messages(
        &self,
        params: GetMessagesParams,
    ) -> Result<ProviderResult<MessengerMessage>>;

    /// Search messages across chats.
    async fn search_messages(
        &self,
        params: SearchMessagesParams,
    ) -> Result<ProviderResult<MessengerMessage>>;

    /// Send a message to a chat or thread.
    async fn send_message(&self, params: SendMessageParams) -> Result<MessengerMessage>;
}

// ============================================================================
// Default-method coverage
// ============================================================================
//
// The traits above expose a lot of default methods that return
// `ProviderUnsupported` so that concrete providers only have to
// override what they actually implement. The unit tests below pin the
// contract of that default set so a future refactor cannot silently
// turn an unsupported operation into a panic, a silent success, or a
// wrong error variant.

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Provider` that only overrides `provider_name()` and nothing
    /// else — every other method should fall through to the default
    /// `ProviderUnsupported` return.
    struct DummyProvider;

    #[async_trait]
    impl IssueProvider for DummyProvider {
        async fn get_issues(&self, _: IssueFilter) -> Result<ProviderResult<Issue>> {
            unreachable!("the dispatcher should never call this in these tests")
        }
        async fn get_issue(&self, _: &str) -> Result<Issue> {
            unreachable!()
        }
        async fn create_issue(&self, _: CreateIssueInput) -> Result<Issue> {
            unreachable!()
        }
        async fn update_issue(&self, _: &str, _: UpdateIssueInput) -> Result<Issue> {
            unreachable!()
        }
        async fn get_comments(&self, _: &str) -> Result<ProviderResult<Comment>> {
            unreachable!()
        }
        async fn add_comment(&self, _: &str, _: &str) -> Result<Comment> {
            unreachable!()
        }
        fn provider_name(&self) -> &'static str {
            "dummy"
        }
    }

    #[async_trait]
    impl MergeRequestProvider for DummyProvider {
        fn provider_name(&self) -> &'static str {
            "dummy"
        }
    }

    #[async_trait]
    impl PipelineProvider for DummyProvider {
        fn provider_name(&self) -> &'static str {
            "dummy"
        }
    }

    /// Assert that a result is `ProviderUnsupported { provider, operation }`
    /// and that both fields carry the expected values.
    fn assert_unsupported<T: std::fmt::Debug>(result: Result<T>, expected_op: &str) {
        match result {
            Err(Error::ProviderUnsupported {
                provider,
                operation,
            }) => {
                assert_eq!(provider, "dummy");
                assert_eq!(operation, expected_op);
            }
            other => panic!("expected ProviderUnsupported({expected_op}), got {other:?}"),
        }
    }

    // ------------------------------------------------------------------
    // IssueProvider defaults
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn issue_provider_defaults_return_unsupported() {
        let p = DummyProvider;

        assert_unsupported(p.get_statuses().await, "get_statuses");
        assert_unsupported(p.link_issues("a", "b", "blocks").await, "link_issues");
        assert_unsupported(p.unlink_issues("a", "b", "blocks").await, "unlink_issues");
        assert_unsupported(p.get_users(GetUsersOptions::default()).await, "get_users");
        assert_unsupported(
            p.upload_attachment("k", "f.png", b"x").await,
            "upload_attachment",
        );
        assert_unsupported(p.get_issue_attachments("k").await, "get_issue_attachments");
        assert_unsupported(p.download_attachment("k", "1").await, "download_attachment");
        assert_unsupported(p.delete_attachment("k", "1").await, "delete_attachment");
        assert_unsupported(p.get_issue_relations("k").await, "get_issue_relations");
    }

    #[tokio::test]
    async fn issue_provider_set_custom_fields_is_no_op_by_default() {
        // Distinct from every other default: this one returns Ok(()).
        let p = DummyProvider;
        p.set_custom_fields("k", &[]).await.unwrap();
    }

    #[test]
    fn issue_provider_default_asset_capabilities_is_empty() {
        let caps = IssueProvider::asset_capabilities(&DummyProvider);
        assert_eq!(caps, AssetCapabilities::default());
    }

    // ------------------------------------------------------------------
    // MergeRequestProvider defaults
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn merge_request_provider_defaults_return_unsupported() {
        let p = DummyProvider;
        assert_unsupported(
            p.get_merge_requests(MrFilter::default()).await,
            "get_merge_requests",
        );
        assert_unsupported(p.get_merge_request("mr#1").await, "get_merge_request");
        assert_unsupported(p.get_discussions("mr#1").await, "get_discussions");
        assert_unsupported(p.get_diffs("mr#1").await, "get_diffs");
        assert_unsupported(
            MergeRequestProvider::add_comment(
                &p,
                "mr#1",
                CreateCommentInput {
                    body: "".into(),
                    position: None,
                    discussion_id: None,
                },
            )
            .await,
            "add_merge_request_comment",
        );
        assert_unsupported(
            p.create_merge_request(CreateMergeRequestInput::default())
                .await,
            "create_merge_request",
        );
        assert_unsupported(
            p.update_merge_request("mr#1", UpdateMergeRequestInput::default())
                .await,
            "update_merge_request",
        );
        assert_unsupported(p.get_releases().await, "get_releases");
        assert_unsupported(p.get_mr_attachments("mr#1").await, "get_mr_attachments");
        assert_unsupported(
            p.download_mr_attachment("mr#1", "1").await,
            "download_mr_attachment",
        );
        assert_unsupported(
            p.delete_mr_attachment("mr#1", "1").await,
            "delete_mr_attachment",
        );
    }

    // ------------------------------------------------------------------
    // PipelineProvider defaults
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn pipeline_provider_defaults_return_unsupported() {
        let p = DummyProvider;
        assert_unsupported(
            p.get_pipeline(GetPipelineInput::default()).await,
            "get_pipeline",
        );
        assert_unsupported(
            p.get_job_logs(
                "1",
                JobLogOptions {
                    mode: JobLogMode::Smart,
                },
            )
            .await,
            "get_job_logs",
        );
    }
}
