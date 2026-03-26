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

    #[test]
    fn test_tool_output_item_count() {
        let output = ToolOutput::Issues(vec![]);
        assert_eq!(output.item_count(), 0);

        let output = ToolOutput::Text("hello".into());
        assert_eq!(output.item_count(), 1);
    }

    #[test]
    fn test_tool_output_type_name() {
        let output = ToolOutput::MergeRequests(vec![]);
        assert_eq!(output.type_name(), "merge_requests");

        let output = ToolOutput::Text("ok".into());
        assert_eq!(output.type_name(), "text");
    }
}
