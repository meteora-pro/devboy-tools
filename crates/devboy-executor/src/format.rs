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
        ToolOutput::Pipeline(info) => Ok(format_pipeline(&info)),
        ToolOutput::JobLog(log) => Ok(format_job_log(&log)),
        ToolOutput::Statuses(statuses) => Ok(format_statuses(&statuses)),
        ToolOutput::Users(users) => Ok(format_users(&users)),
        ToolOutput::Text(text) => Ok(text),
    }
}

/// Format issue statuses as a markdown table.
fn format_statuses(statuses: &[devboy_core::IssueStatus]) -> String {
    if statuses.is_empty() {
        return "No statuses found.".to_string();
    }

    let mut output = String::from("# Available Statuses\n\n");
    output.push_str("| ID | Name | Category | Color | Order |\n");
    output.push_str("|---|---|---|---|---|\n");

    for s in statuses {
        let color = s.color.as_deref().unwrap_or("-");
        let order = s
            .order
            .map(|o| o.to_string())
            .unwrap_or_else(|| "-".to_string());
        output.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            s.id, s.name, s.category, color, order
        ));
    }

    output
}

/// Format users as a markdown table.
fn format_users(users: &[devboy_core::User]) -> String {
    if users.is_empty() {
        return "No users found.".to_string();
    }

    let mut output = String::from("# Users\n\n");
    output.push_str("| ID | Username | Name | Email |\n");
    output.push_str("|---|---|---|---|\n");

    for u in users {
        let name = u.name.as_deref().unwrap_or("-");
        let email = u.email.as_deref().unwrap_or("-");
        output.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            u.id, u.username, name, email
        ));
    }

    output
}

/// Format pipeline status as markdown.
fn format_pipeline(info: &devboy_core::PipelineInfo) -> String {
    let status_icon = match info.status {
        devboy_core::PipelineStatus::Success => "✅",
        devboy_core::PipelineStatus::Failed => "❌",
        devboy_core::PipelineStatus::Running => "🔄",
        devboy_core::PipelineStatus::Pending => "⏳",
        devboy_core::PipelineStatus::Canceled => "🚫",
        _ => "❓",
    };

    let mut output = format!(
        "# Pipeline {}\n\n{} **Status:** {} | **Ref:** `{}` | **SHA:** `{}`",
        info.id,
        status_icon,
        info.status.as_str(),
        info.reference,
        &info.sha[..7.min(info.sha.len())]
    );

    if let Some(url) = &info.url {
        output.push_str(&format!("\n🔗 {url}"));
    }

    if let Some(duration) = info.duration {
        output.push_str(&format!("\n⏱️ Duration: {}s", duration));
    }

    // Summary
    let s = &info.summary;
    output.push_str(&format!(
        "\n\n**Summary:** {} total | ✅ {} | ❌ {} | 🔄 {} | ⏳ {} | 🚫 {} | ⏭️ {}",
        s.total, s.success, s.failed, s.running, s.pending, s.canceled, s.skipped
    ));

    // Stages/jobs
    for stage in &info.stages {
        output.push_str(&format!("\n\n## {}\n", stage.name));
        for job in &stage.jobs {
            let job_icon = match job.status {
                devboy_core::PipelineStatus::Success => "✅",
                devboy_core::PipelineStatus::Failed => "❌",
                devboy_core::PipelineStatus::Running => "🔄",
                devboy_core::PipelineStatus::Pending => "⏳",
                _ => "❓",
            };
            let dur = job.duration.map(|d| format!(" ({d}s)")).unwrap_or_default();
            output.push_str(&format!("\n{} **{}**{}", job_icon, job.name, dur));
            if let Some(url) = &job.url {
                output.push_str(&format!(" — [logs]({url})"));
            }
        }
    }

    // Failed jobs with errors
    if !info.failed_jobs.is_empty() {
        output.push_str("\n\n## Failed Jobs\n");
        for fj in &info.failed_jobs {
            output.push_str(&format!("\n### ❌ {} (job {})\n", fj.name, fj.id));
            if let Some(snippet) = &fj.error_snippet {
                output.push_str(&format!("\n```\n{snippet}\n```\n"));
            }
        }
    }

    output
}

/// Format job log output as markdown.
fn format_job_log(log: &devboy_core::JobLogOutput) -> String {
    let mut output = format!("# Job Log ({})\n\n", log.job_id);
    output.push_str(&format!("**Mode:** {}", log.mode));
    if let Some(total) = log.total_lines {
        output.push_str(&format!(" | **Total lines:** {total}"));
    }
    output.push_str(&format!("\n\n```\n{}\n```", log.content));
    output
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

    #[test]
    fn test_format_pipeline() {
        let output = ToolOutput::Pipeline(Box::new(devboy_core::PipelineInfo {
            id: "100".into(),
            status: devboy_core::PipelineStatus::Failed,
            reference: "main".into(),
            sha: "abc123def".into(),
            url: Some("https://example.com/pipeline/100".into()),
            duration: Some(120),
            coverage: Some(85.5),
            summary: devboy_core::PipelineSummary {
                total: 3,
                success: 2,
                failed: 1,
                ..Default::default()
            },
            stages: vec![devboy_core::PipelineStage {
                name: "build".into(),
                jobs: vec![devboy_core::PipelineJob {
                    id: "1".into(),
                    name: "compile".into(),
                    status: devboy_core::PipelineStatus::Success,
                    url: None,
                    duration: Some(30),
                }],
            }],
            failed_jobs: vec![devboy_core::FailedJob {
                id: "2".into(),
                name: "test".into(),
                url: None,
                error_snippet: Some("error: test failed".into()),
            }],
        }));
        let result = format_output(output, None, None).unwrap();
        assert!(result.contains("Pipeline 100"));
        assert!(result.contains("failed"));
        assert!(result.contains("main"));
        assert!(result.contains("120s"));
        assert!(result.contains("compile"));
        assert!(result.contains("error: test failed"));
    }

    #[test]
    fn test_format_job_log() {
        let output = ToolOutput::JobLog(Box::new(devboy_core::JobLogOutput {
            job_id: "202".into(),
            job_name: Some("test".into()),
            content: "error: assertion failed\nat src/test.rs:42".into(),
            mode: "smart".into(),
            total_lines: Some(100),
        }));
        let result = format_output(output, None, None).unwrap();
        assert!(result.contains("Job Log"));
        assert!(result.contains("202"));
        assert!(result.contains("smart"));
        assert!(result.contains("assertion failed"));
    }
}
