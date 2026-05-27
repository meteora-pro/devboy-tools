//! Common types used across providers.
//!
//! These types are provider-agnostic and represent unified data structures
//! that can be populated from GitLab, GitHub, ClickUp, or Jira APIs.

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    pub title: String,
    pub description: Option<String>,
    /// State (e.g., "opened", "closed")
    pub state: String,
    /// Source provider name (e.g., "gitlab", "github", "clickup", "jira")
    pub source: String,
    /// Priority (e.g., "urgent", "high", "normal", "low")
    pub priority: Option<String>,
    /// Labels / tags
    pub labels: Vec<String>,
    pub author: Option<User>,
    pub assignees: Vec<User>,
    /// Web URL for the issue
    pub url: Option<String>,
    /// Created at timestamp (ISO 8601)
    pub created_at: Option<String>,
    /// Updated at timestamp (ISO 8601)
    pub updated_at: Option<String>,
    /// Number of file attachments on this issue (populated from the
    /// provider response when available, without extra API calls).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments_count: Option<u32>,
    /// Parent issue key (for subtasks)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Subtasks / child issues
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtasks: Vec<Issue>,
    /// Provider-specific custom fields. **Key is always the
    /// provider-stable id** (Jira `customfield_10014`, ClickUp field
    /// uuid) so downstream consumers can join with metadata from
    /// `get_custom_fields` regardless of provider. Display name
    /// rides along inside the value when the provider returns it
    /// on the issue payload — Jira leaves `name` empty (do a
    /// follow-up `get_custom_fields` call if you need it), ClickUp
    /// fills it from `task.custom_fields[].name`. GitHub and GitLab
    /// don't have a customfield concept and always return an empty
    /// map here.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub custom_fields: std::collections::HashMap<String, CustomFieldValue>,
}

/// Value of a single entry in [`Issue::custom_fields`]. Splits the
/// raw provider value from the optional human-readable name so
/// downstream consumers don't have to parse one out of the other.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CustomFieldValue {
    /// Human-readable field name (e.g. `"Epic Link"`,
    /// `"Severity"`). `None` when the issue payload didn't carry a
    /// name — Jira's `/issue/{key}` returns customfield values
    /// keyed only by id, so the mapper leaves this empty and
    /// relies on `get_custom_fields` for resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Raw provider value. Shape varies — string for text, number
    /// for numeric, object/array for selects and multi-selects.
    pub value: Value,
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
    /// Parent epic key (Jira-only). Populated when the issue has an
    /// `Epic Link` customfield value — covers Server/DC and Cloud
    /// company-managed projects. Cloud team-managed Epics use the
    /// system `parent` field instead and surface there. Lightweight
    /// key-only field on purpose: a full `Issue` lookup for every
    /// relations call would be too expensive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epic_key: Option<String>,
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
    /// Project key override (e.g., "PROJ"). When set, overrides the default project
    /// configured for the provider. Ignored by providers that don't support it.
    pub project_key: Option<String>,
    /// Native query passed directly to the provider (e.g., Jira JQL).
    /// Takes precedence over other filters. If the query doesn't contain a project
    /// clause, the provider may inject one automatically.
    pub native_query: Option<String>,
}

