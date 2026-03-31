//! Format pipeline for tool output transformation.
//!
//! Formats tool responses into an optimal format for LLM:
//!
//! - **TOON** (default): Token-Oriented Object Notation -- saves 39-90% of tokens
//! - **JSON**: for programmatic processing
//! - **Budget trimming**: smart strategy-based trimming when output exceeds budget
//!
//! # Example
//!
//! ```ignore
//! use devboy_format_pipeline::{Pipeline, PipelineConfig, OutputFormat};
//! use devboy_core::Issue;
//!
//! let pipeline = Pipeline::with_config(PipelineConfig {
//!     format: OutputFormat::Toon,
//!     max_chars: 100_000,
//!     ..Default::default()
//! });
//!
//! let output = pipeline.transform_issues(issues)?;
//! println!("{}", output.to_string_with_hints());
//! ```

pub mod budget;
pub mod page_index;
pub mod pagination;
pub mod strategy;
pub mod token_counter;
pub mod toon;
pub mod tree;
pub mod trim;
pub mod truncation;

pub use truncation::TruncationPlugin;

use devboy_core::{Comment, Discussion, FileDiff, Issue, MergeRequest, Result};

use budget::BudgetConfig;
use strategy::StrategyResolver;

/// Convert character budget to token estimate (chars / 3.5).
fn estimate_tokens_from_chars(chars: usize) -> usize {
    (chars as f64 / 3.5).ceil() as usize
}

/// Output from a pipeline transformation.
///
/// Contains the transformed data and metadata about truncation/pagination.
#[derive(Debug, Clone)]
pub struct TransformOutput {
    /// The transformed output (TOON or JSON string)
    pub content: String,
    /// Whether the output was truncated
    pub truncated: bool,
    /// Total count before truncation (if known)
    pub total_count: Option<usize>,
    /// Number of items actually included
    pub included_count: usize,
    /// Hint for the agent about hidden content
    pub agent_hint: Option<String>,
    /// Cursor for fetching the next page (if overflow exists)
    pub page_cursor: Option<String>,
    /// Page index for large results (when budget trimming is applied)
    pub page_index: Option<page_index::PageIndex>,
    /// Provider-level pagination metadata
    pub provider_pagination: Option<devboy_core::Pagination>,
    /// Provider-level sort metadata
    pub provider_sort: Option<devboy_core::SortInfo>,
    /// Size of raw input data before formatting (UTF-8 bytes)
    pub raw_chars: usize,
    /// Size of formatted output (UTF-8 bytes) — updated after apply_char_limit
    pub output_chars: usize,
    /// Size of output BEFORE budget trimming (UTF-8 bytes).
    /// Set by apply_char_limit when truncation occurs.
    pub pre_trim_chars: usize,
}

impl TransformOutput {
    /// Create a new output with content.
    pub fn new(content: String) -> Self {
        let output_chars = content.len();
        Self {
            content,
            truncated: false,
            total_count: None,
            included_count: 0,
            agent_hint: None,
            page_cursor: None,
            page_index: None,
            provider_pagination: None,
            provider_sort: None,
            raw_chars: 0,
            output_chars,
            pre_trim_chars: 0,
        }
    }

    /// Set raw input size (before formatting).
    pub fn with_raw_chars(mut self, raw_chars: usize) -> Self {
        self.raw_chars = raw_chars;
        self
    }

    /// Mark output as truncated with a hint.
    pub fn with_truncation(mut self, total: usize, included: usize, hint: String) -> Self {
        self.truncated = true;
        self.total_count = Some(total);
        self.included_count = included;
        self.agent_hint = Some(hint);
        self
    }

    /// Get the final output including page index and agent hints.
    pub fn to_string_with_hints(&self) -> String {
        let mut parts = Vec::new();

        // Page index header (when budget trimming produced pages)
        if let Some(index) = &self.page_index {
            parts.push(index.to_toon());
        }

        // Main content
        parts.push(self.content.clone());

        // Agent hint footer
        if let Some(hint) = &self.agent_hint {
            parts.push(hint.clone());
        }

        parts.join("\n\n")
    }
}

