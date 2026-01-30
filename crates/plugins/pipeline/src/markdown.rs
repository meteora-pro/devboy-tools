//! Markdown conversion for token-efficient output.
//!
//! Converts structured data (Issues, MRs, Diffs) to Markdown format,
//! reducing token usage by ~50-70% compared to JSON.
//!
//! # Format Comparison
//!
//! | Format   | Typical Tokens | Use Case                    |
//! |----------|---------------|------------------------------|
//! | JSON     | ~2000         | Machine processing           |
//! | Markdown | ~500          | LLM reading, human readable  |
//! | Compact  | ~200          | Quick overview, listing      |

use devboy_core::{Comment, Discussion, FileDiff, Issue, MergeRequest};

/// Configuration for markdown output.
#[derive(Debug, Clone)]
pub struct MarkdownConfig {
    /// Include timestamps in output
    pub include_timestamps: bool,
    /// Include URLs in output
    pub include_urls: bool,
    /// Include author information
    pub include_author: bool,
    /// Maximum description length before truncation
    pub max_description_len: usize,
}

impl Default for MarkdownConfig {
    fn default() -> Self {
        Self {
            include_timestamps: true,
            include_urls: true,
            include_author: true,
            max_description_len: 200,
        }
    }
}

/// Markdown plugin for converting structured data to Markdown.
pub struct MarkdownPlugin {
    config: MarkdownConfig,
}

impl MarkdownPlugin {
    /// Create a new markdown plugin with default config.
    pub fn new() -> Self {
        Self {
            config: MarkdownConfig::default(),
        }
    }

    /// Create a markdown plugin with custom config.
    pub fn with_config(config: MarkdownConfig) -> Self {
        Self { config }
    }
}