/// Input for creating a new issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateIssueInput {
    pub title: String,
    pub description: Option<String>,
    /// Labels to add
    pub labels: Vec<String>,
    /// Assignee usernames
    pub assignees: Vec<String>,
    pub priority: Option<String>,
    /// Parent issue key (for creating subtasks, e.g., "CU-abc123" or "DEV-42")
    pub parent: Option<String>,
    /// Whether the description is markdown (default: true).
    /// When true, providers that support it (e.g., ClickUp) will use
    /// markdown rendering for the description.
    #[serde(default = "default_true")]
    pub markdown: bool,
    /// Project key for issue creation (e.g., "PROJ"). Overrides default project.
    /// Ignored by providers that don't support multi-project (GitHub, GitLab, ClickUp).
    pub project_id: Option<String>,
    /// Issue type (e.g., "Task", "Bug", "Story"). Provider-specific.
    /// Jira defaults to "Task" if not specified.
    pub issue_type: Option<String>,
    /// Provider-specific custom fields.
    /// Jira: Object `{"customfield_10001": value}` — merged into create payload.
    /// ClickUp: Array `[{"id": "field_id", "value": val}]` — set via separate API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_fields: Option<Value>,
    /// Component **names** to associate with the issue (Jira-only, issue
    /// #197). Each entry is a component name obtained from project
    /// metadata (`JiraComponent.name`). Jira accepts both id- and
    /// name-based references in `fields.components`; we use names to line
    /// up with the schema enricher, which enumerates component *names* in
    /// the `components` enum. Ignored by providers that don't have a
    /// first-class Components concept (GitHub/GitLab/ClickUp).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
    /// Fix version **names** to associate with the issue (Jira-only).
    /// Each entry is a release name from the project's versions
    /// (`ProjectVersion.name`, e.g. `"3.18.0"`). Jira accepts both id-
    /// and name-based references in `fields.fixVersions`; names line up
    /// with the schema enricher and stay portable across Cloud and
    /// Self-Hosted. Ignored by providers without first-class fix
    /// versions (GitHub/GitLab/ClickUp).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fix_versions: Vec<String>,
    /// Parent epic key (Jira-only). Maps to the instance's `Epic Link`
    /// custom field — id resolved at runtime, no need for callers to
    /// know the `customfield_*` number. On Cloud team-managed projects
    /// the canonical Epic Link is the system `parent` field; passing
    /// the same value via `epic_key` works there too because Jira
    /// accepts the customfield as an alias.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epic_key: Option<String>,
    /// Sprint id (Jira-only). Maps to the instance's `Sprint` custom
    /// field. Numeric, not name — Jira's Sprint field stores agile
    /// board sprint ids; use `get_board_sprints` to discover them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sprint_id: Option<i64>,
    /// Epic name (Jira-only, required when `issue_type == "Epic"` on
    /// Server/DC and Cloud company-managed projects). Maps to the
    /// instance's `Epic Name` custom field. Cloud team-managed
    /// projects use the regular `summary` and ignore this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epic_name: Option<String>,
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
            project_id: None,
            issue_type: None,
            custom_fields: None,
            components: Vec::new(),
            fix_versions: Vec::new(),
            epic_key: None,
            sprint_id: None,
            epic_name: None,
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
    /// New state (generic open/closed). For provider-specific custom
    /// statuses (e.g. ClickUp "in progress" / "review") use
    /// [`Self::status`] — that field takes precedence.
    pub state: Option<String>,
    /// Provider-specific status name. ClickUp routes this directly to
    /// `PUT /task/:id { "status": ... }`; the value must match one of
    /// the statuses returned by `get_available_statuses` (case-
    /// insensitive, the canonical name is sent). Other providers
    /// currently ignore this field — see issue #288 for the path to
    /// generic transitions for Jira / GitLab / GitHub.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
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
    /// Provider-specific custom fields (same format as CreateIssueInput).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_fields: Option<Value>,
    /// Replace components on the issue (Jira-only, issue #197).
    /// `None` leaves components untouched. `Some(vec![])` clears all
    /// components. `Some(vec![...])` replaces with the given component
    /// **names** (see [`CreateIssueInput::components`] for rationale).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub components: Option<Vec<String>>,
    /// Replace fix versions on the issue (Jira-only).
    /// `None` leaves fix versions untouched. `Some(vec![])` clears all
    /// fix versions. `Some(vec![...])` replaces with the given release
    /// **names** (see [`CreateIssueInput::fix_versions`] for rationale).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix_versions: Option<Vec<String>>,
    /// Set the parent epic (Jira-only). `None` leaves the link
    /// untouched. `Some("PROJ-1")` replaces it. To detach an issue
    /// from its epic, pass `customFields: { "<epic-link-cf>": null }`
    /// — the `epic_key` slot is for setting, not clearing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epic_key: Option<String>,
    /// Move the issue to a sprint (Jira-only). `None` leaves the
    /// sprint untouched. See [`CreateIssueInput::sprint_id`] for the
    /// id-vs-name rationale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sprint_id: Option<i64>,
    /// Set the Epic Name (Jira-only, applies to Epic-typed issues).
    /// `None` leaves it untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epic_name: Option<String>,
}

