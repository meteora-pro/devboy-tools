use devboy_core::{
    CreateCommentInput, CreateIssueInput, CreateMergeRequestInput, Error, IssueFilter,
    MergeRequestProvider, MrFilter, Result, UpdateIssueInput,
};
use serde::Deserialize;
use serde_json::Value;
use tracing::debug;

use crate::context::AdditionalContext;
use crate::enricher::ToolEnricher;
use crate::factory;
use crate::output::ToolOutput;

/// Tool execution engine.
///
/// Manages enrichers and dispatches tool calls to providers.
/// Stateless per call — provider is created from `AdditionalContext` each time.
pub struct Executor {
    enrichers: Vec<Box<dyn ToolEnricher>>,
}

impl Executor {
    pub fn new() -> Self {
        Self {
            enrichers: Vec::new(),
        }
    }

    /// Register an enricher (provider, pipeline, or custom).
    /// Enrichers are applied in registration order.
    pub fn add_enricher(&mut self, enricher: Box<dyn ToolEnricher>) {
        self.enrichers.push(enricher);
    }

    /// Execute a tool with the given arguments and context.
    ///
    /// Flow:
    /// 1. Pre-execute: enrichers transform args
    /// 2. Create provider from context (cheap, stack-allocated)
    /// 3. Dispatch tool call to provider method
    /// 4. Post-execute: enrichers transform output
    /// 5. Return typed ToolOutput
    pub async fn execute(
        &self,
        tool: &str,
        args: Value,
        ctx: &AdditionalContext,
    ) -> Result<ToolOutput> {
        let mut args = args;

        // Pre-execute: enrichers transform args
        for enricher in &self.enrichers {
            if enricher.supported_tools().contains(&tool) {
                enricher.transform_args(tool, &mut args);
            }
        }

        debug!(
            tool = tool,
            provider = ctx.provider.provider_name(),
            "executing tool"
        );

        // Create provider from context
        let provider = factory::create_provider(&ctx.provider)?;

        // Dispatch to tool handler
        let mut output = dispatch_tool(tool, &args, provider.as_ref()).await?;

        // Post-execute: enrichers transform output
        for enricher in &self.enrichers {
            if enricher.supported_tools().contains(&tool) {
                enricher.transform_output(tool, &mut output);
            }
        }

        Ok(output)
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

// --- Tool dispatch ---

/// Dispatch a tool call to the appropriate provider method.
async fn dispatch_tool(
    tool: &str,
    args: &Value,
    provider: &dyn devboy_core::Provider,
) -> Result<ToolOutput> {
    match tool {
        // Issue tools
        "get_issues" => execute_get_issues(provider, args).await,
        "get_issue" => execute_get_issue(provider, args).await,
        "get_issue_comments" => execute_get_issue_comments(provider, args).await,
        "create_issue" => execute_create_issue(provider, args).await,
        "update_issue" => execute_update_issue(provider, args).await,
        "add_issue_comment" => execute_add_issue_comment(provider, args).await,

        // Merge request tools
        "get_merge_requests" => execute_get_merge_requests(provider, args).await,
        "get_merge_request" => execute_get_merge_request(provider, args).await,
        "get_merge_request_discussions" => {
            execute_get_merge_request_discussions(provider, args).await
        }
        "get_merge_request_diffs" => execute_get_merge_request_diffs(provider, args).await,
        "create_merge_request" => execute_create_merge_request(provider, args).await,
        "create_merge_request_comment" => {
            execute_create_merge_request_comment(provider, args).await
        }

        _ => Err(Error::NotFound(format!("unknown tool: {tool}"))),
    }
}

// --- Issue tool handlers ---

#[derive(Deserialize, Default)]
struct GetIssuesParams {
    state: Option<String>,
    search: Option<String>,
    labels: Option<Vec<String>>,
    assignee: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
    sort_by: Option<String>,
    sort_order: Option<String>,
}

async fn execute_get_issues(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetIssuesParams = serde_json::from_value(args.clone()).unwrap_or_default();
    let filter = IssueFilter {
        state: params.state,
        search: params.search,
        labels: params.labels,
        assignee: params.assignee,
        limit: params.limit.or(Some(20)),
        offset: params.offset,
        sort_by: params.sort_by,
        sort_order: params.sort_order,
    };
    let issues = provider.get_issues(filter).await?;
    Ok(ToolOutput::Issues(issues))
}

#[derive(Deserialize)]
struct KeyParam {
    key: String,
}

async fn execute_get_issue(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: KeyParam = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("missing 'key' parameter: {e}")))?;
    let issue = provider.get_issue(&params.key).await?;
    Ok(ToolOutput::SingleIssue(Box::new(issue)))
}

async fn execute_get_issue_comments(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: KeyParam = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("missing 'key' parameter: {e}")))?;
    let comments = provider.get_comments(&params.key).await?;
    Ok(ToolOutput::Comments(comments))
}

#[derive(Deserialize)]
struct CreateIssueParams {
    title: String,
    description: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    assignees: Vec<String>,
    priority: Option<String>,
}

async fn execute_create_issue(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: CreateIssueParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid create_issue params: {e}")))?;
    let input = CreateIssueInput {
        title: params.title,
        description: params.description,
        labels: params.labels,
        assignees: params.assignees,
        priority: params.priority,
    };
    let issue = provider.create_issue(input).await?;
    Ok(ToolOutput::SingleIssue(Box::new(issue)))
}

#[derive(Deserialize)]
struct UpdateIssueParams {
    key: String,
    title: Option<String>,
    description: Option<String>,
    state: Option<String>,
    labels: Option<Vec<String>>,
    assignees: Option<Vec<String>>,
    priority: Option<String>,
}

