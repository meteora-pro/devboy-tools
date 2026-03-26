//! Format `ToolOutput` to text using the pipeline plugin.
//!
//! This module bridges the executor's typed output with the pipeline's
//! text formatting. The caller can specify output format (markdown, compact, json).

use devboy_core::Result;
use devboy_pipeline::{OutputFormat, Pipeline, PipelineConfig};

use crate::output::ToolOutput;

/// Format a `ToolOutput` to text using the pipeline.
///
/// # Arguments
/// * `output` — typed result from executor
/// * `format` — output format string ("markdown", "compact", "json"), defaults to "markdown"
/// * `config` — optional pipeline config override
pub fn format_output(
    output: ToolOutput,
    format: Option<&str>,
    config: Option<PipelineConfig>,
) -> Result<String> {
    let output_format = match format {
        Some("json") => OutputFormat::Json,
        Some("compact") => OutputFormat::Compact,
        _ => OutputFormat::Markdown,
    };

    let pipeline_config = config.unwrap_or_else(|| PipelineConfig {
        format: output_format,
        ..PipelineConfig::default()
    });

    // Override format in config
    let pipeline_config = PipelineConfig {
        format: output_format,
        ..pipeline_config
    };

    let pipeline = Pipeline::with_config(pipeline_config);

    match output {
        ToolOutput::Issues(issues) => {
            let result = pipeline.transform_issues(issues)?;
            Ok(result.to_string_with_hints())
        }
        ToolOutput::SingleIssue(issue) => {
            let result = pipeline.transform_issues(vec![*issue])?;
            Ok(result.to_string_with_hints())
        }
        ToolOutput::MergeRequests(mrs) => {
            let result = pipeline.transform_merge_requests(mrs)?;
            Ok(result.to_string_with_hints())
        }
        ToolOutput::SingleMergeRequest(mr) => {
            let result = pipeline.transform_merge_requests(vec![*mr])?;
            Ok(result.to_string_with_hints())
        }
        ToolOutput::Discussions(discussions) => {
            let result = pipeline.transform_discussions(discussions)?;
            Ok(result.to_string_with_hints())
        }
        ToolOutput::Diffs(diffs) => {
            let result = pipeline.transform_diffs(diffs)?;
            Ok(result.to_string_with_hints())
        }
        ToolOutput::Comments(comments) => {
            let result = pipeline.transform_comments(comments)?;
            Ok(result.to_string_with_hints())
        }
        ToolOutput::Text(text) => Ok(text),
    }
}

/// Convenience: execute a tool and format the output in one call.
///
/// Extracts `format` from args before passing to executor.
pub async fn execute_and_format(
    executor: &crate::executor::Executor,
    tool: &str,
    args: serde_json::Value,
    ctx: &crate::context::AdditionalContext,
    pipeline_config: Option<PipelineConfig>,
) -> Result<String> {
    // Extract format from args before execution
    let format = args
        .get("format")
        .and_then(|v| v.as_str())
        .map(String::from);

    let output = executor.execute(tool, args, ctx).await?;
    format_output(output, format.as_deref(), pipeline_config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::Issue;

    fn sample_issue() -> Issue {
        Issue {
            key: "gh#1".into(),
            title: "Test Issue".into(),
            description: Some("Test description".into()),
            state: "open".into(),
            source: "github".into(),
            priority: None,
            labels: vec!["bug".into()],
            author: None,
            assignees: vec![],
            url: Some("https://github.com/test/repo/issues/1".into()),
            created_at: Some("2024-01-01T00:00:00Z".into()),
            updated_at: Some("2024-01-02T00:00:00Z".into()),
        }
    }

    #[test]
    fn test_format_issues_markdown() {
        let output = ToolOutput::Issues(vec![sample_issue()]);
        let result = format_output(output, Some("markdown"), None).unwrap();
        assert!(result.contains("gh#1"));
        assert!(result.contains("Test Issue"));
    }

    #[test]
    fn test_format_issues_json() {
        let output = ToolOutput::Issues(vec![sample_issue()]);
        let result = format_output(output, Some("json"), None).unwrap();
        assert!(result.contains("gh#1"));
    }

    #[test]
    fn test_format_issues_compact() {
        let output = ToolOutput::Issues(vec![sample_issue()]);
        let result = format_output(output, Some("compact"), None).unwrap();
        assert!(result.contains("gh#1"));
    }

    #[test]
    fn test_format_text_passthrough() {
        let output = ToolOutput::Text("Comment created".into());
        let result = format_output(output, None, None).unwrap();
        assert_eq!(result, "Comment created");
    }

    #[test]
    fn test_format_default_is_markdown() {
        let output = ToolOutput::Issues(vec![sample_issue()]);
        let result = format_output(output, None, None).unwrap();
        assert!(result.contains("gh#1"));
    }

    #[test]
    fn test_format_single_issue() {
        let output = ToolOutput::SingleIssue(Box::new(sample_issue()));
        let result = format_output(output, Some("markdown"), None).unwrap();
        assert!(result.contains("gh#1"));
    }

    fn sample_mr() -> devboy_core::MergeRequest {
        devboy_core::MergeRequest {
            key: "pr#1".into(),
            title: "Test PR".into(),
            description: None,
            state: "open".into(),
            source: "github".into(),
            source_branch: "feature".into(),
            target_branch: "main".into(),
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
    fn test_format_merge_requests() {
        let output = ToolOutput::MergeRequests(vec![sample_mr()]);
        let result = format_output(output, Some("markdown"), None).unwrap();
        assert!(result.contains("pr#1"));
    }

    #[test]
    fn test_format_single_merge_request() {
        let output = ToolOutput::SingleMergeRequest(Box::new(sample_mr()));
        let result = format_output(output, Some("compact"), None).unwrap();
        assert!(result.contains("pr#1"));
    }

    #[test]
    fn test_format_discussions() {
        let output = ToolOutput::Discussions(vec![devboy_core::Discussion {
            id: "d1".into(),
            resolved: false,
            resolved_by: None,
            comments: vec![devboy_core::Comment {
                id: "c1".into(),
                body: "Review comment".into(),
                author: None,
                created_at: None,
                updated_at: None,
                position: None,
            }],
            position: None,
        }]);
        let result = format_output(output, Some("markdown"), None).unwrap();
        assert!(result.contains("Review comment"));
    }

    #[test]
    fn test_format_diffs() {
        let output = ToolOutput::Diffs(vec![devboy_core::FileDiff {
            file_path: "src/main.rs".into(),
            old_path: None,
            new_file: false,
            deleted_file: false,
            renamed_file: false,
            diff: "+added line".into(),
            additions: Some(1),
            deletions: Some(0),
        }]);
        let result = format_output(output, Some("markdown"), None).unwrap();
        assert!(result.contains("src/main.rs"));
    }

    #[test]
    fn test_format_comments() {
        let output = ToolOutput::Comments(vec![devboy_core::Comment {
            id: "c1".into(),
            body: "A comment body".into(),
            author: None,
            created_at: None,
            updated_at: None,
            position: None,
        }]);
        let result = format_output(output, Some("json"), None).unwrap();
        assert!(result.contains("A comment body"));
    }

    #[test]
    fn test_format_with_custom_pipeline_config() {
        let output = ToolOutput::Issues(vec![sample_issue()]);
        let config = PipelineConfig {
            max_items: 1,
            max_chars: 500,
            ..PipelineConfig::default()
        };
        let result = format_output(output, Some("compact"), Some(config)).unwrap();
        assert!(result.contains("gh#1"));
    }
}