impl Default for UpdateIssueInput {
    fn default() -> Self {
        Self {
            title: None,
            description: None,
            state: None,
            status: None,
            labels: None,
            assignees: None,
            priority: None,
            parent_id: None,
            markdown: true,
            custom_fields: None,
            components: None,
            fix_versions: None,
            epic_key: None,
            sprint_id: None,
            epic_name: None,
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
    pub title: String,
    pub description: Option<String>,
    /// State (e.g., "opened", "closed", "merged")
    pub state: String,
    /// Source provider name
    pub source: String,
    pub source_branch: String,
    pub target_branch: String,
    pub author: Option<User>,
    pub assignees: Vec<User>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateMergeRequestInput {
    pub title: String,
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

/// Input for updating an existing merge request / pull request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateMergeRequestInput {
    /// New title (None = keep current).
    pub title: Option<String>,
    /// New description / body (None = keep current).
    pub description: Option<String>,
    /// New state: "close" or "reopen" (None = keep current).
    pub state: Option<String>,
    /// New labels (None = keep current).
    pub labels: Option<Vec<String>>,
    /// Mark as draft / WIP (None = keep current).
    pub draft: Option<bool>,
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
    pub id: String,
    /// Comment body / text
    pub body: String,
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
    pub file_path: String,
    pub line: u32,
    /// Line type ("old" for deleted, "new" for added)
    pub line_type: String,
    pub commit_sha: Option<String>,
}

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
    pub new_file: bool,
    pub deleted_file: bool,
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
    /// Provider pagination cursor/token for fetching the next page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
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
            next_cursor: Some("cursor-1".into()),
        };
        let result = ProviderResult::new(vec!["a", "b"]).with_pagination(pagination);
        assert_eq!(result.items, vec!["a", "b"]);
        let pag = result.pagination.unwrap();
        assert_eq!(pag.total, Some(100));
        assert!(pag.has_more);
        assert_eq!(pag.offset, 0);
        assert_eq!(pag.limit, 10);
        assert_eq!(pag.next_cursor.as_deref(), Some("cursor-1"));
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
                next_cursor: Some("cursor-2".into()),
            })
            .with_sort_info(SortInfo {
                sort_by: Some("priority".into()),
                sort_order: SortOrder::Asc,
                available_sorts: vec![],
            });
        assert!(result.pagination.is_some());
        assert!(result.sort_info.is_some());
        assert_eq!(result.items, vec!["x"]);
        assert_eq!(
            result
                .pagination
                .as_ref()
                .and_then(|pagination| pagination.next_cursor.as_deref()),
            Some("cursor-2")
        );
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

#[derive(Debug, Clone, Default)]
pub struct GetUsersOptions {
    pub user_id: Option<String>,
    pub project_key: Option<String>,
    pub search: Option<String>,
    pub include_inactive: Option<bool>,
    pub start_at: Option<u32>,
    /// Pub.
    pub max_results: Option<u32>,
}

// =============================================================================
// Project Versions (Jira fixVersion / release marker)
// =============================================================================

