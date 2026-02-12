//! Pipeline plugins for output transformation and optimization.
//!
//! This crate provides plugins to transform tool outputs before returning them to the LLM:
//!
//! - **Truncation**: Limit output size with pagination hints for the agent
//! - **Markdown**: Convert JSON to Markdown for token savings (~50-70% reduction)
//!
//! # Example
//!
//! ```ignore
//! use devboy_pipeline::{Pipeline, TruncationPlugin, MarkdownPlugin};
//! use devboy_core::Issue;
//!
//! let pipeline = Pipeline::new()
//!     .add(TruncationPlugin::new(10, 1000))  // max 10 items, 1000 chars
//!     .add(MarkdownPlugin::new());
//!
//! let output = pipeline.transform_issues(issues)?;
//! ```

pub mod markdown;
pub mod truncation;

pub use markdown::MarkdownPlugin;
pub use truncation::TruncationPlugin;

use devboy_core::{Comment, Discussion, FileDiff, Issue, MergeRequest, Result};

/// Output from a pipeline transformation.
///
/// Contains the transformed data and metadata about truncation/pagination.
#[derive(Debug, Clone)]
pub struct TransformOutput {
    /// The transformed output (Markdown or JSON string)
    pub content: String,
    /// Whether the output was truncated
    pub truncated: bool,
    /// Total count before truncation (if known)
    pub total_count: Option<usize>,
    /// Number of items actually included
    pub included_count: usize,
    /// Hint for the agent about hidden content
    pub agent_hint: Option<String>,
}

impl TransformOutput {
    /// Create a new output with content.
    pub fn new(content: String) -> Self {
        Self {
            content,
            truncated: false,
            total_count: None,
            included_count: 0,
            agent_hint: None,
        }
    }

    /// Mark output as truncated with a hint.
    pub fn with_truncation(mut self, total: usize, included: usize, hint: String) -> Self {
        self.truncated = true;
        self.total_count = Some(total);
        self.included_count = included;
        self.agent_hint = Some(hint);
        self
    }

    /// Get the final output including any agent hints.
    pub fn to_string_with_hints(&self) -> String {
        if let Some(hint) = &self.agent_hint {
            format!("{}\n\n{}", self.content, hint)
        } else {
            self.content.clone()
        }
    }
}

/// Configuration for pipeline transformations.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Maximum number of items to include in output
    pub max_items: usize,
    /// Maximum characters for the entire output
    pub max_chars: usize,
    /// Maximum characters per item (e.g., diff content)
    pub max_chars_per_item: usize,
    /// Output format
    pub format: OutputFormat,
    /// Whether to include agent hints about truncation
    pub include_hints: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            max_items: 20,
            max_chars: 4000,
            max_chars_per_item: 500,
            format: OutputFormat::Markdown,
            include_hints: true,
        }
    }
}

/// Output format for transformations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// JSON format (verbose, ~2000 tokens for typical output)
    Json,
    /// Markdown format (compact, ~100-500 tokens)
    Markdown,
    /// Compact text format (minimal, ~50-200 tokens)
    Compact,
}

/// Pipeline for chaining output transformations.
pub struct Pipeline {
    config: PipelineConfig,
}

impl Pipeline {
    /// Create a new pipeline with default configuration.
    pub fn new() -> Self {
        Self {
            config: PipelineConfig::default(),
        }
    }

    /// Create a pipeline with custom configuration.
    pub fn with_config(config: PipelineConfig) -> Self {
        Self { config }
    }

    /// Transform a list of issues.
    pub fn transform_issues(&self, issues: Vec<Issue>) -> Result<TransformOutput> {
        let total = issues.len();
        let truncated_issues = self.truncate_items(issues);
        let included = truncated_issues.len();

        let content = match self.config.format {
            OutputFormat::Json => serde_json::to_string_pretty(&truncated_issues)?,
            OutputFormat::Markdown => markdown::issues_to_markdown(&truncated_issues),
            OutputFormat::Compact => markdown::issues_to_compact(&truncated_issues),
        };

        let mut output = TransformOutput::new(content);
        output.included_count = included;

        if included < total && self.config.include_hints {
            let hint = self.create_pagination_hint("issues", total, included, None);
            output = output.with_truncation(total, included, hint);
        }

        Ok(self.apply_char_limit(output))
    }

