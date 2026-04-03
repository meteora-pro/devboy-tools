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
    /// Parent issue key (for subtasks)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Subtasks / child issues
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtasks: Vec<Issue>,
}

/// A link between two issues.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IssueLink {
    /// The linked issue as a full [`Issue`]. In many providers this will only be
    /// partially populated (often just key, title, state, and source), but all
    /// fields are allowed when available.
    pub issue: Issue,
    /// Link type name (e.g., "Blocks", "Relates", "Duplicates")
    pub link_type: String,
}

/// All relations for a single issue.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct IssueRelations {
    /// Parent issue (if this is a subtask)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<Issue>,
    /// Child issues / subtasks
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtasks: Vec<Issue>,
    /// Issues that block this one
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<IssueLink>,
    /// Issues that this one blocks
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<IssueLink>,
    /// Related issues
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_to: Vec<IssueLink>,
    /// Duplicate issues
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub duplicates: Vec<IssueLink>,
}

/// Filter parameters for listing issues.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IssueFilter {
    /// Filter by state (e.g., "opened", "closed", "all")
    pub state: Option<String>,
    /// Filter by semantic state category (e.g., "backlog", "todo", "in_progress", "done", "cancelled").
    /// Maps to provider-specific statuses using name heuristics.
    pub state_category: Option<String>,
    /// Search query for title and description
    pub search: Option<String>,
    /// Filter by labels
    pub labels: Option<Vec<String>>,
    /// Label matching logic: "and" requires all labels, "or" requires any (default: "or")
    pub labels_operator: Option<String>,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Parent issue key (for creating subtasks, e.g., "CU-abc123" or "DEV-42")
    pub parent: Option<String>,
    /// Whether the description is markdown (default: true).
    /// When true, providers that support it (e.g., ClickUp) will use
    /// markdown rendering for the description.
    #[serde(default = "default_true")]
    pub markdown: bool,
}

impl Default for CreateIssueInput {
    fn default() -> Self {
        Self {
            title: String::new(),
            description: None,
            labels: Vec::new(),
            assignees: Vec::new(),
            priority: None,
            parent: None,
            markdown: true,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Input for updating an existing issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Parent issue key (for moving task to subtask, e.g., "CU-abc123" or "DEV-42").
    /// Set to `"none"` to detach from parent (convert subtask back to standalone task).
    /// Empty string is treated as detach (same as `"none"`).
    /// Not supported by all providers.
    pub parent_id: Option<String>,
    /// Whether the description is markdown (default: true).
    #[serde(default = "default_true")]
    pub markdown: bool,
}

impl Default for UpdateIssueInput {
    fn default() -> Self {
        Self {
            title: None,
            description: None,
            state: None,
            labels: None,
            assignees: None,
            priority: None,
            parent_id: None,
            markdown: true,
        }
    }
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

/// Input for creating a merge request / pull request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateMergeRequestInput {
    /// Title of the merge request
    pub title: String,
    /// Description / body
    pub description: Option<String>,
    /// Source branch (head)
    pub source_branch: String,
    /// Target branch (base)
    pub target_branch: String,
    /// Whether to create as draft/WIP
    pub draft: bool,
    /// Labels to add
    pub labels: Vec<String>,
    /// Reviewer usernames
    pub reviewers: Vec<String>,
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
    /// Number of results to skip (offset)
    pub offset: Option<u32>,
    /// Sort by field (e.g., "created_at", "updated_at")
    pub sort_by: Option<String>,
    /// Sort order ("asc" or "desc")
    pub sort_order: Option<String>,
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

// =============================================================================
// Sort Info
// =============================================================================

/// Sort direction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortOrder {
    Asc,
    #[default]
    Desc,
}

/// Sorting metadata from provider API.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SortInfo {
    /// Current sort field (e.g., "updated_at", "created_at")
    pub sort_by: Option<String>,
    /// Current sort order
    pub sort_order: SortOrder,
    /// Available sort fields for this endpoint
    pub available_sorts: Vec<String>,
}

// =============================================================================
// Provider Result
// =============================================================================

/// Wrapper for provider list responses with pagination and sorting metadata.
///
/// Providers return this instead of plain `Vec<T>` to convey API-level
/// pagination state and sorting info to the format pipeline.
#[derive(Debug, Clone, Default)]
pub struct ProviderResult<T> {
    /// The actual items returned by the provider
    pub items: Vec<T>,
    /// Pagination metadata from the API (total count, has_more, etc.)
    pub pagination: Option<Pagination>,
    /// Sorting metadata (current sort, available sort fields)
    pub sort_info: Option<SortInfo>,
}

impl<T> ProviderResult<T> {
    /// Create a new ProviderResult with just items (no metadata).
    pub fn new(items: Vec<T>) -> Self {
        Self {
            items,
            pagination: None,
            sort_info: None,
        }
    }