/// A project-scoped release marker (Jira "version" / `fixVersion`).
///
/// Provider-agnostic abstraction over the Jira Versions API; other
/// providers (GitLab milestones, GitHub releases) could implement the
/// same surface in the future. Carries enough fields up-front to avoid
/// per-id follow-up calls (Paper 3 — Context Enrichment Hypothesis).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProjectVersion {
    /// Provider-native identifier (Jira: numeric id as string).
    pub id: String,
    /// Project key this version belongs to (e.g. `"PROJ"`).
    pub project: String,
    /// Human-readable version name (e.g. `"3.18.0"`).
    pub name: String,
    /// Release notes / description.
    pub description: Option<String>,
    /// Planned start date (ISO 8601 date, no time).
    pub start_date: Option<String>,
    /// Planned or actual release date (ISO 8601 date, no time).
    pub release_date: Option<String>,
    /// Whether the version has been released.
    pub released: bool,
    /// Whether the version has been archived.
    pub archived: bool,
    /// Provider-computed flag — true when `release_date` is in the past
    /// and the version is still unreleased. Optional because the
    /// provider may not return it.
    pub overdue: Option<bool>,
    /// Total number of issues attached to this version, when the
    /// provider can report it.
    ///
    /// Provider semantics differ:
    /// - **Jira Cloud** populates this from `?expand=issuesstatus`
    ///   (sum across all status categories — true total).
    /// - **Jira Self-Hosted / DC** does *not* populate this on the
    ///   listing endpoint — `unresolved_issue_count` carries what the
    ///   server returns instead. Fetching a real total there requires
    ///   a per-version `/version/{id}/relatedIssueCounts` call which
    ///   we deliberately don't make on the list path.
    pub issue_count: Option<u32>,
    /// Number of *unresolved* issues attached to this version. Set on
    /// Jira Self-Hosted/DC from the base payload; on Cloud the same
    /// number is buried inside the per-status breakdown so we don't
    /// re-derive it here. Kept separate from `issue_count` so callers
    /// can't accidentally compare an unresolved-count against a total.
    pub unresolved_issue_count: Option<u32>,
    /// Source provider name (e.g. `"jira"`).
    pub source: String,
}

/// Filters / pagination for `IssueProvider::list_project_versions`.
#[derive(Debug, Clone, Default)]
pub struct ListProjectVersionsParams {
    /// Project key to scope the query (required).
    pub project: String,
    /// `Some(true)` → only released; `Some(false)` → only unreleased;
    /// `None` → both.
    pub released: Option<bool>,
    /// `Some(true)` → only archived; `Some(false)` → only non-archived;
    /// `None` → both. Tool layer defaults this to `Some(false)` to
    /// hide archival noise.
    pub archived: Option<bool>,
    /// Maximum results to return after filtering. Tool layer defaults to 20.
    pub limit: Option<u32>,
    /// Whether to fetch per-version `issue_count` (extra round-trip
    /// or `?expand=issuesstatus` on Cloud).
    pub include_issue_count: bool,
}

/// Provider-agnostic descriptor for a custom field on an issue
/// tracker — Jira's `customfield_*` numbers, GitLab's resource
/// labels, ClickUp's "custom field" entities. Only the lowest common
/// denominator (`id`, `name`, `field_type`) is unified; provider
/// specifics live under `native` so callers that need them (e.g.
/// downstream codegen) can read them without losing fidelity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomFieldDescriptor {
    /// Provider-specific id (e.g. `"customfield_10014"` on Jira).
    pub id: String,
    /// Human-readable name (e.g. `"Epic Link"`).
    pub name: String,
    /// Normalised type tag (`"string"`, `"array"`, `"number"`, ...).
    /// Empty string when the provider didn't expose a schema.
    pub field_type: String,
    /// Optional description copied from the provider, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Provider-native schema details preserved verbatim — Jira's
    /// `JiraFieldSchema` (custom URI, customId), ClickUp's option
    /// list, GitHub's enum values. Opaque to the unified layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<Value>,
}

/// Filters / pagination for `IssueProvider::list_custom_fields`.
#[derive(Debug, Clone, Default)]
pub struct ListCustomFieldsParams {
    /// Project key to scope the query — used by providers that
    /// expose project-scoped customfields. Ignored on Jira global
    /// `/field` endpoint.
    pub project: Option<String>,
    /// Issue type to scope the query (Jira create-screen contexts).
    /// Ignored when the provider returns a flat global list.
    pub issue_type: Option<String>,
    /// Case-insensitive substring filter on the field name. Useful
    /// for "find the Epic Link customfield" without listing every
    /// field on the instance.
    pub search: Option<String>,
    /// Maximum results after filtering. Tool layer caps this at 200
    /// to honour Paper-1 token budgets.
    pub limit: Option<u32>,
}