async fn execute_update_issue(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: UpdateIssueParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid update_issue params: {e}")))?;
    let input = UpdateIssueInput {
        title: params.title,
        description: params.description,
        state: params.state,
        labels: params.labels,
        assignees: params.assignees,
        priority: params.priority,
    };
    let issue = provider.update_issue(&params.key, input).await?;
    Ok(ToolOutput::SingleIssue(Box::new(issue)))
}

#[derive(Deserialize)]
struct AddCommentParams {
    key: String,
    body: String,
}

async fn execute_add_issue_comment(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: AddCommentParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid add_issue_comment params: {e}")))?;
    let comment =
        devboy_core::IssueProvider::add_comment(provider, &params.key, &params.body).await?;
    Ok(ToolOutput::Text(format!(
        "Comment added to {} (id: {})",
        params.key, comment.id
    )))
}

// --- Merge request tool handlers ---

#[derive(Deserialize, Default)]
struct GetMergeRequestsParams {
    state: Option<String>,
    author: Option<String>,
    labels: Option<Vec<String>>,
    source_branch: Option<String>,
    target_branch: Option<String>,
    limit: Option<u32>,
}

async fn execute_get_merge_requests(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetMergeRequestsParams = serde_json::from_value(args.clone()).unwrap_or_default();
    let filter = MrFilter {
        state: params.state,
        source_branch: params.source_branch,
        target_branch: params.target_branch,
        author: params.author,
        labels: params.labels,
        limit: params.limit.or(Some(20)),
    };
    let mrs = provider.get_merge_requests(filter).await?;
    Ok(ToolOutput::MergeRequests(mrs))
}

async fn execute_get_merge_request(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: KeyParam = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("missing 'key' parameter: {e}")))?;
    let mr = provider.get_merge_request(&params.key).await?;
    Ok(ToolOutput::SingleMergeRequest(Box::new(mr)))
}

async fn execute_get_merge_request_discussions(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: KeyParam = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("missing 'key' parameter: {e}")))?;
    let discussions = provider.get_discussions(&params.key).await?;
    Ok(ToolOutput::Discussions(discussions))
}

async fn execute_get_merge_request_diffs(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: KeyParam = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("missing 'key' parameter: {e}")))?;
    let diffs = provider.get_diffs(&params.key).await?;
    Ok(ToolOutput::Diffs(diffs))
}

#[derive(Deserialize)]
struct CreateMergeRequestParams {
    title: String,
    description: Option<String>,
    source_branch: String,
    target_branch: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    reviewers: Vec<String>,
}

async fn execute_create_merge_request(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: CreateMergeRequestParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid create_merge_request params: {e}")))?;
    let input = CreateMergeRequestInput {
        title: params.title,
        description: params.description,
        source_branch: params.source_branch,
        target_branch: params.target_branch,
        draft: params.draft,
        labels: params.labels,
        reviewers: params.reviewers,
    };
    let mr = provider.create_merge_request(input).await?;
    Ok(ToolOutput::SingleMergeRequest(Box::new(mr)))
}

#[derive(Deserialize)]
struct CreateMrCommentParams {
    key: String,
    body: String,
    file_path: Option<String>,
    line: Option<u32>,
    line_type: Option<String>,
    commit_sha: Option<String>,
    discussion_id: Option<String>,
}

async fn execute_create_merge_request_comment(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: CreateMrCommentParams = serde_json::from_value(args.clone()).map_err(|e| {
        Error::InvalidData(format!("invalid create_merge_request_comment params: {e}"))
    })?;

    let position = params.file_path.map(|fp| devboy_core::CodePosition {
        file_path: fp,
        line: params.line.unwrap_or(1),
        line_type: params.line_type.unwrap_or_else(|| "new".into()),
        commit_sha: params.commit_sha,
    });

    let input = CreateCommentInput {
        body: params.body,
        position,
        discussion_id: params.discussion_id,
    };

    let comment = MergeRequestProvider::add_comment(provider, &params.key, input).await?;
    Ok(ToolOutput::Text(format!(
        "Comment added to {} (id: {})",
        params.key, comment.id
    )))
}

/// List of all tool names supported by the executor.
pub const SUPPORTED_TOOLS: &[&str] = &[
    "get_issues",
    "get_issue",
    "get_issue_comments",
    "create_issue",
    "update_issue",
    "add_issue_comment",
    "get_merge_requests",
    "get_merge_request",
    "get_merge_request_discussions",
    "get_merge_request_diffs",
    "create_merge_request",
    "create_merge_request_comment",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_executor_new() {
        let executor = Executor::new();
        assert!(executor.enrichers.is_empty());
    }

    #[test]
    fn test_supported_tools_contains_all() {
        assert!(SUPPORTED_TOOLS.contains(&"get_issues"));
        assert!(SUPPORTED_TOOLS.contains(&"get_merge_requests"));
        assert!(SUPPORTED_TOOLS.contains(&"create_merge_request_comment"));
        assert_eq!(SUPPORTED_TOOLS.len(), 12);
    }

    #[test]
    fn test_dispatch_unknown_tool() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // We can't easily create a mock provider here without mockall setup,
            // but we can test the unknown tool path
            let config = crate::context::ProviderConfig::GitHub {
                base_url: "https://api.github.com".into(),
                access_token: "test".into(),
                scope: crate::context::GitHubScope::Repository {
                    owner: "test".into(),
                    repo: "test".into(),
                },
                extra: std::collections::HashMap::new(),
            };
            let provider = factory::create_provider(&config).unwrap();
            let result = dispatch_tool("nonexistent_tool", &Value::Null, provider.as_ref()).await;
            assert!(result.is_err());
        });
    }
}