    /// Set pagination metadata.
    pub fn with_pagination(mut self, pagination: Pagination) -> Self {
        self.pagination = Some(pagination);
        self
    }

    /// Set sort info metadata.
    pub fn with_sort_info(mut self, sort_info: SortInfo) -> Self {
        self.sort_info = Some(sort_info);
        self
    }
}

impl<T> From<Vec<T>> for ProviderResult<T> {
    fn from(items: Vec<T>) -> Self {
        Self::new(items)
    }
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
    fn test_issue_parent_subtasks_serialization() {
        let child = Issue {
            key: "DEV-101".to_string(),
            title: "Child".to_string(),
            state: "open".to_string(),
            source: "clickup".to_string(),
            parent: Some("parent123".to_string()),
            ..Default::default()
        };

        let parent = Issue {
            key: "DEV-100".to_string(),
            title: "Parent".to_string(),
            state: "open".to_string(),
            source: "clickup".to_string(),
            subtasks: vec![child],
            ..Default::default()
        };

        let json = serde_json::to_string(&parent).unwrap();
        assert!(json.contains("\"subtasks\""));
        assert!(json.contains("DEV-101"));
        assert!(!json.contains("\"parent\":null")); // parent=None is skipped

        let parsed: Issue = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.subtasks.len(), 1);
        assert_eq!(parsed.subtasks[0].key, "DEV-101");
        assert_eq!(parsed.subtasks[0].parent, Some("parent123".to_string()));
    }

    #[test]
    fn test_issue_no_subtasks_skipped_in_json() {
        let issue = Issue {
            key: "DEV-200".to_string(),
            title: "No children".to_string(),
            state: "open".to_string(),
            source: "clickup".to_string(),
            ..Default::default()
        };

        let json = serde_json::to_string(&issue).unwrap();
        // subtasks: [] should be skipped, parent: None should be skipped
        assert!(!json.contains("subtasks"));
        assert!(!json.contains("parent"));
    }

    #[test]
    fn test_issue_deserialize_without_parent_subtasks() {
        // JSON from providers that don't have parent/subtasks
        let json = r#"{
            "key": "gitlab#1",
            "title": "Test",
            "state": "open",
            "source": "gitlab",
            "labels": [],
            "assignees": []
        }"#;