/// Input to `IssueProvider::upsert_project_version`.
///
/// Resolves `(project, name)` → existing version id. If found, the
/// provider issues a partial update preserving any fields not
/// explicitly set here (Optional means "don't touch"). If not found,
/// the version is created.
#[derive(Debug, Clone, Default)]
pub struct UpsertProjectVersionInput {
    /// Project key (required).
    pub project: String,
    /// Version name — used both as the lookup key and as the value
    /// when creating (required).
    pub name: String,
    /// Release notes / description.
    pub description: Option<String>,
    /// Planned start date (ISO 8601 date).
    pub start_date: Option<String>,
    /// Planned or actual release date (ISO 8601 date).
    pub release_date: Option<String>,
    /// Mark released / unreleased.
    pub released: Option<bool>,
    /// Archive / unarchive.
    pub archived: Option<bool>,
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
    /// Pub.
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

#[derive(Debug, Clone, Default)]
pub struct GetPipelineInput {
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
    pub duration_seconds: Option<u64>,
    pub host_email: Option<String>,
    pub organizer_email: Option<String>,
    /// Participant identifiers (emails, names, or display names depending on provider)
    pub participants: Vec<String>,
    /// Speaker names
    pub speakers: Vec<MeetingSpeaker>,
    /// AI-extracted action items
    pub action_items: Vec<String>,
    /// Keywords / topics
    pub keywords: Vec<String>,
    pub topics_discussed: Vec<String>,
    /// Meeting type (e.g., "standup", "planning")
    pub meeting_type: Option<String>,
    /// AI summary overview
    pub summary: Option<String>,
    pub transcript_url: Option<String>,
    /// Audio recording URL
    pub audio_url: Option<String>,
    /// Video recording URL
    pub video_url: Option<String>,
    /// Meeting link (e.g., Zoom/Google Meet URL)
    pub meeting_link: Option<String>,
}

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

// =============================================================================
// Knowledge Base
// =============================================================================

/// A knowledge base space / wiki container.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct KbSpace {
    /// Provider-specific space ID.
    pub id: String,
    /// Stable human-facing space key.
    pub key: String,
    /// Space display name.
    pub name: String,
    /// Provider-specific space type.
    pub space_type: Option<String>,
    /// Current status, if exposed.
    pub status: Option<String>,
    /// Optional description / summary.
    pub description: Option<String>,
    /// Canonical web URL.
    pub url: Option<String>,
}

/// A knowledge base page summary.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct KbPage {
    /// Provider-specific page ID.
    pub id: String,
    /// Page title.
    pub title: String,
    /// Owning space key when available.
    pub space_key: Option<String>,
    pub url: Option<String>,
    /// Current page version.
    pub version: Option<u32>,
    /// Last modification timestamp.
    pub last_modified: Option<String>,
    pub author: Option<String>,
    /// Short excerpt / snippet.
    pub excerpt: Option<String>,
}

/// Full page content plus metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct KbPageContent {
    /// Summary page metadata.
    pub page: KbPage,
    /// Page body content.
    pub content: String,
    /// Content representation: e.g. "markdown", "html", "storage".
    pub content_type: String,
    pub ancestors: Vec<KbPage>,
    /// Page labels/tags.
    pub labels: Vec<String>,
}

/// A comment on a knowledge base page.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct KbComment {
    /// Provider-specific comment ID.
    pub id: String,
    /// Page ID this comment belongs to.
    pub page_id: String,
    /// Comment body.
    pub content: String,
    /// Human-readable author name.
    pub author: Option<String>,
    /// Comment creation timestamp.
    pub created_at: Option<String>,
    /// Comment version/revision when available.
    pub version: Option<u32>,
}

/// Parameters for listing pages within a space.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListPagesParams {
    /// Space key to list within.
    pub space_key: String,
    /// Maximum number of pages to return.
    pub limit: Option<u32>,
    /// Number of results to skip when offset pagination is supported.
    pub offset: Option<u32>,
    /// Provider pagination cursor/token.
    pub cursor: Option<String>,
    /// Optional free-text title/content filter.
    pub search: Option<String>,
    pub parent_id: Option<String>,
}

/// Parameters for creating a new page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreatePageParams {
    /// Target space key.
    pub space_key: String,
    /// Page title.
    pub title: String,
    /// Page body content.
    pub content: String,
    pub content_type: Option<String>,
    pub parent_id: Option<String>,
    /// Labels to set on the page.
    pub labels: Vec<String>,
}

