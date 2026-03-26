use devboy_core::{Comment, Discussion, FileDiff, Issue, MergeRequest};

/// Typed result of tool execution.
///
/// Each variant carries structured data from the provider.
/// The caller (MCP server, NAPI bridge, HTTP handler) decides
/// how to format the output (pipeline text, JSON, etc.).
#[derive(Debug)]
pub enum ToolOutput {
    /// List of merge requests / pull requests
    MergeRequests(Vec<MergeRequest>),
    /// Single merge request / pull request
    SingleMergeRequest(Box<MergeRequest>),
    /// MR/PR discussions with comments and code positions
    Discussions(Vec<Discussion>),
    /// File diffs from a merge request / pull request
    Diffs(Vec<FileDiff>),
    /// List of issues / tasks
    Issues(Vec<Issue>),
    /// Single issue / task
    SingleIssue(Box<Issue>),
    /// Comments on an issue or merge request
    Comments(Vec<Comment>),
    /// Plain text result (e.g., "Comment created successfully")
    Text(String),
}

impl ToolOutput {
    /// Returns the number of items in collection outputs, or 1 for single items.
    pub fn item_count(&self) -> usize {
        match self {
            Self::MergeRequests(v) => v.len(),
            Self::Discussions(v) => v.len(),
            Self::Diffs(v) => v.len(),
            Self::Issues(v) => v.len(),
            Self::Comments(v) => v.len(),
            Self::SingleMergeRequest(_) | Self::SingleIssue(_) | Self::Text(_) => 1,
        }
    }

    /// Returns a human-readable type name for this output.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::MergeRequests(_) => "merge_requests",
            Self::SingleMergeRequest(_) => "merge_request",
            Self::Discussions(_) => "discussions",
            Self::Diffs(_) => "diffs",
            Self::Issues(_) => "issues",
            Self::SingleIssue(_) => "issue",
            Self::Comments(_) => "comments",
            Self::Text(_) => "text",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::{Comment, Discussion, FileDiff, Issue, MergeRequest};

    fn issue() -> Issue {
        Issue {
            key: "gh#1".into(),
            title: "T".into(),
            description: None,
            state: "open".into(),
            source: "mock".into(),
            priority: None,
            labels: vec![],
            author: None,
            assignees: vec![],
            url: None,
            created_at: None,
            updated_at: None,
        }
    }

    fn mr() -> MergeRequest {
        MergeRequest {
            key: "pr#1".into(),
            title: "T".into(),
            description: None,
            state: "open".into(),
            source: "mock".into(),
            source_branch: "f".into(),
            target_branch: "m".into(),
            author: None,
            assignees: vec![],
            reviewers: vec![],
            labels: vec![],
            draft: false,
            url: None,
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn test_item_count_all_variants() {
        assert_eq!(ToolOutput::Issues(vec![issue(), issue()]).item_count(), 2);
        assert_eq!(ToolOutput::MergeRequests(vec![]).item_count(), 0);
        assert_eq!(ToolOutput::SingleIssue(Box::new(issue())).item_count(), 1);
        assert_eq!(
            ToolOutput::SingleMergeRequest(Box::new(mr())).item_count(),
            1
        );
        assert_eq!(ToolOutput::Discussions(vec![]).item_count(), 0);
        assert_eq!(ToolOutput::Diffs(vec![]).item_count(), 0);
        assert_eq!(ToolOutput::Comments(vec![]).item_count(), 0);
        assert_eq!(ToolOutput::Text("x".into()).item_count(), 1);
    }

    #[test]
    fn test_type_name_all_variants() {
        assert_eq!(ToolOutput::Issues(vec![]).type_name(), "issues");
        assert_eq!(
            ToolOutput::MergeRequests(vec![]).type_name(),
            "merge_requests"
        );
        assert_eq!(
            ToolOutput::SingleIssue(Box::new(issue())).type_name(),
            "issue"
        );
        assert_eq!(
            ToolOutput::SingleMergeRequest(Box::new(mr())).type_name(),
            "merge_request"
        );
        assert_eq!(ToolOutput::Discussions(vec![]).type_name(), "discussions");
        assert_eq!(ToolOutput::Diffs(vec![]).type_name(), "diffs");
        assert_eq!(ToolOutput::Comments(vec![]).type_name(), "comments");
        assert_eq!(ToolOutput::Text("x".into()).type_name(), "text");
    }
}
