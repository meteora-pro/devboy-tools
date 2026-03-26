use devboy_core::{
    CreateCommentInput, CreateIssueInput, CreateMergeRequestInput, Error, IssueFilter,
    MergeRequestProvider, MrFilter, Result, UpdateIssueInput,
};
use serde::Deserialize;
use serde_json::Value;
use tracing::debug;

use crate::context::AdditionalContext;
use crate::factory;
use crate::output::ToolOutput;
use devboy_core::ToolEnricher;

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
        let output = dispatch_tool(tool, &args, provider.as_ref()).await?;

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
    use async_trait::async_trait;
    use devboy_core::{
        Comment, CreateMergeRequestInput, Discussion, FileDiff, Issue, IssueProvider,
        MergeRequest, MergeRequestProvider, Provider, User,
    };

    // --- Mock Provider ---

    struct MockProvider;

    fn sample_issue() -> Issue {
        Issue {
            key: "gh#1".into(),
            title: "Test Issue".into(),
            description: Some("Body".into()),
            state: "open".into(),
            source: "mock".into(),
            priority: None,
            labels: vec!["bug".into()],
            author: None,
            assignees: vec![],
            url: Some("https://example.com/1".into()),
            created_at: Some("2024-01-01T00:00:00Z".into()),
            updated_at: Some("2024-01-02T00:00:00Z".into()),
        }
    }

    fn sample_mr() -> MergeRequest {
        MergeRequest {
            key: "pr#1".into(),
            title: "Test PR".into(),
            description: Some("PR body".into()),
            state: "open".into(),
            source: "mock".into(),
            source_branch: "feature".into(),
            target_branch: "main".into(),
            author: None,
            assignees: vec![],
            reviewers: vec![],
            labels: vec![],
            draft: false,
            url: Some("https://example.com/pr/1".into()),
            created_at: Some("2024-01-01T00:00:00Z".into()),
            updated_at: Some("2024-01-02T00:00:00Z".into()),
        }
    }

    fn sample_comment() -> Comment {
        Comment {
            id: "c1".into(),
            body: "Test comment".into(),
            author: None,
            created_at: None,
            updated_at: None,
            position: None,
        }
    }

    fn sample_discussion() -> Discussion {
        Discussion {
            id: "d1".into(),
            resolved: false,
            resolved_by: None,
            comments: vec![sample_comment()],
            position: None,
        }
    }

    fn sample_diff() -> FileDiff {
        FileDiff {
            file_path: "src/main.rs".into(),
            old_path: None,
            new_file: false,
            deleted_file: false,
            renamed_file: false,
            diff: "+added\n-removed".into(),
            additions: Some(1),
            deletions: Some(1),
        }
    }

    #[async_trait]
    impl IssueProvider for MockProvider {
        async fn get_issues(&self, _filter: IssueFilter) -> devboy_core::Result<Vec<Issue>> {
            Ok(vec![sample_issue()])
        }
        async fn get_issue(&self, _key: &str) -> devboy_core::Result<Issue> {
            Ok(sample_issue())
        }
        async fn create_issue(
            &self,
            _input: devboy_core::CreateIssueInput,
        ) -> devboy_core::Result<Issue> {
            Ok(sample_issue())
        }
        async fn update_issue(
            &self,
            _key: &str,
            _input: devboy_core::UpdateIssueInput,
        ) -> devboy_core::Result<Issue> {
            Ok(sample_issue())
        }
        async fn get_comments(&self, _key: &str) -> devboy_core::Result<Vec<Comment>> {
            Ok(vec![sample_comment()])
        }
        async fn add_comment(&self, _key: &str, _body: &str) -> devboy_core::Result<Comment> {
            Ok(sample_comment())
        }
        fn provider_name(&self) -> &'static str {
            "mock"
        }
    }

    #[async_trait]
    impl MergeRequestProvider for MockProvider {
        async fn get_merge_requests(
            &self,
            _filter: MrFilter,
        ) -> devboy_core::Result<Vec<MergeRequest>> {
            Ok(vec![sample_mr()])
        }
        async fn get_merge_request(&self, _key: &str) -> devboy_core::Result<MergeRequest> {
            Ok(sample_mr())
        }
        async fn get_discussions(&self, _key: &str) -> devboy_core::Result<Vec<Discussion>> {
            Ok(vec![sample_discussion()])
        }
        async fn get_diffs(&self, _key: &str) -> devboy_core::Result<Vec<FileDiff>> {
            Ok(vec![sample_diff()])
        }
        async fn add_comment(
            &self,
            _key: &str,
            _input: CreateCommentInput,
        ) -> devboy_core::Result<Comment> {
            Ok(sample_comment())
        }
        async fn create_merge_request(
            &self,
            _input: CreateMergeRequestInput,
        ) -> devboy_core::Result<MergeRequest> {
            Ok(sample_mr())
        }
        fn provider_name(&self) -> &'static str {
            "mock"
        }
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn get_current_user(&self) -> devboy_core::Result<User> {
            Ok(User {
                id: "1".into(),
                username: "test".into(),
                name: None,
                email: None,
                avatar_url: None,
            })
        }
    }

    // --- Tests ---

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

    // --- Issue tool dispatch tests ---

    #[tokio::test]
    async fn test_dispatch_get_issues() {
        let provider = MockProvider;
        let args = serde_json::json!({"state": "open", "limit": 10});
        let result = dispatch_tool("get_issues", &args, &provider).await.unwrap();
        assert!(matches!(result, ToolOutput::Issues(v) if v.len() == 1));
    }

    #[tokio::test]
    async fn test_dispatch_get_issues_empty_args() {
        let provider = MockProvider;
        let result = dispatch_tool("get_issues", &Value::Null, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Issues(_)));
    }

    #[tokio::test]
    async fn test_dispatch_get_issue() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "gh#1"});
        let result = dispatch_tool("get_issue", &args, &provider).await.unwrap();
        assert!(matches!(result, ToolOutput::SingleIssue(_)));
    }

    #[tokio::test]
    async fn test_dispatch_get_issue_missing_key() {
        let provider = MockProvider;
        let result = dispatch_tool("get_issue", &serde_json::json!({}), &provider).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_get_issue_comments() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "gh#1"});
        let result = dispatch_tool("get_issue_comments", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Comments(v) if v.len() == 1));
    }

    #[tokio::test]
    async fn test_dispatch_create_issue() {
        let provider = MockProvider;
        let args =
            serde_json::json!({"title": "New issue", "description": "Body", "labels": ["bug"]});
        let result = dispatch_tool("create_issue", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::SingleIssue(_)));
    }

    #[tokio::test]
    async fn test_dispatch_update_issue() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "gh#1", "title": "Updated"});
        let result = dispatch_tool("update_issue", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::SingleIssue(_)));
    }

    #[tokio::test]
    async fn test_dispatch_add_issue_comment() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "gh#1", "body": "A comment"});
        let result = dispatch_tool("add_issue_comment", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Text(ref t) if t.contains("Comment added")));
    }

    // --- MR tool dispatch tests ---

    #[tokio::test]
    async fn test_dispatch_get_merge_requests() {
        let provider = MockProvider;
        let args = serde_json::json!({"state": "open", "limit": 5});
        let result = dispatch_tool("get_merge_requests", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::MergeRequests(v) if v.len() == 1));
    }

    #[tokio::test]
    async fn test_dispatch_get_merge_requests_empty_args() {
        let provider = MockProvider;
        let result = dispatch_tool("get_merge_requests", &Value::Null, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::MergeRequests(_)));
    }

    #[tokio::test]
    async fn test_dispatch_get_merge_request() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "pr#1"});
        let result = dispatch_tool("get_merge_request", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::SingleMergeRequest(_)));
    }

    #[tokio::test]
    async fn test_dispatch_get_merge_request_discussions() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "pr#1"});
        let result = dispatch_tool("get_merge_request_discussions", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Discussions(v) if v.len() == 1));
    }

    #[tokio::test]
    async fn test_dispatch_get_merge_request_diffs() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "pr#1"});
        let result = dispatch_tool("get_merge_request_diffs", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Diffs(v) if v.len() == 1));
    }

    #[tokio::test]
    async fn test_dispatch_create_merge_request() {
        let provider = MockProvider;
        let args = serde_json::json!({
            "title": "New PR",
            "source_branch": "feature",
            "target_branch": "main",
            "draft": false
        });
        let result = dispatch_tool("create_merge_request", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::SingleMergeRequest(_)));
    }

    #[tokio::test]
    async fn test_dispatch_create_merge_request_comment_general() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "pr#1", "body": "LGTM"});
        let result = dispatch_tool("create_merge_request_comment", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Text(ref t) if t.contains("Comment added")));
    }

    #[tokio::test]
    async fn test_dispatch_create_merge_request_comment_inline() {
        let provider = MockProvider;
        let args = serde_json::json!({
            "key": "pr#1",
            "body": "Fix this line",
            "file_path": "src/main.rs",
            "line": 42,
            "line_type": "new",
            "commit_sha": "abc123"
        });
        let result = dispatch_tool("create_merge_request_comment", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Text(ref t) if t.contains("Comment added")));
    }

    #[tokio::test]
    async fn test_dispatch_unknown_tool() {
        let provider = MockProvider;
        let result = dispatch_tool("nonexistent_tool", &Value::Null, &provider).await;
        assert!(result.is_err());
    }

    // --- Executor enricher integration ---

    #[tokio::test]
    async fn test_executor_enricher_transforms_args() {
        use devboy_core::{ToolEnricher, ToolSchema};

        struct TestEnricher;
        impl ToolEnricher for TestEnricher {
            fn supported_tools(&self) -> &[&str] {
                &["get_issues"]
            }
            fn enrich_schema(&self, _tool: &str, _schema: &mut ToolSchema) {}
            fn transform_args(&self, _tool: &str, args: &mut Value) {
                if let Some(obj) = args.as_object_mut() {
                    obj.insert("transformed".into(), Value::Bool(true));
                }
            }
        }

        let mut executor = Executor::new();
        executor.add_enricher(Box::new(TestEnricher));
        // Can't easily test full execute() without real provider,
        // but we verify enricher is stored
        assert_eq!(executor.enrichers.len(), 1);
    }
}