/// Parameters for updating an existing page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdatePageParams {
    /// Page ID to update.
    pub page_id: String,
    /// New title, if changing.
    pub title: Option<String>,
    pub content: Option<String>,
    /// Content representation supplied by the caller.
    pub content_type: Option<String>,
    /// Expected current version for optimistic locking.
    pub version: Option<u32>,
    /// New labels when replacing labels explicitly.
    pub labels: Option<Vec<String>>,
    /// Optional new parent page ID.
    pub parent_id: Option<String>,
}

/// Parameters for searching a knowledge base.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchKbParams {
    /// Free-text query or provider-native search expression.
    pub query: String,
    /// Restrict search to a specific space key.
    pub space_key: Option<String>,
    /// Provider pagination cursor/token.
    pub cursor: Option<String>,
    /// Maximum number of matches to return.
    pub limit: Option<u32>,
    #[serde(default)]
    pub raw_query: bool,
}

// =============================================================================
// Messenger
// =============================================================================

/// Chat type in a messenger provider.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChatType {
    /// One-to-one direct message.
    Direct,
    /// Multi-user private conversation.
    Group,
    /// Public or private channel-like room.
    #[default]
    Channel,
}

/// Unified messenger chat/channel representation.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MessengerChat {
    /// Provider-specific chat ID.
    pub id: String,
    /// Stable display key for the chat.
    pub key: String,
    /// Human-readable chat name.
    pub name: String,
    /// Kind of chat.
    pub chat_type: ChatType,
    /// Source provider name (e.g. "slack").
    pub source: String,
    pub member_count: Option<u32>,
    /// Optional chat/topic description.
    pub description: Option<String>,
    /// Whether the chat is currently active / not archived.
    pub is_active: bool,
}

/// Author metadata for a messenger message.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MessageAuthor {
    /// Provider-specific user ID.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Username/handle when available.
    pub username: Option<String>,
    pub avatar_url: Option<String>,
}

/// Attached file, link, or rich object on a message.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MessageAttachment {
    /// Provider-specific attachment ID when available.
    pub id: Option<String>,
    /// Attachment title or filename.
    pub name: Option<String>,
    /// Attachment type/kind (e.g. "file", "image", "link").
    pub attachment_type: Option<String>,
    /// Direct or canonical URL.
    pub url: Option<String>,
    /// Optional mime type.
    pub mime_type: Option<String>,
    /// Optional file size in bytes.
    pub file_size: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MessengerMessage {
    /// Provider-specific message ID.
    pub id: String,
    /// Chat ID this message belongs to.
    pub chat_id: String,
    /// Message text/body.
    pub text: String,
    /// Message author.
    pub author: MessageAuthor,
    pub source: String,
    pub timestamp: String,
    /// Parent thread identifier when this message is in a thread.
    pub thread_id: Option<String>,
    /// Direct parent/replied-to message identifier when available.
    pub reply_to_id: Option<String>,
    /// Attachments associated with the message.
    pub attachments: Vec<MessageAttachment>,
    /// Whether the message was edited after creation.
    pub is_edited: bool,
    /// Provider-specific edit timestamp when available.
    pub edit_date: Option<String>,
}

/// Parameters for listing available messenger chats.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetChatsParams {
    /// Optional free-text search/filter for chat names.
    pub search: Option<String>,
    /// Restrict to a specific chat type.
    pub chat_type: Option<ChatType>,
    /// Maximum number of results to return.
    pub limit: Option<u32>,
    /// Cursor/token for provider pagination.
    pub cursor: Option<String>,
    /// Include inactive/archived chats.
    pub include_inactive: Option<bool>,
}

/// Parameters for reading message history from a chat.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetMessagesParams {
    /// Chat/channel ID to fetch from.
    pub chat_id: String,
    /// Maximum number of messages to return.
    pub limit: Option<u32>,
    /// Cursor/token for provider pagination.
    pub cursor: Option<String>,
    /// Optional thread identifier to fetch replies for a thread.
    pub thread_id: Option<String>,
    pub since: Option<String>,
    /// Only include messages before this provider timestamp string.
    pub until: Option<String>,
}