    /// Transform a list of merge requests.
    pub fn transform_merge_requests(&self, mrs: Vec<MergeRequest>) -> Result<TransformOutput> {
        let total = mrs.len();
        let truncated_mrs = self.truncate_items(mrs);
        let included = truncated_mrs.len();

        let content = match self.config.format {
            OutputFormat::Json => serde_json::to_string_pretty(&truncated_mrs)?,
            OutputFormat::Markdown => markdown::merge_requests_to_markdown(&truncated_mrs),
            OutputFormat::Compact => markdown::merge_requests_to_compact(&truncated_mrs),
        };

        let mut output = TransformOutput::new(content);
        output.included_count = included;

        if included < total && self.config.include_hints {
            let hint = self.create_pagination_hint("merge_requests", total, included, None);
            output = output.with_truncation(total, included, hint);
        }

        Ok(self.apply_char_limit(output))
    }

    /// Transform a list of file diffs.
    pub fn transform_diffs(&self, diffs: Vec<FileDiff>) -> Result<TransformOutput> {
        let total = diffs.len();

        // Truncate diff content first
        let truncated_diffs: Vec<FileDiff> = diffs
            .into_iter()
            .take(self.config.max_items)
            .map(|mut d| {
                d.diff = truncation::truncate_string(&d.diff, self.config.max_chars_per_item);
                d
            })
            .collect();

        let included = truncated_diffs.len();

        let content = match self.config.format {
            OutputFormat::Json => serde_json::to_string_pretty(&truncated_diffs)?,
            OutputFormat::Markdown => markdown::diffs_to_markdown(&truncated_diffs),
            OutputFormat::Compact => markdown::diffs_to_compact(&truncated_diffs),
        };

        let mut output = TransformOutput::new(content);
        output.included_count = included;

        if included < total && self.config.include_hints {
            let hint = self.create_pagination_hint("diffs", total, included, Some("get_diffs"));
            output = output.with_truncation(total, included, hint);
        }

        Ok(self.apply_char_limit(output))
    }

    /// Transform a list of comments.
    pub fn transform_comments(&self, comments: Vec<Comment>) -> Result<TransformOutput> {
        let total = comments.len();
        let truncated_comments = self.truncate_items(comments);
        let included = truncated_comments.len();

        let content = match self.config.format {
            OutputFormat::Json => serde_json::to_string_pretty(&truncated_comments)?,
            OutputFormat::Markdown => markdown::comments_to_markdown(&truncated_comments),
            OutputFormat::Compact => markdown::comments_to_compact(&truncated_comments),
        };

        let mut output = TransformOutput::new(content);
        output.included_count = included;

        if included < total && self.config.include_hints {
            let hint = self.create_pagination_hint("comments", total, included, None);
            output = output.with_truncation(total, included, hint);
        }

        Ok(self.apply_char_limit(output))
    }

    /// Transform a list of discussions.
    pub fn transform_discussions(&self, discussions: Vec<Discussion>) -> Result<TransformOutput> {
        let total = discussions.len();
        let truncated_discussions = self.truncate_items(discussions);
        let included = truncated_discussions.len();

        let content = match self.config.format {
            OutputFormat::Json => serde_json::to_string_pretty(&truncated_discussions)?,
            OutputFormat::Markdown => markdown::discussions_to_markdown(&truncated_discussions),
            OutputFormat::Compact => markdown::discussions_to_compact(&truncated_discussions),
        };

        let mut output = TransformOutput::new(content);
        output.included_count = included;

        if included < total && self.config.include_hints {
            let hint = self.create_pagination_hint("discussions", total, included, None);
            output = output.with_truncation(total, included, hint);
        }

        Ok(self.apply_char_limit(output))
    }

    /// Truncate a vector to max_items.
    fn truncate_items<T>(&self, items: Vec<T>) -> Vec<T> {
        items.into_iter().take(self.config.max_items).collect()
    }