impl Default for MarkdownPlugin {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Issues
// ============================================================================

/// Convert issues to Markdown format.
pub fn issues_to_markdown(issues: &[Issue]) -> String {
    if issues.is_empty() {
        return "No issues found.".to_string();
    }

    let mut output = String::new();
    output.push_str("# Issues\n\n");

    for issue in issues {
        output.push_str(&issue_to_markdown(issue));
        output.push('\n');
    }

    output
}

/// Convert a single issue to Markdown.
fn issue_to_markdown(issue: &Issue) -> String {
    let mut output = String::new();

    // Header with key and title
    output.push_str(&format!("## {} - {}\n\n", issue.key, issue.title));

    // Status line
    output.push_str(&format!(
        "**State:** {} | **Source:** {}",
        issue.state, issue.source
    ));

    if let Some(priority) = &issue.priority {
        output.push_str(&format!(" | **Priority:** {}", priority));
    }

    output.push('\n');

    // Labels
    if !issue.labels.is_empty() {
        output.push_str(&format!("**Labels:** {}\n", issue.labels.join(", ")));
    }

    // Author
    if let Some(author) = &issue.author {
        output.push_str(&format!("**Author:** @{}\n", author.username));
    }

    // Assignees
    if !issue.assignees.is_empty() {
        let assignees: Vec<String> = issue
            .assignees
            .iter()
            .map(|a| format!("@{}", a.username))
            .collect();
        output.push_str(&format!("**Assignees:** {}\n", assignees.join(", ")));
    }

    // Description (truncated)
    if let Some(desc) = &issue.description {
        if !desc.is_empty() {
            let truncated = truncate_text(desc, 200);
            output.push_str(&format!("\n{}\n", truncated));
        }
    }

    // URL
    if let Some(url) = &issue.url {
        output.push_str(&format!("\n🔗 {}\n", url));
    }

    output
}

/// Convert issues to compact format (one line per issue).
pub fn issues_to_compact(issues: &[Issue]) -> String {
    if issues.is_empty() {
        return "No issues found.".to_string();
    }

    issues
        .iter()
        .map(|issue| {
            let labels = if issue.labels.is_empty() {
                String::new()
            } else {
                format!(" [{}]", issue.labels.join(", "))
            };
            format!("{} [{}] {}{}", issue.key, issue.state, issue.title, labels)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ============================================================================
// Merge Requests
// ============================================================================

/// Convert merge requests to Markdown format.
pub fn merge_requests_to_markdown(mrs: &[MergeRequest]) -> String {
    if mrs.is_empty() {
        return "No merge requests found.".to_string();
    }

    let mut output = String::new();
    output.push_str("# Merge Requests\n\n");

    for mr in mrs {
        output.push_str(&merge_request_to_markdown(mr));
        output.push('\n');
    }

    output
}

/// Convert a single merge request to Markdown.
fn merge_request_to_markdown(mr: &MergeRequest) -> String {
    let mut output = String::new();

    // Header with key and title
    let draft_marker = if mr.draft { " [DRAFT]" } else { "" };
    output.push_str(&format!("## {}{} - {}\n\n", mr.key, draft_marker, mr.title));

    // Branches
    output.push_str(&format!(
        "**Branch:** `{}` → `{}`\n",
        mr.source_branch, mr.target_branch
    ));

    // Status line
    output.push_str(&format!(
        "**State:** {} | **Source:** {}\n",
        mr.state, mr.source
    ));

    // Labels
    if !mr.labels.is_empty() {
        output.push_str(&format!("**Labels:** {}\n", mr.labels.join(", ")));
    }

    // Author
    if let Some(author) = &mr.author {
        output.push_str(&format!("**Author:** @{}\n", author.username));
    }

    // Assignees
    if !mr.assignees.is_empty() {
        let assignees: Vec<String> = mr
            .assignees
            .iter()
            .map(|a| format!("@{}", a.username))
            .collect();
        output.push_str(&format!("**Assignees:** {}\n", assignees.join(", ")));
    }

    // Reviewers
    if !mr.reviewers.is_empty() {
        let reviewers: Vec<String> = mr
            .reviewers
            .iter()
            .map(|r| format!("@{}", r.username))
            .collect();
        output.push_str(&format!("**Reviewers:** {}\n", reviewers.join(", ")));
    }

    // Description (truncated)
    if let Some(desc) = &mr.description {
        if !desc.is_empty() {
            let truncated = truncate_text(desc, 200);
            output.push_str(&format!("\n{}\n", truncated));
        }
    }

    // URL
    if let Some(url) = &mr.url {
        output.push_str(&format!("\n🔗 {}\n", url));
    }

    output
}

/// Convert merge requests to compact format.
pub fn merge_requests_to_compact(mrs: &[MergeRequest]) -> String {
    if mrs.is_empty() {
        return "No merge requests found.".to_string();
    }

    mrs.iter()
        .map(|mr| {
            let draft = if mr.draft { " [DRAFT]" } else { "" };
            format!(
                "{} [{}]{} {} ({} → {})",
                mr.key, mr.state, draft, mr.title, mr.source_branch, mr.target_branch
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ============================================================================
// File Diffs
// ============================================================================

/// Convert file diffs to Markdown format.
pub fn diffs_to_markdown(diffs: &[FileDiff]) -> String {
    if diffs.is_empty() {
        return "No file changes.".to_string();
    }

    let mut output = String::new();
    output.push_str("# Changed Files\n\n");

    for diff in diffs {
        output.push_str(&diff_to_markdown(diff));
        output.push('\n');
    }

    output
}

/// Convert a single diff to Markdown.
fn diff_to_markdown(diff: &FileDiff) -> String {
    let mut output = String::new();

    // File status indicator
    let status = if diff.new_file {
        "➕"
    } else if diff.deleted_file {
        "➖"
    } else if diff.renamed_file {
        "📝"
    } else {
        "✏️"
    };

    // Header
    output.push_str(&format!("## {} {}\n\n", status, diff.file_path));

    // Rename info
    if diff.renamed_file {
        if let Some(old_path) = &diff.old_path {
            output.push_str(&format!("Renamed from: `{}`\n", old_path));
        }
    }

    // Stats
    if let (Some(adds), Some(dels)) = (diff.additions, diff.deletions) {
        output.push_str(&format!("+{} -{}\n\n", adds, dels));
    }

    // Diff content (in code block)
    if !diff.diff.is_empty() {
        output.push_str("```diff\n");
        output.push_str(&diff.diff);
        if !diff.diff.ends_with('\n') {
            output.push('\n');
        }
        output.push_str("```\n");
    }

    output
}

/// Convert diffs to compact format.
pub fn diffs_to_compact(diffs: &[FileDiff]) -> String {
    if diffs.is_empty() {
        return "No file changes.".to_string();
    }

    diffs
        .iter()
        .map(|diff| {
            let status = if diff.new_file {
                "A"
            } else if diff.deleted_file {
                "D"
            } else if diff.renamed_file {
                "R"
            } else {
                "M"
            };

            let stats = match (diff.additions, diff.deletions) {
                (Some(a), Some(d)) => format!(" (+{} -{}", a, d),
                _ => String::new(),
            };

            format!("[{}] {}{}", status, diff.file_path, stats)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ============================================================================
// Comments
// ============================================================================

/// Convert comments to Markdown format.
pub fn comments_to_markdown(comments: &[Comment]) -> String {
    if comments.is_empty() {
        return "No comments.".to_string();
    }

    let mut output = String::new();
    output.push_str("# Comments\n\n");

    for comment in comments {
        output.push_str(&comment_to_markdown(comment));
        output.push_str("---\n\n");
    }

    output
}

/// Convert a single comment to Markdown.
fn comment_to_markdown(comment: &Comment) -> String {
    let mut output = String::new();

    // Author and timestamp
    if let Some(author) = &comment.author {
        output.push_str(&format!("**@{}**", author.username));
    }
    if let Some(created) = &comment.created_at {
        output.push_str(&format!(" · {}", format_timestamp(created)));
    }
    output.push('\n');

    // Position (for code comments)
    if let Some(pos) = &comment.position {
        output.push_str(&format!("📍 `{}` line {}\n", pos.file_path, pos.line));
    }

    // Body
    output.push('\n');
    output.push_str(&comment.body);
    output.push_str("\n\n");

    output
}

/// Convert comments to compact format.
pub fn comments_to_compact(comments: &[Comment]) -> String {
    if comments.is_empty() {
        return "No comments.".to_string();
    }

    comments
        .iter()
        .map(|c| {
            let author = c
                .author
                .as_ref()
                .map(|a| format!("@{}", a.username))
                .unwrap_or_else(|| "unknown".to_string());
            let body = truncate_text(&c.body, 80);
            format!("{}: {}", author, body)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ============================================================================
// Discussions
// ============================================================================

/// Convert discussions to Markdown format.
pub fn discussions_to_markdown(discussions: &[Discussion]) -> String {
    if discussions.is_empty() {
        return "No discussions.".to_string();
    }

    let mut output = String::new();
    output.push_str("# Discussions\n\n");

    for (i, discussion) in discussions.iter().enumerate() {
        output.push_str(&discussion_to_markdown(discussion, i + 1));
        output.push('\n');
    }

    output
}

/// Convert a single discussion to Markdown.
fn discussion_to_markdown(discussion: &Discussion, index: usize) -> String {
    let mut output = String::new();

    let status = if discussion.resolved {
        "✅ Resolved"
    } else {
        "💬 Open"
    };

    output.push_str(&format!("## Discussion #{} [{}]\n\n", index, status));

    // Position
    if let Some(pos) = &discussion.position {
        output.push_str(&format!("📍 `{}` line {}\n\n", pos.file_path, pos.line));
    }

    // Notes (comments in the discussion)
    for note in &discussion.comments {
        output.push_str(&comment_to_markdown(note));
    }

    output.push_str("---\n");
    output
}

/// Convert discussions to compact format.
pub fn discussions_to_compact(discussions: &[Discussion]) -> String {
    if discussions.is_empty() {
        return "No discussions.".to_string();
    }

    discussions
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let status = if d.resolved { "✅" } else { "💬" };
            let location = d
                .position
                .as_ref()
                .map(|p| format!(" @{}:{}", p.file_path, p.line))
                .unwrap_or_default();
            let note_count = d.comments.len();
            format!("#{} {} {} replies{}", i + 1, status, note_count, location)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ============================================================================
// Helpers
// ============================================================================

/// Truncate text to max length, adding ellipsis if needed.
fn truncate_text(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }

    // Try to break at word boundary
    let truncated = &text[..max_len];
    if let Some(pos) = truncated.rfind(' ') {
        if pos > max_len / 2 {
            return format!("{}...", &text[..pos]);
        }
    }

    format!("{}...", truncated)
}

/// Format a timestamp to a shorter form.
fn format_timestamp(ts: &str) -> String {
    // Just extract the date part if it's ISO format
    if ts.len() >= 10 {
        ts[..10].to_string()
    } else {
        ts.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::User;

    fn sample_user() -> User {
        User {
            id: "1".to_string(),
            username: "testuser".to_string(),
            name: Some("Test User".to_string()),
            email: None,
            avatar_url: None,
        }
    }

    fn sample_issue() -> Issue {
        Issue {
            key: "gh#42".to_string(),
            title: "Fix the bug".to_string(),
            description: Some("This is a description of the bug.".to_string()),
            state: "open".to_string(),
            source: "github".to_string(),
            priority: Some("high".to_string()),
            labels: vec!["bug".to_string(), "urgent".to_string()],
            author: Some(sample_user()),
            assignees: vec![sample_user()],
            url: Some("https://github.com/test/repo/issues/42".to_string()),
            created_at: Some("2024-01-15T10:30:00Z".to_string()),
            updated_at: Some("2024-01-16T14:00:00Z".to_string()),
        }
    }

    #[test]
    fn test_issue_to_markdown() {
        let issue = sample_issue();
        let md = issue_to_markdown(&issue);

        assert!(md.contains("## gh#42"));
        assert!(md.contains("Fix the bug"));
        assert!(md.contains("**State:** open"));
        assert!(md.contains("**Priority:** high"));
        assert!(md.contains("**Labels:** bug, urgent"));
        assert!(md.contains("@testuser"));
    }

    #[test]
    fn test_issues_to_compact() {
        let issues = vec![sample_issue()];
        let compact = issues_to_compact(&issues);

        assert!(compact.contains("gh#42"));
        assert!(compact.contains("[open]"));
        assert!(compact.contains("Fix the bug"));
        assert!(compact.contains("[bug, urgent]"));
    }

    #[test]
    fn test_empty_issues() {
        let md = issues_to_markdown(&[]);
        assert_eq!(md, "No issues found.");

        let compact = issues_to_compact(&[]);
        assert_eq!(compact, "No issues found.");
    }

    #[test]
    fn test_diff_to_markdown() {
        let diff = FileDiff {
            file_path: "src/main.rs".to_string(),
            old_path: None,
            new_file: false,
            deleted_file: false,
            renamed_file: false,
            diff: "+ added line\n- removed line".to_string(),
            additions: Some(1),
            deletions: Some(1),
        };

        let md = diff_to_markdown(&diff);

        assert!(md.contains("✏️ src/main.rs"));
        assert!(md.contains("+1 -1"));
        assert!(md.contains("```diff"));
        assert!(md.contains("+ added line"));
    }

    #[test]
    fn test_diffs_to_compact() {
        let diffs = vec![
            FileDiff {
                file_path: "new.rs".to_string(),
                old_path: None,
                new_file: true,
                deleted_file: false,
                renamed_file: false,
                diff: String::new(),
                additions: Some(10),
                deletions: Some(0),
            },
            FileDiff {
                file_path: "deleted.rs".to_string(),
                old_path: None,
                new_file: false,
                deleted_file: true,
                renamed_file: false,
                diff: String::new(),
                additions: Some(0),
                deletions: Some(5),
            },
        ];

        let compact = diffs_to_compact(&diffs);

        assert!(compact.contains("[A] new.rs"));
        assert!(compact.contains("[D] deleted.rs"));
    }

    #[test]
    fn test_truncate_text() {
        let text = "Hello world this is a test";
        let truncated = truncate_text(text, 15);
        assert!(truncated.ends_with("..."));
        assert!(truncated.len() <= 18);
    }

    #[test]
    fn test_truncate_text_no_truncation() {
        let text = "Short";
        assert_eq!(truncate_text(text, 100), "Short");
    }

    #[test]
    fn test_markdown_vs_json_size() {
        let issues: Vec<Issue> = (1..=5)
            .map(|i| Issue {
                key: format!("gh#{}", i),
                title: format!("Issue number {}", i),
                description: Some("A description".to_string()),
                state: "open".to_string(),
                source: "github".to_string(),
                priority: None,
                labels: vec!["label".to_string()],
                author: Some(sample_user()),
                assignees: vec![],
                url: None,
                created_at: None,
                updated_at: None,
            })
            .collect();

        let json = serde_json::to_string_pretty(&issues).unwrap();
        let markdown = issues_to_markdown(&issues);
        let compact = issues_to_compact(&issues);

        // Markdown should be significantly smaller than JSON
        println!("JSON: {} chars", json.len());
        println!("Markdown: {} chars", markdown.len());
        println!("Compact: {} chars", compact.len());

        assert!(markdown.len() < json.len());
        assert!(compact.len() < markdown.len());
    }
}