/// Parameters for cross-chat message search.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SearchMessagesParams {
    /// Search query/keyword.
    pub query: String,
    /// Restrict search to a specific chat.
    pub chat_id: Option<String>,
    /// Maximum number of matches to return.
    pub limit: Option<u32>,
    /// Cursor/token for provider pagination.
    pub cursor: Option<String>,
    /// Only include messages after this provider timestamp string.
    pub since: Option<String>,
    pub until: Option<String>,
}

/// Parameters for sending a new message.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SendMessageParams {
    /// Destination chat/channel ID.
    pub chat_id: String,
    /// Message body text.
    pub text: String,
    /// Optional thread identifier to post as a threaded reply.
    pub thread_id: Option<String>,
    pub reply_to_id: Option<String>,
    /// Optional attachments to include or reference.
    pub attachments: Vec<MessageAttachment>,
}

// =============================================================================
// Jira Structure (plugin)
// =============================================================================

/// A Jira Structure container.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Structure {
    /// Structure ID
    pub id: u64,
    /// Structure name
    pub name: String,
    /// Structure description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A node in a Structure forest tree.
///
/// Transformed from the compact `rows[] + depths[]` API format
/// into a nested tree with `children` for LLM readability.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StructureNode {
    /// Row ID within the structure
    pub row_id: u64,
    /// Item identifier (e.g., Jira issue key "PROJ-123")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    /// Item type (e.g., "issue", "folder")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_type: Option<String>,
    /// Child nodes
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<StructureNode>,
}

/// A Structure forest — the hierarchy tree for a Structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StructureForest {
    /// Forest version (for optimistic concurrency)
    pub version: u64,
    pub structure_id: u64,
    /// Root nodes of the tree
    pub tree: Vec<StructureNode>,
    /// Total number of rows in the structure
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_count: Option<u64>,
}

/// Result of a forest modification (add/move rows).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ForestModifyResult {
    /// New forest version after modification
    pub version: u64,
    /// Number of rows affected
    pub affected_count: usize,
}

/// A saved view for a Structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StructureView {
    /// View ID
    pub id: u64,
    /// View name
    pub name: String,
    /// Structure ID this view belongs to
    pub structure_id: u64,
    /// Column definitions
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<StructureViewColumn>,
    /// Group by field
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_by: Option<String>,
    /// Sort by field
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_by: Option<String>,
    /// JQL filter
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StructureViewColumn {
    /// Column ID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Jira field name (e.g., "summary", "status")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Structure Expr formula (e.g., "SUM(\"Story Points\")")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formula: Option<String>,
    /// Column width in pixels
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
}

/// Batch values for Structure rows × columns.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StructureValues {
    pub structure_id: u64,
    /// Values matrix: row_id → column values
    pub values: Vec<StructureRowValues>,
}

/// Values for a single row.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StructureRowValues {
    pub row_id: u64,
    /// Column values (column_id/field → value)
    pub columns: Vec<StructureColumnValue>,
}

/// A single column value.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct StructureColumnValue {
    /// Column identifier (field name or formula)
    pub column: String,
    /// The value (can be string, number, null, etc.)
    pub value: Value,
}

// --- Structure input types ---

/// Options for getting a structure forest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetForestOptions {
    /// Offset for pagination
    pub offset: Option<u64>,
    /// Max rows to return (default: 200)
    pub limit: Option<u64>,
}

/// Input for adding rows to a structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AddStructureRowsInput {
    /// Items to add
    pub items: Vec<StructureRowItem>,
    /// Parent row ID (items become children)
    pub under: Option<u64>,
    /// Sibling row ID (items placed after)
    pub after: Option<u64>,
    /// Forest version for optimistic locking
    pub forest_version: Option<u64>,
}

/// A single item to add to a structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StructureRowItem {
    /// Jira issue key or folder name
    pub item_id: String,
    /// Item type: "issue" (default) or "folder"
    pub item_type: Option<String>,
}

/// Input for moving rows within a structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MoveStructureRowsInput {
    /// Row IDs to move
    pub row_ids: Vec<u64>,
    /// New parent row ID
    pub under: Option<u64>,
    /// Sibling row ID (placed after)
    pub after: Option<u64>,
    /// Forest version for optimistic locking
    pub forest_version: Option<u64>,
}