    /// Apply character limit to output.
    fn apply_char_limit(&self, mut output: TransformOutput) -> TransformOutput {
        if output.content.len() > self.config.max_chars {
            output.content = truncation::truncate_string(&output.content, self.config.max_chars);
            if !output.truncated {
                output.truncated = true;
                output.agent_hint = Some(format!(
                    "⚠️ Output truncated to {} chars. Use pagination or filters to get more specific results.",
                    self.config.max_chars
                ));
            }
        }
        output
    }

    /// Create a pagination hint for the agent.
    fn create_pagination_hint(
        &self,
        item_type: &str,
        total: usize,
        included: usize,
        tool_name: Option<&str>,
    ) -> String {
        let remaining = total - included;
        let next_offset = included;

        let tool_hint = tool_name
            .map(|t| format!(" Use `{}` with offset={}", t, next_offset))
            .unwrap_or_default();

        format!(
            "📊 Showing {}/{} {}. {} more available.{} You can use `offset` and `limit` parameters for pagination.",
            included, total, item_type, remaining, tool_hint
        )
    }
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::User;

    fn sample_issues() -> Vec<Issue> {
        (1..=25)
            .map(|i| Issue {
                key: format!("gh#{}", i),
                title: format!("Issue {}", i),
                description: Some(format!("Description for issue {}", i)),
                state: "open".to_string(),
                source: "github".to_string(),
                priority: None,
                labels: vec!["bug".to_string()],
                author: Some(User {
                    id: "1".to_string(),
                    username: "test".to_string(),
                    name: None,
                    email: None,
                    avatar_url: None,
                }),
                assignees: vec![],
                url: Some(format!("https://github.com/test/repo/issues/{}", i)),
                created_at: Some("2024-01-01T00:00:00Z".to_string()),
                updated_at: Some("2024-01-02T00:00:00Z".to_string()),
            })
            .collect()
    }

