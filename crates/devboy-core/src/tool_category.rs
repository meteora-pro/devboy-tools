//! Tool categories for capability-based filtering.
//!
//! Providers declare which categories they support.
//! Tools are tagged with a category. Only tools from
//! supported categories are shown in `list_tools()`.

use serde::{Deserialize, Serialize};

/// Category of MCP tools — determines which tools are available
/// based on provider capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    /// Git repository tools: MR/PR, pipeline, diffs, discussions.
    /// Providers: GitLab, GitHub
    GitRepository,

    /// Issue tracker tools: issues CRUD, comments, statuses.
    /// Providers: GitLab, GitHub, ClickUp, Jira, YouGile, Linear
    IssueTracker,

    /// Epic tools: high-level tasks with goals and progress.
    /// Providers: ClickUp
    Epics,

    /// Release tools: tags, releases, assets.
    /// Providers: GitLab, GitHub
    Releases,

    /// Meeting notes tools: transcripts, summaries, search.
    /// Providers: Fireflies
    MeetingNotes,

    /// Messenger tools: chats, messages, search, sending.
    /// Providers: Slack
    Messenger,
}