        let issue: Issue = serde_json::from_str(json).unwrap();
        assert!(issue.parent.is_none());
        assert!(issue.subtasks.is_empty());
    }

    #[test]
    fn test_filter_default() {
        let filter = IssueFilter::default();
        assert!(filter.state.is_none());
        assert!(filter.limit.is_none());
    }

    #[test]
    fn test_pipeline_status_display() {
        assert_eq!(PipelineStatus::Success.as_str(), "success");
        assert_eq!(PipelineStatus::Failed.as_str(), "failed");
        assert_eq!(PipelineStatus::Running.as_str(), "running");
    }

    // --- ProviderResult tests ---

    #[test]
    fn test_provider_result_new() {
        let result = ProviderResult::new(vec![1, 2, 3]);
        assert_eq!(result.items, vec![1, 2, 3]);
        assert!(result.pagination.is_none());
        assert!(result.sort_info.is_none());
    }

    #[test]
    fn test_provider_result_with_pagination() {
        let pagination = Pagination {
            offset: 0,
            limit: 10,
            total: Some(100),
            has_more: true,
        };
        let result = ProviderResult::new(vec!["a", "b"]).with_pagination(pagination);
        assert_eq!(result.items, vec!["a", "b"]);
        let pag = result.pagination.unwrap();
        assert_eq!(pag.total, Some(100));
        assert!(pag.has_more);
        assert_eq!(pag.offset, 0);
        assert_eq!(pag.limit, 10);
    }

    #[test]
    fn test_provider_result_with_sort_info() {
        let sort_info = SortInfo {
            sort_by: Some("updated_at".into()),
            sort_order: SortOrder::Desc,
            available_sorts: vec!["created_at".into(), "updated_at".into()],
        };
        let result = ProviderResult::new(vec![42]).with_sort_info(sort_info);
        assert_eq!(result.items, vec![42]);
        let si = result.sort_info.unwrap();
        assert_eq!(si.sort_by, Some("updated_at".into()));
        assert_eq!(si.sort_order, SortOrder::Desc);
        assert_eq!(si.available_sorts.len(), 2);
    }

    #[test]
    fn test_provider_result_from_vec() {
        let items = vec![1, 2, 3, 4];
        let result: ProviderResult<i32> = items.into();
        assert_eq!(result.items, vec![1, 2, 3, 4]);
        assert!(result.pagination.is_none());
        assert!(result.sort_info.is_none());
    }

    #[test]
    fn test_provider_result_chained() {
        let result = ProviderResult::new(vec!["x"])
            .with_pagination(Pagination {
                offset: 10,
                limit: 5,
                total: Some(50),
                has_more: true,
            })
            .with_sort_info(SortInfo {
                sort_by: Some("priority".into()),
                sort_order: SortOrder::Asc,
                available_sorts: vec![],
            });
        assert!(result.pagination.is_some());
        assert!(result.sort_info.is_some());
        assert_eq!(result.items, vec!["x"]);
    }
}

// =============================================================================
// Issue Status
// =============================================================================

/// Available status in an issue tracker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueStatus {
    pub id: String,
    pub name: String,
    /// Normalized category for cross-provider compatibility.
    pub category: String,
    pub color: Option<String>,
    pub order: Option<u32>,
}

/// Options for get_users.
#[derive(Debug, Clone, Default)]
pub struct GetUsersOptions {
    pub user_id: Option<String>,
    pub project_key: Option<String>,
    pub search: Option<String>,
    pub include_inactive: Option<bool>,
    pub start_at: Option<u32>,
    pub max_results: Option<u32>,
}

// =============================================================================
// Releases
// =============================================================================

/// A release/tag from a git repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    pub id: String,
    pub key: String,
    pub tag_name: String,
    pub name: String,
    pub description: Option<String>,
    pub source: String,
    pub url: Option<String>,
    pub author: Option<User>,
    pub is_draft: Option<bool>,
    pub is_prerelease: Option<bool>,
    pub assets: Vec<ReleaseAsset>,
    pub created_at: Option<String>,
    pub published_at: Option<String>,
}

/// Asset attached to a release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub url: String,
    pub size: Option<u64>,
    pub download_count: Option<u64>,
}

// =============================================================================
// Pipeline / CI
// =============================================================================

/// CI/CD pipeline status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStatus {
    Success,
    Failed,
    Running,
    Pending,
    Canceled,
    Skipped,
    Unknown,
}

impl PipelineStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Running => "running",
            Self::Pending => "pending",
            Self::Canceled => "canceled",
            Self::Skipped => "skipped",
            Self::Unknown => "unknown",
        }
    }

    pub fn is_healthy(&self) -> bool {
        matches!(self, Self::Success)
    }
}

/// Summary counts of jobs in a pipeline.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PipelineSummary {
    pub total: u32,
    pub success: u32,
    pub failed: u32,
    pub running: u32,
    pub pending: u32,
    pub canceled: u32,
    pub skipped: u32,
}

/// A CI/CD pipeline with jobs grouped by stage/workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineInfo {
    pub id: String,
    pub status: PipelineStatus,
    /// Branch or tag ref.
    pub reference: String,
    pub sha: String,
    pub url: Option<String>,
    /// Duration in seconds.
    pub duration: Option<u64>,
    pub coverage: Option<f64>,
    pub summary: PipelineSummary,
    /// Jobs grouped by stage (GitLab) or workflow (GitHub).
    pub stages: Vec<PipelineStage>,
    /// Failed jobs with extracted error snippets.
    pub failed_jobs: Vec<FailedJob>,
}

/// A stage/workflow in the pipeline containing jobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStage {
    pub name: String,
    pub jobs: Vec<PipelineJob>,
}