/// Configuration for pipeline transformations.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Maximum characters for the entire output (0 = no limit).
    /// Used as budget ceiling — converted to tokens via `max_chars / 3.5`.
    pub max_chars: usize,
    /// Maximum characters per item (e.g., diff content)
    pub max_chars_per_item: usize,
    /// Maximum description/body length before truncation (only outliers get truncated)
    pub max_description_len: usize,
    /// Output format
    pub format: OutputFormat,
    /// Whether to include agent hints about truncation
    pub include_hints: bool,
    /// Page cursor from a previous request (for pagination)
    pub page_cursor: Option<String>,
    /// Tool name for strategy resolution (e.g., "get_issues", "get_merge_request_diffs")
    pub tool_name: Option<String>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            max_chars: 100_000,
            max_chars_per_item: 10_000,
            max_description_len: 10_000,
            format: OutputFormat::Toon,
            include_hints: true,
            page_cursor: None,
            tool_name: None,
        }
    }
}

/// Output format for transformations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// TOON format (default) -- token-optimized, saves 39-90% vs JSON
    Toon,
    /// JSON format -- for programmatic processing
    Json,
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

    /// Transform a list of issues using budget pipeline.
    pub fn transform_issues(&self, issues: Vec<Issue>) -> Result<TransformOutput> {
        let total = issues.len();
        let raw_json = serde_json::to_string(&issues)?;
        let raw_chars = raw_json.len();

        let content = match self.config.format {
            OutputFormat::Json => serde_json::to_string_pretty(&issues)?,
            OutputFormat::Toon => toon::encode_issues(&issues, toon::TrimLevel::Full)?,
        };

        // Fast path: fits within budget
        if self.config.max_chars == 0 || content.len() <= self.config.max_chars {
            let mut output = TransformOutput::new(content).with_raw_chars(raw_chars);
            output.included_count = total;
            return Ok(output);
        }

        // Budget pipeline: smart trimming with strategy
        let budget_config = self.budget_config();
        let strategy_kind = self.resolve_strategy("get_issues");
        let result = budget::process_issues(&issues, strategy_kind, &budget_config)?;

        let json_fallback = self.json_fallback(&content);
        let index = page_index::build_issues_index(&issues, result.included_items);
        self.build_budget_output(
            result,
            raw_chars,
            total,
            "issues",
            Some(index),
            json_fallback,
        )
    }

    /// Transform a list of merge requests using budget pipeline.
    pub fn transform_merge_requests(&self, mrs: Vec<MergeRequest>) -> Result<TransformOutput> {
        let total = mrs.len();
        let raw_json = serde_json::to_string(&mrs)?;
        let raw_chars = raw_json.len();

        let content = match self.config.format {
            OutputFormat::Json => serde_json::to_string_pretty(&mrs)?,
            OutputFormat::Toon => toon::encode_merge_requests(&mrs, toon::TrimLevel::Full)?,
        };

        if self.config.max_chars == 0 || content.len() <= self.config.max_chars {
            let mut output = TransformOutput::new(content).with_raw_chars(raw_chars);
            output.included_count = total;
            return Ok(output);
        }

        let json_fallback = self.json_fallback(&content);
        let budget_config = self.budget_config();
        let strategy_kind = self.resolve_strategy("get_merge_requests");
        let result = budget::process_merge_requests(&mrs, strategy_kind, &budget_config)?;

        let index = page_index::build_merge_requests_index(&mrs, result.included_items);
        self.build_budget_output(
            result,
            raw_chars,
            total,
            "merge_requests",
            Some(index),
            json_fallback,
        )
    }

    /// Transform a list of file diffs using budget pipeline.
    ///
    /// Individual diff content is truncated per `max_chars_per_item` before
    /// budget trimming to protect against giant lock/generated files.
    pub fn transform_diffs(&self, diffs: Vec<FileDiff>) -> Result<TransformOutput> {
        let total = diffs.len();

        // Per-item truncation for individual diff content (protection against giant files)
        let diffs: Vec<FileDiff> = diffs
            .into_iter()
            .map(|mut d| {
                d.diff = truncation::truncate_string(&d.diff, self.config.max_chars_per_item);
                d
            })
            .collect();

        let raw_json = serde_json::to_string(&diffs)?;
        let raw_chars = raw_json.len();

        let content = match self.config.format {
            OutputFormat::Json => serde_json::to_string_pretty(&diffs)?,
            OutputFormat::Toon => toon::encode_diffs(&diffs)?,
        };

        if self.config.max_chars == 0 || content.len() <= self.config.max_chars {
            let mut output = TransformOutput::new(content).with_raw_chars(raw_chars);
            output.included_count = total;
            return Ok(output);
        }

        let json_fallback = self.json_fallback(&content);
        let budget_config = self.budget_config();
        let strategy_kind = self.resolve_strategy("get_merge_request_diffs");
        let result = budget::process_diffs(&diffs, strategy_kind, &budget_config)?;

        let index = page_index::build_diffs_index(&diffs, result.included_items);
        self.build_budget_output(
            result,
            raw_chars,
            total,
            "diffs",
            Some(index),
            json_fallback,
        )
    }

    /// Transform a list of comments using budget pipeline.
    pub fn transform_comments(&self, comments: Vec<Comment>) -> Result<TransformOutput> {
        let total = comments.len();
        let raw_json = serde_json::to_string(&comments)?;
        let raw_chars = raw_json.len();

        let content = match self.config.format {
            OutputFormat::Json => serde_json::to_string_pretty(&comments)?,
            OutputFormat::Toon => toon::encode_comments(&comments)?,
        };

        if self.config.max_chars == 0 || content.len() <= self.config.max_chars {
            let mut output = TransformOutput::new(content).with_raw_chars(raw_chars);
            output.included_count = total;
            return Ok(output);
        }

        let json_fallback = self.json_fallback(&content);
        let budget_config = self.budget_config();
        let strategy_kind = self.resolve_strategy("get_issue_comments");
        let result = budget::process_comments(&comments, strategy_kind, &budget_config)?;

        let index = page_index::build_comments_index(&comments, result.included_items);
        self.build_budget_output(
            result,
            raw_chars,
            total,
            "comments",
            Some(index),
            json_fallback,
        )
    }

    /// Transform a list of discussions using budget pipeline.
    pub fn transform_discussions(&self, discussions: Vec<Discussion>) -> Result<TransformOutput> {
        let total = discussions.len();
        let raw_json = serde_json::to_string(&discussions)?;
        let raw_chars = raw_json.len();

        let content = match self.config.format {
            OutputFormat::Json => serde_json::to_string_pretty(&discussions)?,
            OutputFormat::Toon => toon::encode_discussions(&discussions)?,
        };

        if self.config.max_chars == 0 || content.len() <= self.config.max_chars {
            let mut output = TransformOutput::new(content).with_raw_chars(raw_chars);
            output.included_count = total;
            return Ok(output);
        }

        let json_fallback = self.json_fallback(&content);
        let budget_config = self.budget_config();
        let strategy_kind = self.resolve_strategy("get_merge_request_discussions");
        let result = budget::process_discussions(&discussions, strategy_kind, &budget_config)?;

        let index = page_index::build_discussions_index(&discussions, result.included_items);
        self.build_budget_output(
            result,
            raw_chars,
            total,
            "discussions",
            Some(index),
            json_fallback,
        )
    }

    /// When format is JSON, return the content for truncation fallback.
    /// Budget pipeline always produces TOON, so for JSON we truncate the original JSON.
    fn json_fallback(&self, content: &str) -> Option<String> {
        if matches!(self.config.format, OutputFormat::Json) {
            Some(content.to_string())
        } else {
            None
        }
    }

    /// Convert max_chars to budget pipeline config.
    fn budget_config(&self) -> BudgetConfig {
        BudgetConfig {
            budget_tokens: estimate_tokens_from_chars(self.config.max_chars),
            ..Default::default()
        }
    }

    /// Resolve trimming strategy for tool name.
    fn resolve_strategy(&self, default_tool: &str) -> strategy::TrimStrategyKind {
        let resolver = StrategyResolver::new();
        let tool = self.config.tool_name.as_deref().unwrap_or(default_tool);
        resolver.resolve(tool)
    }

    /// Build TransformOutput from BudgetResult with optional page index.
    ///
    /// Note: budget pipeline always produces TOON content. When format is JSON,
    /// we fall back to simple character truncation of the JSON output instead.
    fn build_budget_output(
        &self,
        result: budget::BudgetResult,
        raw_chars: usize,
        total: usize,
        item_type: &str,
        index: Option<page_index::PageIndex>,
        json_fallback: Option<String>,
    ) -> Result<TransformOutput> {
        // Budget pipeline produces TOON. For JSON format, use truncated JSON instead.
        let content = if matches!(self.config.format, OutputFormat::Json) {
            if let Some(json) = json_fallback {
                truncation::truncate_string(&json, self.config.max_chars)
            } else {
                result.content
            }
        } else {
            result.content
        };

        let mut output = TransformOutput::new(content).with_raw_chars(raw_chars);
        output.included_count = result.included_items;

        // Always set truncation metadata when trimmed, regardless of include_hints
        if result.trimmed {
            output.truncated = true;
            output.total_count = Some(total);

            if self.config.include_hints {
                let remaining = total.saturating_sub(result.included_items);

                // Generate page index for multi-page results
                if let Some(idx) = index {
                    if idx.total_pages > 1 {
                        let hint = format!(
                            "Showing {}/{} {} selected by priority ({} pages available). Use `offset` and `limit` for sequential access.",
                            result.included_items, total, item_type, idx.total_pages
                        );
                        output.page_index = Some(idx);
                        output.agent_hint = Some(hint);
                    } else {
                        output.agent_hint = Some(format!(
                            "Showing {}/{} {}. {} items trimmed by budget.",
                            result.included_items, total, item_type, remaining
                        ));
                    }
                } else {
                    output.agent_hint = Some(format!(
                        "Showing {}/{} {}. {} items trimmed by budget. Use `offset` and `limit` parameters for pagination.",
                        result.included_items, total, item_type, remaining
                    ));
                }
            }
        }

        Ok(output)
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
                parent: None,
                subtasks: vec![],
            })
            .collect()
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

    // --- Pipeline truncation (budget-based) ---

    #[test]
    fn test_pipeline_truncates_items() {
        // Use a small max_chars to force budget trimming
        let pipeline = Pipeline::with_config(PipelineConfig {
            max_chars: 200,
            ..Default::default()
        });

        let issues = sample_issues();
        let output = pipeline.transform_issues(issues).unwrap();

        assert!(output.truncated);
        assert_eq!(output.total_count, Some(25));
        assert!(output.included_count < 25);
        assert!(output.agent_hint.is_some());
    }

    #[test]
    fn test_pipeline_no_truncation_when_under_limit() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            max_chars: 100_000,
            ..Default::default()
        });

        let issues: Vec<Issue> = sample_issues().into_iter().take(5).collect();
        let output = pipeline.transform_issues(issues).unwrap();

        assert!(!output.truncated);
        assert!(output.agent_hint.is_none());
    }

    // --- Toon format ---

    #[test]
    fn test_toon_format_issues() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Toon,
            max_chars: 100_000,
            ..Default::default()
        });

        let issues: Vec<Issue> = sample_issues().into_iter().take(3).collect();
        let output = pipeline.transform_issues(issues).unwrap();

        assert!(output.content.contains("gh#1"));
        assert!(output.content.contains("Issue 1"));
    }

    #[test]
    fn test_toon_format_merge_requests() {
        // Use max_chars large enough to include some but not all MRs
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Toon,
            max_chars: 500,
            ..Default::default()
        });

        let mrs = sample_merge_requests();
        let output = pipeline.transform_merge_requests(mrs).unwrap();

        assert!(output.content.contains("mr#1"));
        assert!(output.content.contains("MR 1"));
        assert!(output.truncated);
        assert!(output.included_count < 5);
    }

    #[test]
    fn test_toon_format_diffs() {
        // Use max_chars small enough to force budget trimming of 5 diffs
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Toon,
            max_chars: 200,
            ..Default::default()
        });

        let diffs = sample_diffs();
        let output = pipeline.transform_diffs(diffs).unwrap();

        assert!(output.content.contains("src/file_1.rs"));
        assert!(output.truncated);
        assert!(output.included_count < 5);
    }

    #[test]
    fn test_toon_format_comments() {
        // Use max_chars small enough to force budget trimming of 5 comments
        // but large enough to include at least one comment with body text
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Toon,
            max_chars: 300,
            ..Default::default()
        });

        let comments = sample_comments();
        let output = pipeline.transform_comments(comments).unwrap();

        // Budget trimming may drop early items; check that some comment body is present
        assert!(output.content.contains("Comment body"));
        assert!(output.truncated);
        assert!(output.included_count < 5);
    }

    #[test]
    fn test_toon_format_discussions() {
        // Use max_chars large enough to include some but not all discussions
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Toon,
            max_chars: 500,
            ..Default::default()
        });

        let discussions = sample_discussions();
        let output = pipeline.transform_discussions(discussions).unwrap();

        assert!(output.content.contains("Discussion comment 1"));
        assert!(output.truncated);
        assert!(output.included_count < 5);
    }

    // --- JSON format ---

    #[test]
    fn test_json_format_issues() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Json,
            max_chars: 100_000,
            ..Default::default()
        });

        let issues: Vec<Issue> = sample_issues().into_iter().take(2).collect();
        let output = pipeline.transform_issues(issues).unwrap();

        let parsed: Vec<Issue> = serde_json::from_str(&output.content).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn test_json_format_merge_requests() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Json,
            max_chars: 100_000,
            ..Default::default()
        });

        let mrs: Vec<MergeRequest> = sample_merge_requests().into_iter().take(2).collect();
        let output = pipeline.transform_merge_requests(mrs).unwrap();

        let parsed: Vec<MergeRequest> = serde_json::from_str(&output.content).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn test_json_format_diffs() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Json,
            max_chars: 100_000,
            ..Default::default()
        });

        let diffs: Vec<FileDiff> = sample_diffs().into_iter().take(2).collect();
        let output = pipeline.transform_diffs(diffs).unwrap();

        let parsed: Vec<FileDiff> = serde_json::from_str(&output.content).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn test_json_format_comments() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Json,
            max_chars: 100_000,
            ..Default::default()
        });

        let comments: Vec<Comment> = sample_comments().into_iter().take(2).collect();
        let output = pipeline.transform_comments(comments).unwrap();

        let parsed: Vec<Comment> = serde_json::from_str(&output.content).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn test_json_format_discussions() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Json,
            max_chars: 100_000,
            ..Default::default()
        });

        let discussions: Vec<Discussion> = sample_discussions().into_iter().take(2).collect();
        let output = pipeline.transform_discussions(discussions).unwrap();

        let parsed: Vec<Discussion> = serde_json::from_str(&output.content).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    // --- TransformOutput ---

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
    fn test_transform_output_with_truncation() {
        let output =
            TransformOutput::new("data".into()).with_truncation(100, 10, "90 more items".into());
        assert!(output.truncated);
        assert_eq!(output.total_count, Some(100));
        assert_eq!(output.included_count, 10);
        assert_eq!(output.agent_hint.as_deref(), Some("90 more items"));
    }

    // --- PipelineConfig ---

    #[test]
    fn test_pipeline_config_default_values() {
        let config = PipelineConfig::default();
        assert_eq!(config.max_chars, 100_000);
        assert_eq!(config.max_chars_per_item, 10_000);
        assert_eq!(config.max_description_len, 10_000);
        assert!(matches!(config.format, OutputFormat::Toon));
        assert!(config.include_hints);
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
        // Use small max_chars to trigger budget trimming, but with hints disabled
        let pipeline = Pipeline::with_config(PipelineConfig {
            max_chars: 200,
            include_hints: false,
            ..Default::default()
        });

        let issues = sample_issues();
        let output = pipeline.transform_issues(issues).unwrap();

        assert!(output.included_count < 25);
        // truncated flag is always set when trimming occurs (for metadata consumers)
        assert!(output.truncated);
        // but agent_hint and page_index are suppressed when include_hints is false
        assert!(output.agent_hint.is_none());
        assert!(output.page_index.is_none());
    }

    // --- Character limit (budget-based) ---

    #[test]
    fn test_char_limit_applied() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            max_chars: 100,
            ..Default::default()
        });

        let issues = sample_issues();
        let output = pipeline.transform_issues(issues).unwrap();

        assert!(output.truncated);
    }

    #[test]
    fn test_char_limit_triggers_trimming() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            max_chars: 50,
            ..Default::default()
        });

        let issues: Vec<Issue> = sample_issues().into_iter().take(3).collect();
        let output = pipeline.transform_issues(issues).unwrap();
        assert!(output.truncated);
    }

    // --- Empty collections ---

    #[test]
    fn test_transform_empty_issues() {
        let pipeline = Pipeline::new();
        let output = pipeline.transform_issues(vec![]).unwrap();
        assert!(!output.truncated);
        assert_eq!(output.included_count, 0);
    }

    #[test]
    fn test_transform_empty_merge_requests() {
        let pipeline = Pipeline::new();
        let output = pipeline.transform_merge_requests(vec![]).unwrap();
        assert!(!output.truncated);
        assert_eq!(output.included_count, 0);
    }

    #[test]
    fn test_transform_empty_diffs() {
        let pipeline = Pipeline::new();
        let output = pipeline.transform_diffs(vec![]).unwrap();
        assert!(!output.truncated);
        assert_eq!(output.included_count, 0);
    }

    #[test]
    fn test_transform_empty_comments() {
        let pipeline = Pipeline::new();
        let output = pipeline.transform_comments(vec![]).unwrap();
        assert!(!output.truncated);
        assert_eq!(output.included_count, 0);
    }

    #[test]
    fn test_transform_empty_discussions() {
        let pipeline = Pipeline::new();
        let output = pipeline.transform_discussions(vec![]).unwrap();
        assert!(!output.truncated);
        assert_eq!(output.included_count, 0);
    }

    // --- Diff truncation per item ---

    #[test]
    fn test_diff_content_truncated_per_item() {
        let pipeline = Pipeline::with_config(PipelineConfig {
            max_chars_per_item: 10,
            max_chars: 100_000,
            ..Default::default()
        });

        let diffs = vec![FileDiff {
            file_path: "big.rs".into(),
            old_path: None,
            new_file: false,
            deleted_file: false,
            renamed_file: false,
            diff: "x".repeat(1000),
            additions: Some(100),
            deletions: Some(0),
        }];

        let output = pipeline.transform_diffs(diffs).unwrap();
        assert!(output.content.len() < 1000);
    }

    // --- TOON smaller than JSON ---

    #[test]
    fn test_toon_smaller_than_json_for_issues() {
        let issues: Vec<Issue> = sample_issues().into_iter().take(10).collect();

        let json_pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Json,
            max_chars: 1_000_000,
            ..Default::default()
        });
        let toon_pipeline = Pipeline::with_config(PipelineConfig {
            format: OutputFormat::Toon,
            max_chars: 1_000_000,
            ..Default::default()
        });

        let json_output = json_pipeline.transform_issues(issues.clone()).unwrap();
        let toon_output = toon_pipeline.transform_issues(issues).unwrap();

        assert!(
            toon_output.content.len() < json_output.content.len(),
            "TOON ({}) should be smaller than JSON ({})",
            toon_output.content.len(),
            json_output.content.len()
        );
    }
}