/// Input for reading structure values.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GetStructureValuesInput {
    pub structure_id: u64,
    /// Row IDs to read
    pub rows: Vec<u64>,
    /// Columns to read
    pub columns: Vec<StructureViewColumn>,
}

/// Input for saving a structure view.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SaveStructureViewInput {
    /// View ID (omit to create new)
    pub id: Option<u64>,
    pub structure_id: u64,
    /// View name
    pub name: String,
    /// Column definitions
    pub columns: Option<Vec<StructureViewColumn>>,
    /// Group by field
    pub group_by: Option<String>,
    /// Sort by field
    pub sort_by: Option<String>,
    pub filter: Option<String>,
}

/// Input for creating a new structure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateStructureInput {
    /// Structure name
    pub name: String,
    /// Structure description
    pub description: Option<String>,
}

// =============================================================================
// Agile (Sprint) types — issue #198
// =============================================================================

/// A Jira Agile Sprint as returned by `/rest/agile/1.0/board/{id}/sprint`.
///
/// Jira serialises fields in `camelCase` — `origin_board_id` matches
/// `originBoardId`, `start_date` matches `startDate`, etc.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Sprint {
    pub id: u64,
    pub name: String,
    /// `future`, `active`, or `closed`.
    pub state: String,
    /// Board this sprint belongs to (when returned from a board query).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_board_id: Option<u64>,
    /// Planned start of the sprint, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_date: Option<String>,
    /// Planned end of the sprint, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_date: Option<String>,
    /// Sprint goal, if set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
}

/// Filter applied when listing sprints for a board.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SprintState {
    Active,
    Future,
    Closed,
    All,
}

impl SprintState {
    pub fn as_query_value(&self) -> Option<&'static str> {
        match self {
            SprintState::Active => Some("active"),
            SprintState::Future => Some("future"),
            SprintState::Closed => Some("closed"),
            // `all` → omit the state query param, Jira returns every sprint.
            SprintState::All => None,
        }
    }
}

/// Input for moving a set of issues onto a sprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignToSprintInput {
    /// Target sprint id.
    pub sprint_id: u64,
    /// Issue keys to move (e.g. `["PROJ-1", "PROJ-2"]`).
    pub issue_keys: Vec<String>,
}

// =============================================================================
// Structure plugin extensions — issues #179 / #180
// =============================================================================

/// Reference to a Structure "generator" — the rule engine that auto-populates
/// rows from JQL / Agile boards / manual lists. Issue #179.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructureGenerator {
    /// Generator id within the structure.
    pub id: String,
    /// Generator type (e.g. `"jql"`, `"agile-board"`, `"insert"`).
    #[serde(rename = "type")]
    pub generator_type: String,
    /// Arbitrary generator-specific configuration — opaque to us.
    #[serde(default)]
    pub spec: Value,
}

/// Input for `add_structure_generator` (issue #179).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddStructureGeneratorInput {
    pub structure_id: u64,
    /// Generator type, e.g. `"jql"` or `"agile-board"`.
    #[serde(rename = "type")]
    pub generator_type: String,
    /// Generator configuration passed through verbatim.
    pub spec: Value,
}

/// Input for `sync_structure_generator` (issue #179).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStructureGeneratorInput {
    pub structure_id: u64,
    pub generator_id: String,
}

/// Input for `update_structure_automation` (issue #180).
///
/// Structure plugin exposes automation as a collection of rules —
/// `PUT /rest/structure/2.0/structure/{structureId}/automation/{automationId}`
/// replaces a specific rule, and a bare
/// `PUT /rest/structure/2.0/structure/{structureId}/automation` replaces
/// the entire set. We support both via `automation_id` being optional:
/// `Some(id)` → replaces that rule, `None` → replaces all rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateStructureAutomationInput {
    pub structure_id: u64,
    /// Automation rule id. `None` means replace the entire automation
    /// collection on the structure (legacy "set everything" behaviour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    /// Automation payload — passed through verbatim so we don't need to
    /// mirror every field Structure supports.
    pub config: Value,
}