/// A single job in a pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineJob {
    pub id: String,
    pub name: String,
    pub status: PipelineStatus,
    pub url: Option<String>,
    /// Duration in seconds.
    pub duration: Option<u64>,
}

/// A failed job with extracted error context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedJob {
    pub id: String,
    pub name: String,
    pub url: Option<String>,
    /// Extracted error lines from the job log.
    pub error_snippet: Option<String>,
}

/// Input for get_pipeline.
#[derive(Debug, Clone, Default)]
pub struct GetPipelineInput {
    /// Branch name (e.g., "main", "feat/DEV-123").
    pub branch: Option<String>,
    /// MR/PR key (e.g., "mr#123", "pr#456"). Takes priority over branch.
    pub mr_key: Option<String>,
    /// Include smart error extraction for failed jobs.
    pub include_failed_logs: bool,
}

/// Options for get_job_logs.
#[derive(Debug, Clone)]
pub struct JobLogOptions {
    pub mode: JobLogMode,
}

/// Job log retrieval mode.
#[derive(Debug, Clone)]
pub enum JobLogMode {
    /// Automatic smart error extraction.
    Smart,
    /// Search with regex/keyword pattern.
    Search {
        pattern: String,
        context: usize,
        max_matches: usize,
    },
    /// Browse specific line range.
    Paginated { offset: usize, limit: usize },
    /// Full log (can be large).
    Full { max_lines: usize },
}

/// Result of job log retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobLogOutput {
    pub job_id: String,
    pub job_name: Option<String>,
    pub content: String,
    pub mode: String,
    pub total_lines: Option<usize>,
}

// =============================================================================
// Meeting Notes
// =============================================================================

/// Represents a meeting note / transcript summary.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MeetingNote {
    /// Unique identifier from the provider
    pub id: String,
    /// Meeting title
    pub title: String,
    /// Meeting date (ISO 8601)
    pub meeting_date: Option<String>,
    /// Duration in seconds
    pub duration_seconds: Option<u64>,
    /// Host email
    pub host_email: Option<String>,
    /// Organizer email
    pub organizer_email: Option<String>,
    /// Participant identifiers (emails, names, or display names depending on provider)
    pub participants: Vec<String>,
    /// Speaker names
    pub speakers: Vec<MeetingSpeaker>,
    /// AI-extracted action items
    pub action_items: Vec<String>,
    /// Keywords / topics
    pub keywords: Vec<String>,
    /// Topics discussed
    pub topics_discussed: Vec<String>,
    /// Meeting type (e.g., "standup", "planning")
    pub meeting_type: Option<String>,
    /// AI summary overview
    pub summary: Option<String>,
    /// Transcript URL
    pub transcript_url: Option<String>,
    /// Audio recording URL
    pub audio_url: Option<String>,
    /// Video recording URL
    pub video_url: Option<String>,
    /// Meeting link (e.g., Zoom/Google Meet URL)
    pub meeting_link: Option<String>,
}

/// A speaker in a meeting.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MeetingSpeaker {
    pub id: String,
    pub name: String,
}

/// Full meeting transcript with speaker-attributed sentences.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MeetingTranscript {
    /// Meeting ID this transcript belongs to
    pub meeting_id: String,
    /// Meeting title
    pub title: Option<String>,
    /// Speaker-attributed sentences
    pub sentences: Vec<TranscriptSentence>,
}

/// A single sentence in a meeting transcript.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TranscriptSentence {
    /// Speaker ID (maps to MeetingSpeaker.id)
    pub speaker_id: String,
    /// Speaker name (resolved from speakers list)
    pub speaker_name: Option<String>,
    /// Sentence text
    pub text: String,
    /// Start time in seconds
    pub start_time: f64,
    /// End time in seconds
    pub end_time: f64,
}

/// Filter for listing meeting notes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MeetingFilter {
    /// Search keyword
    pub keyword: Option<String>,
    /// Filter from date (ISO 8601)
    pub from_date: Option<String>,
    /// Filter to date (ISO 8601)
    pub to_date: Option<String>,
    /// Filter by participant emails
    pub participants: Option<Vec<String>>,
    /// Filter by host email
    pub host_email: Option<String>,
    /// Max results
    pub limit: Option<u32>,
    /// Skip N results
    pub skip: Option<u32>,
}