    #[test]
    fn test_pipeline_truncates_items() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            max_items: 5,
            max_chars: 10000,
            ..Default::default()
        });

        let issues = sample_issues();
        let output = pipeline.transform_issues(issues).unwrap();

        assert!(output.truncated);
        assert_eq!(output.total_count, Some(25));
        assert_eq!(output.included_count, 5);
        assert!(output.agent_hint.is_some());
    }

    #[test]
    fn test_pipeline_no_truncation_when_under_limit() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            max_items: 50,
            max_chars: 100000,
            ..Default::default()
        });

        let issues: Vec<Issue> = sample_issues().into_iter().take(5).collect();
        let output = pipeline.transform_issues(issues).unwrap();

        assert!(!output.truncated);
        assert!(output.agent_hint.is_none());
    }

    #[test]
    fn test_markdown_format() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Markdown,
            max_items: 3,
            max_chars: 10000,
            ..Default::default()
        });

        let issues: Vec<Issue> = sample_issues().into_iter().take(3).collect();
        let output = pipeline.transform_issues(issues).unwrap();

        assert!(output.content.contains("## gh#1"));
        assert!(output.content.contains("**State:**"));
    }

    #[test]
    fn test_compact_format() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Compact,
            max_items: 3,
            max_chars: 10000,
            ..Default::default()
        });

        let issues: Vec<Issue> = sample_issues().into_iter().take(3).collect();
        let output = pipeline.transform_issues(issues).unwrap();

        // Compact format should be shorter than markdown
        assert!(output.content.contains("gh#1"));
        assert!(!output.content.contains("##")); // No markdown headers
    }

    #[test]
    fn test_json_format() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Json,
            max_items: 2,
            max_chars: 10000,
            ..Default::default()
        });

        let issues: Vec<Issue> = sample_issues().into_iter().take(2).collect();
        let output = pipeline.transform_issues(issues).unwrap();

        // Should be valid JSON
        let parsed: Vec<Issue> = serde_json::from_str(&output.content).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn test_char_limit_applied() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            max_items: 100,
            max_chars: 100, // Very small limit
            ..Default::default()
        });

        let issues = sample_issues();
        let output = pipeline.transform_issues(issues).unwrap();

        assert!(output.content.len() <= 100);
        assert!(output.truncated);
    }

    fn sample_merge_requests() -> Vec<MergeRequest> {
        (1..=5)
            .map(|i| MergeRequest {
                key: format!("mr#{}", i),
                title: format!("MR {}", i),
                description: Some(format!("MR description {}", i)),
                state: "opened".to_string(),
                source: "gitlab".to_string(),
                source_branch: format!("feature-{}", i),
                target_branch: "main".to_string(),
                author: None,
                assignees: vec![],
                reviewers: vec![],
                labels: vec![],
                url: Some(format!(
                    "https://gitlab.com/test/repo/-/merge_requests/{}",
                    i
                )),
                created_at: Some("2024-01-01T00:00:00Z".to_string()),
                updated_at: Some("2024-01-02T00:00:00Z".to_string()),
                draft: false,
            })
            .collect()
    }

    fn sample_diffs() -> Vec<FileDiff> {
        (1..=5)
            .map(|i| FileDiff {
                file_path: format!("src/file_{}.rs", i),
                old_path: None,
                new_file: i == 1,
                deleted_file: false,
                renamed_file: false,
                diff: format!("+added line {}\n-removed line {}", i, i),
                additions: Some(1),
                deletions: Some(1),
            })
            .collect()
    }

    fn sample_comments() -> Vec<Comment> {
        (1..=5)
            .map(|i| Comment {
                id: format!("{}", i),
                body: format!("Comment body {}", i),
                author: None,
                created_at: Some("2024-01-01T00:00:00Z".to_string()),
                updated_at: None,
                position: None,
            })
            .collect()
    }

    fn sample_discussions() -> Vec<Discussion> {
        (1..=5)
            .map(|i| Discussion {
                id: format!("{}", i),
                resolved: i % 2 == 0,
                resolved_by: None,
                comments: vec![Comment {
                    id: format!("c{}", i),
                    body: format!("Discussion comment {}", i),
                    author: None,
                    created_at: None,
                    updated_at: None,
                    position: None,
                }],
                position: None,
            })
            .collect()
    }

    #[test]
    fn test_transform_merge_requests_markdown() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Markdown,
            max_items: 3,
            max_chars: 10000,
            ..Default::default()
        });

        let mrs = sample_merge_requests();
        let output = pipeline.transform_merge_requests(mrs).unwrap();

        assert!(output.content.contains("mr#1"));
        assert!(output.content.contains("MR 1"));
        assert!(output.truncated);
        assert_eq!(output.included_count, 3);
    }

    #[test]
    fn test_transform_merge_requests_json() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Json,
            max_items: 2,
            max_chars: 10000,
            ..Default::default()
        });

        let mrs: Vec<MergeRequest> = sample_merge_requests().into_iter().take(2).collect();
        let output = pipeline.transform_merge_requests(mrs).unwrap();

        let parsed: Vec<MergeRequest> = serde_json::from_str(&output.content).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn test_transform_merge_requests_compact() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Compact,
            max_items: 2,
            max_chars: 10000,
            ..Default::default()
        });

        let mrs: Vec<MergeRequest> = sample_merge_requests().into_iter().take(2).collect();
        let output = pipeline.transform_merge_requests(mrs).unwrap();

        assert!(output.content.contains("mr#1"));
        assert!(!output.truncated);
    }

    #[test]
    fn test_transform_diffs_markdown() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Markdown,
            max_items: 3,
            max_chars: 10000,
            ..Default::default()
        });

        let diffs = sample_diffs();
        let output = pipeline.transform_diffs(diffs).unwrap();

        assert!(output.content.contains("src/file_1.rs"));
        assert!(output.truncated);
        assert_eq!(output.included_count, 3);
        assert!(output.agent_hint.unwrap().contains("get_diffs"));
    }

    #[test]
    fn test_transform_diffs_json() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Json,
            max_items: 2,
            max_chars: 10000,
            ..Default::default()
        });

        let diffs: Vec<FileDiff> = sample_diffs().into_iter().take(2).collect();
        let output = pipeline.transform_diffs(diffs).unwrap();

        let parsed: Vec<FileDiff> = serde_json::from_str(&output.content).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn test_transform_diffs_compact() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Compact,
            max_items: 10,
            max_chars: 10000,
            ..Default::default()
        });

        let diffs: Vec<FileDiff> = sample_diffs().into_iter().take(2).collect();
        let output = pipeline.transform_diffs(diffs).unwrap();

        assert!(output.content.contains("src/file_1.rs"));
        assert!(!output.truncated);
    }

    #[test]
    fn test_transform_comments_markdown() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Markdown,
            max_items: 3,
            max_chars: 10000,
            ..Default::default()
        });

        let comments = sample_comments();
        let output = pipeline.transform_comments(comments).unwrap();

        assert!(output.content.contains("Comment body 1"));
        assert!(output.truncated);
        assert_eq!(output.included_count, 3);
    }

    #[test]
    fn test_transform_comments_json() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Json,
            max_items: 2,
            max_chars: 10000,
            ..Default::default()
        });

        let comments: Vec<Comment> = sample_comments().into_iter().take(2).collect();
        let output = pipeline.transform_comments(comments).unwrap();

        let parsed: Vec<Comment> = serde_json::from_str(&output.content).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn test_transform_comments_compact() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Compact,
            max_items: 10,
            max_chars: 10000,
            ..Default::default()
        });

        let comments: Vec<Comment> = sample_comments().into_iter().take(2).collect();
        let output = pipeline.transform_comments(comments).unwrap();

        assert!(output.content.contains("Comment body 1"));
    }

    #[test]
    fn test_transform_discussions_markdown() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Markdown,
            max_items: 3,
            max_chars: 10000,
            ..Default::default()
        });

        let discussions = sample_discussions();
        let output = pipeline.transform_discussions(discussions).unwrap();

        assert!(output.content.contains("Discussion comment 1"));
        assert!(output.truncated);
        assert_eq!(output.included_count, 3);
    }

    #[test]
    fn test_transform_discussions_json() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Json,
            max_items: 2,
            max_chars: 10000,
            ..Default::default()
        });

        let discussions: Vec<Discussion> = sample_discussions().into_iter().take(2).collect();
        let output = pipeline.transform_discussions(discussions).unwrap();

        let parsed: Vec<Discussion> = serde_json::from_str(&output.content).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn test_transform_discussions_compact() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Compact,
            max_items: 10,
            max_chars: 10000,
            ..Default::default()
        });

        let discussions: Vec<Discussion> = sample_discussions().into_iter().take(2).collect();
        let output = pipeline.transform_discussions(discussions).unwrap();

        // Compact format shows counts, not full content
        assert!(output.content.contains("#1"));
        assert!(output.content.contains("replies"));
    }

    #[test]
    fn test_transform_output_to_string_with_hints() {
        let output = TransformOutput::new("content".to_string());
        assert_eq!(output.to_string_with_hints(), "content");

        let output = TransformOutput::new("content".to_string()).with_truncation(
            10,
            5,
            "hint text".to_string(),
        );
        assert!(output.to_string_with_hints().contains("content"));
        assert!(output.to_string_with_hints().contains("hint text"));
    }

    #[test]
    fn test_pipeline_default() {
        let pipeline = Pipeline::default();
        let issues: Vec<Issue> = sample_issues().into_iter().take(1).collect();
        let output = pipeline.transform_issues(issues).unwrap();
        assert!(!output.content.is_empty());
    }

    #[test]
    fn test_pipeline_hints_disabled() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            max_items: 2,
            max_chars: 100000,
            include_hints: false,
            ..Default::default()
        });

        let issues = sample_issues();
        let output = pipeline.transform_issues(issues).unwrap();

        // Items truncated but no hint since hints are disabled
        assert_eq!(output.included_count, 2);
        assert!(!output.truncated);
        assert!(output.agent_hint.is_none());
    }
}
