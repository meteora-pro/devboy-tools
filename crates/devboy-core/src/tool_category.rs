//! Tool categories for capability-based filtering.
//!
//! Providers declare which categories they support.
//! Tools are tagged with a category. Only tools from
//! supported categories are shown in `list_tools()`.

use serde::{Deserialize, Serialize};

/// Category of MCP tools — determines which tools are available
/// based on provider capabilities.
///
/// Variant order is also the canonical display order used by
/// auto-generated documentation (`devboy tools docs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    /// Issue tracker tools: issues CRUD, comments, statuses.
    /// Providers: GitLab, GitHub, ClickUp, Jira, YouGile, Linear
    IssueTracker,

    /// Git repository tools: MR/PR, pipeline, diffs, discussions.
    /// Providers: GitLab, GitHub
    GitRepository,

    /// Epic tools: high-level tasks with goals and progress.
    /// Providers: ClickUp
    Epics,

    /// Release tools: tags, releases, assets.
    /// Providers: GitLab, GitHub
    Releases,

    /// Meeting notes tools: transcripts, summaries, search.
    /// Providers: Fireflies
    MeetingNotes,

    /// Knowledge base tools: spaces, pages, search, page updates.
    /// Providers: Confluence
    KnowledgeBase,

    /// Messenger tools: chats, messages, search, sending.
    /// Providers: Slack
    Messenger,

    /// Jira Structure plugin tools: hierarchies, views, formulas.
    /// Providers: Jira (requires Structure plugin)
    JiraStructure,
}

impl ToolCategory {
    /// Stable, human-readable display name used in documentation.
    pub fn display_name(self) -> &'static str {
        match self {
            ToolCategory::IssueTracker => "Issue Tracker",
            ToolCategory::GitRepository => "Git Repository",
            ToolCategory::Epics => "Epics",
            ToolCategory::Releases => "Releases",
            ToolCategory::MeetingNotes => "Meeting Notes",
            ToolCategory::KnowledgeBase => "Knowledge Base",
            ToolCategory::Messenger => "Messenger",
            ToolCategory::JiraStructure => "Jira Structure",
        }
    }

    /// Snake-case key used in JSON output (matches `serde(rename_all)`).
    pub fn key(self) -> &'static str {
        match self {
            ToolCategory::IssueTracker => "issue_tracker",
            ToolCategory::GitRepository => "git_repository",
            ToolCategory::Epics => "epics",
            ToolCategory::Releases => "releases",
            ToolCategory::MeetingNotes => "meeting_notes",
            ToolCategory::KnowledgeBase => "knowledge_base",
            ToolCategory::Messenger => "messenger",
            ToolCategory::JiraStructure => "jira_structure",
        }
    }
}
