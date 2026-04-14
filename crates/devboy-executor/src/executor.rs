use devboy_core::{
    CreateCommentInput, CreateIssueInput, CreateMergeRequestInput, Error, GetPipelineInput,
    GetUsersOptions, IssueFilter, IssueProvider, JobLogMode, JobLogOptions, MeetingFilter,
    MeetingNotesProvider, MergeRequestProvider, MrFilter, PipelineProvider, Result, ToolCategory,
    UpdateIssueInput,
};
use serde::Deserialize;
use serde_json::Value;
use tracing::debug;

use crate::context::AdditionalContext;
use crate::factory;
use crate::output::{ResultMeta, ToolOutput};
use devboy_core::ToolEnricher;

/// Maximum file size for upload / download asset operations (10 MB).
const MAX_FILE_SIZE: usize = 10 * 1024 * 1024;

/// Deserialize a value that can be either a string or a number into Option<String>.
/// Enricher may transform priority "high" → 2 (number), but executor needs String.
fn deserialize_string_or_number<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<Value> = Option::deserialize(deserializer)?;
    Ok(value.map(|v| match v {
        Value::String(s) => s,
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }))
}

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

    /// List available tools with enriched schemas.
    ///
    /// 1. Starts with base tool definitions
    /// 2. Keeps only tools whose category is supported by at least one enricher
    /// 3. Applies schema enrichment from enrichers that support each tool's category
    pub fn list_tools(&self) -> Vec<crate::tools::ToolDefinition> {
        let mut tools = crate::tools::base_tool_definitions();

        // Collect all supported categories from enrichers
        let supported_categories: std::collections::HashSet<devboy_core::ToolCategory> = self
            .enrichers
            .iter()
            .flat_map(|e| e.supported_categories().iter().copied())
            .collect();

        // Keep only tools whose category is supported
        tools.retain(|t| supported_categories.contains(&t.category));

        // Apply schema enrichment from enrichers that support each tool's category
        for enricher in &self.enrichers {
            let cats = enricher.supported_categories();
            for tool in &mut tools {
                if cats.contains(&tool.category) {
                    enricher.enrich_schema(&tool.name, &mut tool.input_schema);
                }
            }
        }

        tools
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
        // Look up tool category from base definitions for matching
        let tool_category = crate::tools::base_tool_definitions()
            .iter()
            .find(|t| t.name == tool)
            .map(|t| t.category);
        for enricher in &self.enrichers {
            if let Some(cat) = tool_category
                && enricher.supported_categories().contains(&cat)
            {
                enricher.transform_args(tool, &mut args);
            }
        }

        debug!(
            tool = tool,
            provider = ctx.provider.provider_name(),
            "executing tool"
        );

        // Dispatch based on tool category
        let output = if tool_category == Some(ToolCategory::MeetingNotes) {
            let provider = factory::create_meeting_notes_provider(&ctx.provider)?;
            dispatch_meeting_tool(tool, &args, provider.as_ref()).await?
        } else {
            let provider = factory::create_provider(&ctx.provider, ctx.proxy.as_ref())?;
            dispatch_tool(tool, &args, provider.as_ref()).await?
        };

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
        "get_issue_relations" => execute_get_issue_relations(provider, args).await,
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

        // Pipeline tools
        "get_pipeline" => execute_get_pipeline(provider, args).await,
        "get_job_logs" => execute_get_job_logs(provider, args).await,

        // Status / user / link tools
        "get_available_statuses" => execute_get_available_statuses(provider).await,
        "get_users" => execute_get_users(provider, args).await,
        "link_issues" => execute_link_issues(provider, args).await,
        "unlink_issues" => execute_unlink_issues(provider, args).await,

        // Epic tools (issue-based with "epic" label convention)
        "get_epics" => execute_get_epics(provider, args).await,
        "create_epic" => execute_create_epic(provider, args).await,
        "update_epic" => execute_update_epic(provider, args).await,

        // MR update
        "update_merge_request" => execute_update_merge_request(provider, args).await,

        // Asset tools
        "get_assets" => execute_get_assets(provider, args).await,
        "upload_asset" => execute_upload_asset(provider, args).await,
        "download_asset" => execute_download_asset(provider, args).await,
        "delete_asset" => execute_delete_asset(provider, args).await,

        _ => Err(Error::NotFound(format!("unknown tool: {tool}"))),
    }
}

/// Dispatch a meeting notes tool call.
async fn dispatch_meeting_tool(
    tool: &str,
    args: &Value,
    provider: &dyn MeetingNotesProvider,
) -> Result<ToolOutput> {
    match tool {
        "get_meeting_notes" => execute_get_meeting_notes(provider, args).await,
        "get_meeting_transcript" => execute_get_meeting_transcript(provider, args).await,
        "search_meeting_notes" => execute_search_meeting_notes(provider, args).await,
        _ => Err(Error::NotFound(format!("unknown meeting tool: {tool}"))),
    }
}

// --- Meeting notes tool handlers ---

#[derive(Deserialize, Default)]
struct GetMeetingNotesParams {
    from_date: Option<String>,
    to_date: Option<String>,
    participants: Option<Vec<String>>,
    host_email: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

async fn execute_get_meeting_notes(
    provider: &dyn MeetingNotesProvider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetMeetingNotesParams = serde_json::from_value(args.clone()).unwrap_or_default();
    let filter = MeetingFilter {
        keyword: None,
        from_date: params.from_date,
        to_date: params.to_date,
        participants: params.participants,
        host_email: params.host_email,
        limit: params.limit,
        skip: params.offset,
    };
    let result = provider.get_meetings(filter).await?;
    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::MeetingNotes(result.items, Some(meta)))
}

#[derive(Deserialize)]
struct GetMeetingTranscriptParams {
    meeting_id: String,
}

async fn execute_get_meeting_transcript(
    provider: &dyn MeetingNotesProvider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetMeetingTranscriptParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid params: {e}")))?;
    let transcript = provider.get_transcript(&params.meeting_id).await?;
    Ok(ToolOutput::MeetingTranscript(Box::new(transcript)))
}

#[derive(Deserialize)]
struct SearchMeetingNotesParams {
    query: String,
    from_date: Option<String>,
    to_date: Option<String>,
    participants: Option<Vec<String>>,
    host_email: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

async fn execute_search_meeting_notes(
    provider: &dyn MeetingNotesProvider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: SearchMeetingNotesParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid params: {e}")))?;
    let filter = MeetingFilter {
        keyword: None,
        from_date: params.from_date,
        to_date: params.to_date,
        participants: params.participants,
        host_email: params.host_email,
        limit: params.limit,
        skip: params.offset,
    };
    let result = provider.search_meetings(&params.query, filter).await?;
    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::MeetingNotes(result.items, Some(meta)))
}

// --- Issue tool handlers ---

#[derive(Deserialize, Default)]
struct GetIssuesParams {
    state: Option<String>,
    #[serde(rename = "stateCategory")]
    state_category: Option<String>,
    search: Option<String>,
    labels: Option<Vec<String>>,
    #[serde(rename = "labelsOperator")]
    labels_operator: Option<String>,
    assignee: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
    sort_by: Option<String>,
    sort_order: Option<String>,
    /// Token budget for response size control (consumed by format layer via execute_and_format).
    #[allow(dead_code)]
    budget: Option<usize>,
}

async fn execute_get_issues(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetIssuesParams = serde_json::from_value(args.clone()).unwrap_or_default();
    let filter = IssueFilter {
        state: params.state,
        state_category: params.state_category,
        search: params.search,
        labels: params.labels,
        labels_operator: params.labels_operator,
        assignee: params.assignee,
        limit: params.limit.or(Some(20)),
        offset: params.offset,
        sort_by: params.sort_by,
        sort_order: params.sort_order,
    };
    let result = provider.get_issues(filter).await?;
    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::Issues(result.items, Some(meta)))
}

#[derive(Deserialize)]
struct KeyParam {
    key: String,
    /// Token budget for response size control (consumed by format layer via execute_and_format).
    #[serde(default)]
    #[allow(dead_code)]
    budget: Option<usize>,
}

#[derive(Deserialize)]
struct GetIssueParams {
    key: String,
    #[serde(default = "default_true", rename = "includeComments")]
    include_comments: bool,
    #[serde(default = "default_true", rename = "includeRelations")]
    include_relations: bool,
    #[serde(default)]
    #[allow(dead_code)]
    budget: Option<usize>,
}

fn default_true() -> bool {
    true
}

async fn execute_get_issue(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetIssueParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("missing 'key' parameter: {e}")))?;
    let issue = provider.get_issue(&params.key).await?;

    // If no extras requested, return just the issue
    if !params.include_comments && !params.include_relations {
        return Ok(ToolOutput::SingleIssue(Box::new(issue)));
    }

    // Build a composite JSON with issue + optional comments/relations
    let mut result = serde_json::to_value(&issue).unwrap_or_default();
    let mut has_extras = false;

    if params.include_comments
        && let Ok(comments_result) = provider.get_comments(&params.key).await
    {
        result["comments"] = serde_json::to_value(&comments_result.items).unwrap_or_default();
        result["comments_count"] = serde_json::json!(comments_result.items.len());
        has_extras = true;
    }

    if params.include_relations
        && let Ok(relations) = provider.get_issue_relations(&params.key).await
    {
        result["relations"] = serde_json::to_value(&relations).unwrap_or_default();
        if issue.subtasks.is_empty() && !relations.subtasks.is_empty() {
            result["subtasks"] = serde_json::to_value(&relations.subtasks).unwrap_or_default();
        }
        result["subtasks_count"] =
            serde_json::json!(issue.subtasks.len().max(relations.subtasks.len()));
        has_extras = true;
    }

    // If no extras were actually fetched, return simple issue
    if !has_extras {
        return Ok(ToolOutput::SingleIssue(Box::new(issue)));
    }

    Ok(ToolOutput::Text(
        serde_json::to_string_pretty(&result).unwrap_or_default(),
    ))
}

async fn execute_get_issue_comments(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: KeyParam = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("missing 'key' parameter: {e}")))?;
    let result = provider.get_comments(&params.key).await?;
    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::Comments(result.items, Some(meta)))
}

async fn execute_get_issue_relations(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: KeyParam = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("missing 'key' parameter: {e}")))?;
    let relations = provider.get_issue_relations(&params.key).await?;
    Ok(ToolOutput::Relations(Box::new(relations)))
}

#[derive(Deserialize)]
struct CreateIssueParams {
    title: String,
    description: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    assignees: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    priority: Option<String>,
    parent: Option<String>,
    markdown: Option<bool>,
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
        parent: params.parent,
        markdown: params.markdown.unwrap_or(true),
    };
    let issue = provider.create_issue(input).await?;

    // Set custom fields injected by enricher (e.g., cf_goals → customFields)
    if let Some(cf) = args.get("customFields").and_then(|v| v.as_array())
        && !cf.is_empty()
        && let Err(e) = provider.set_custom_fields(&issue.key, cf).await
    {
        tracing::warn!(error = %e, "Failed to set custom fields on created issue");
    }

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
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    priority: Option<String>,
    #[serde(rename = "parentId")]
    parent_id: Option<String>,
    markdown: Option<bool>,
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
        parent_id: params.parent_id,
        markdown: params.markdown.unwrap_or(true),
    };
    let issue = provider.update_issue(&params.key, input).await?;

    // Set custom fields injected by enricher (e.g., cf_goals → customFields)
    if let Some(cf) = args.get("customFields").and_then(|v| v.as_array())
        && !cf.is_empty()
        && let Err(e) = provider.set_custom_fields(&params.key, cf).await
    {
        tracing::warn!(error = %e, "Failed to set custom fields on updated issue");
    }
    Ok(ToolOutput::SingleIssue(Box::new(issue)))
}

#[derive(Deserialize)]
struct AddCommentParams {
    key: String,
    body: String,
    #[serde(default)]
    attachments: Vec<AttachmentParam>,
}

#[derive(Deserialize)]
struct AttachmentParam {
    /// Base64-encoded file content
    #[serde(rename = "fileData")]
    file_data: String,
    /// Filename (e.g., "screenshot.png")
    filename: String,
}

async fn execute_add_issue_comment(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: AddCommentParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid add_issue_comment params: {e}")))?;

    let mut body = params.body.clone();
    let mut uploaded = 0;
    let mut upload_errors = Vec::new();

    // Validate attachment limits
    const MAX_ATTACHMENTS: usize = 10;

    if params.attachments.len() > MAX_ATTACHMENTS {
        return Err(Error::InvalidData(format!(
            "Too many attachments: {} (max {})",
            params.attachments.len(),
            MAX_ATTACHMENTS
        )));
    }

    // Upload attachments and append links to comment body
    for att in &params.attachments {
        use base64::Engine;
        let data = match base64::engine::general_purpose::STANDARD.decode(&att.file_data) {
            Ok(d) => d,
            Err(e) => {
                upload_errors.push(format!("{}: decode error: {}", att.filename, e));
                continue;
            }
        };

        if data.len() > MAX_FILE_SIZE {
            upload_errors.push(format!(
                "{}: file too large ({} bytes, max {})",
                att.filename,
                data.len(),
                MAX_FILE_SIZE
            ));
            continue;
        }

        match provider
            .upload_attachment(&params.key, &att.filename, &data)
            .await
        {
            Ok(url) => {
                if !url.is_empty() {
                    body.push_str(&format!("\n\n[{}]({})", att.filename, url));
                }
                uploaded += 1;
            }
            Err(e) => {
                upload_errors.push(format!("{}: {}", att.filename, e));
            }
        }
    }

    let comment = devboy_core::IssueProvider::add_comment(provider, &params.key, &body).await?;

    let mut msg = format!("Comment added to {} (id: {})", params.key, comment.id);
    if uploaded > 0 {
        msg.push_str(&format!(", {} attachment(s) uploaded", uploaded));
    }
    if !upload_errors.is_empty() {
        msg.push_str(&format!(
            ", {} attachment error(s): {}",
            upload_errors.len(),
            upload_errors.join("; ")
        ));
    }
    Ok(ToolOutput::Text(msg))
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
    offset: Option<u32>,
    sort_by: Option<String>,
    sort_order: Option<String>,
    /// Token budget for response size control (consumed by format layer via execute_and_format).
    #[allow(dead_code)]
    budget: Option<usize>,
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
        offset: params.offset,
        sort_by: params.sort_by,
        sort_order: params.sort_order,
    };
    let result = provider.get_merge_requests(filter).await?;
    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::MergeRequests(result.items, Some(meta)))
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
    let result = provider.get_discussions(&params.key).await?;
    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::Discussions(result.items, Some(meta)))
}

async fn execute_get_merge_request_diffs(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: KeyParam = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("missing 'key' parameter: {e}")))?;
    let result = provider.get_diffs(&params.key).await?;
    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::Diffs(result.items, Some(meta)))
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

// --- Pipeline tool handlers ---

#[derive(Deserialize, Default)]
struct GetPipelineParams {
    branch: Option<String>,
    #[serde(rename = "mrKey")]
    mr_key: Option<String>,
    #[serde(rename = "includeFailedLogs")]
    include_failed_logs: Option<bool>,
}

async fn execute_get_pipeline(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetPipelineParams = serde_json::from_value(args.clone()).unwrap_or_default();
    let input = GetPipelineInput {
        branch: params.branch,
        mr_key: params.mr_key,
        include_failed_logs: params.include_failed_logs.unwrap_or(true),
    };
    let pipeline = PipelineProvider::get_pipeline(provider, input).await?;
    Ok(ToolOutput::Pipeline(Box::new(pipeline)))
}

#[derive(Deserialize)]
struct GetJobLogsParams {
    #[serde(rename = "jobId")]
    job_id: String,
    pattern: Option<String>,
    context: Option<usize>,
    #[serde(rename = "maxMatches")]
    max_matches: Option<usize>,
    offset: Option<usize>,
    limit: Option<usize>,
    full: Option<bool>,
}

async fn execute_get_job_logs(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetJobLogsParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid get_job_logs params: {e}")))?;

    // Clamp limit to max 1000 as declared in schema
    let clamped_limit = params.limit.map(|l| l.min(1000));

    let mode = if let Some(pattern) = params.pattern {
        JobLogMode::Search {
            pattern,
            context: params.context.unwrap_or(5).min(50),
            max_matches: params.max_matches.unwrap_or(20).min(100),
        }
    } else if let Some(true) = params.full {
        JobLogMode::Full {
            max_lines: clamped_limit.unwrap_or(1000),
        }
    } else if params.offset.is_some() || clamped_limit.is_some() {
        JobLogMode::Paginated {
            offset: params.offset.unwrap_or(0),
            limit: clamped_limit.unwrap_or(200),
        }
    } else {
        JobLogMode::Smart
    };

    let options = JobLogOptions { mode };
    let log_output = PipelineProvider::get_job_logs(provider, &params.job_id, options).await?;
    Ok(ToolOutput::JobLog(Box::new(log_output)))
}

// --- Status / User / Link tool handlers ---

async fn execute_get_available_statuses(
    provider: &dyn devboy_core::Provider,
) -> Result<ToolOutput> {
    let result = IssueProvider::get_statuses(provider).await?;
    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::Statuses(result.items, Some(meta)))
}

#[derive(Deserialize, Default)]
struct GetUsersParams {
    user_id: Option<String>,
    project_key: Option<String>,
    search: Option<String>,
    include_inactive: Option<bool>,
    start_at: Option<u32>,
    max_results: Option<u32>,
}

async fn execute_get_users(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetUsersParams = serde_json::from_value(args.clone()).unwrap_or_default();
    let options = GetUsersOptions {
        user_id: params.user_id,
        project_key: params.project_key,
        search: params.search,
        include_inactive: params.include_inactive,
        start_at: params.start_at,
        max_results: params.max_results,
    };
    let result = IssueProvider::get_users(provider, options).await?;
    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::Users(result.items, Some(meta)))
}

#[derive(Deserialize)]
struct LinkIssuesParams {
    #[serde(alias = "sourceIssueKey", alias = "issueKey1")]
    source_key: String,
    #[serde(alias = "targetIssueKey", alias = "issueKey2")]
    target_key: String,
    #[serde(alias = "linkType")]
    link_type: String,
}

async fn execute_link_issues(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: LinkIssuesParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid link_issues params: {e}")))?;
    IssueProvider::link_issues(
        provider,
        &params.source_key,
        &params.target_key,
        &params.link_type,
    )
    .await?;
    Ok(ToolOutput::Text(format!(
        "Linked {} -> {} (type: {})",
        params.source_key, params.target_key, params.link_type
    )))
}

async fn execute_unlink_issues(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: LinkIssuesParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid unlink_issues params: {e}")))?;
    IssueProvider::unlink_issues(
        provider,
        &params.source_key,
        &params.target_key,
        &params.link_type,
    )
    .await?;
    Ok(ToolOutput::Text(format!(
        "Unlinked {} -> {} (type: {})",
        params.source_key, params.target_key, params.link_type
    )))
}

// --- Epic tool handlers ---

#[derive(Deserialize, Default)]
struct GetEpicsParams {
    state: Option<String>,
    search: Option<String>,
    assignee: Option<String>,
    #[serde(rename = "goalId")]
    goal_id: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

/// Extract goal ID (G1-G9) from issue labels/tags.
fn extract_goal_id(labels: &[String]) -> Option<String> {
    labels.iter().find_map(|l| {
        let lower = l.to_lowercase();
        if lower.len() == 2
            && lower.starts_with('g')
            && lower.chars().nth(1).is_some_and(|c| c.is_ascii_digit())
        {
            Some(lower.to_uppercase())
        } else {
            None
        }
    })
}

/// Calculate epic progress from subtasks.
fn epic_progress(subtasks: &[devboy_core::Issue]) -> serde_json::Value {
    let total = subtasks.len();
    let completed = subtasks.iter().filter(|s| s.state == "closed").count();
    let percentage = if total > 0 {
        (completed as f64 / total as f64 * 100.0).round() as u32
    } else {
        0
    };
    serde_json::json!({
        "total_subtasks": total,
        "completed_subtasks": completed,
        "percentage": percentage,
    })
}

async fn execute_get_epics(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetEpicsParams = serde_json::from_value(args.clone()).unwrap_or_default();
    let filter = IssueFilter {
        state: params.state,
        state_category: None,
        search: params.search,
        labels: Some(vec!["epic".to_string()]),
        labels_operator: None,
        assignee: params.assignee,
        limit: params.limit.or(Some(50)),
        offset: params.offset,
        sort_by: None,
        sort_order: None,
    };
    let result = provider.get_issues(filter).await?;
    let mut epics = result.items;

    // Filter by goalId if provided
    if let Some(ref goal) = params.goal_id {
        let goal_lower = goal.to_lowercase();
        epics.retain(|e| e.labels.iter().any(|l| l.to_lowercase() == goal_lower));
    }

    // Enrich each epic with goal ID and progress
    let enriched: Vec<serde_json::Value> = epics
        .iter()
        .map(|epic| {
            let mut v = serde_json::to_value(epic).unwrap_or_default();
            v["goal_id"] = serde_json::json!(extract_goal_id(&epic.labels));
            v["progress"] = epic_progress(&epic.subtasks);
            v
        })
        .collect();

    Ok(ToolOutput::Text(
        serde_json::to_string_pretty(&enriched).unwrap_or_default(),
    ))
}

#[derive(Deserialize)]
struct CreateEpicParams {
    title: String,
    description: Option<String>,
    #[serde(rename = "goalId")]
    goal_id: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    assignees: Vec<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    priority: Option<String>,
    markdown: Option<bool>,
}

async fn execute_create_epic(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: CreateEpicParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid create_epic params: {e}")))?;

    // Ensure "epic" label is included
    let mut labels = params.labels;
    if !labels.iter().any(|l| l.eq_ignore_ascii_case("epic")) {
        labels.push("epic".to_string());
    }

    // Add goal tag if goalId provided (e.g., "G1" → tag "g1")
    if let Some(ref goal) = params.goal_id {
        let goal_tag = goal.to_lowercase();
        if !labels.iter().any(|l| l.to_lowercase() == goal_tag) {
            labels.push(goal_tag);
        }
    }

    let input = CreateIssueInput {
        title: params.title,
        description: params.description,
        labels,
        assignees: params.assignees,
        priority: params.priority,
        parent: None,
        markdown: params.markdown.unwrap_or(true),
    };
    let issue = provider.create_issue(input).await?;

    // Set custom fields (e.g., Goals) injected by enricher via goalId → cf_goals → customFields
    if let Some(cf) = args.get("customFields").and_then(|v| v.as_array())
        && !cf.is_empty()
        && let Err(e) = provider.set_custom_fields(&issue.key, cf).await
    {
        tracing::warn!(error = %e, "Failed to set custom fields on created epic");
    }

    Ok(ToolOutput::SingleIssue(Box::new(issue)))
}

#[derive(Deserialize)]
struct UpdateEpicParams {
    #[serde(alias = "epicKey")]
    key: String,
    title: Option<String>,
    description: Option<String>,
    state: Option<String>,
    #[serde(rename = "goalId")]
    goal_id: Option<String>,
    labels: Option<Vec<String>>,
    assignees: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    priority: Option<String>,
    markdown: Option<bool>,
}

async fn execute_update_epic(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: UpdateEpicParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid update_epic params: {e}")))?;

    // Handle goal tag transition: if goalId is changing, update labels
    let labels = if let Some(ref new_goal) = params.goal_id {
        // Fetch current issue to get existing labels
        let current = provider.get_issue(&params.key).await?;
        let mut labels: Vec<String> = current
            .labels
            .iter()
            // Remove old goal tags (g1-g9)
            .filter(|l| {
                let lower = l.to_lowercase();
                !(lower.len() == 2
                    && lower.starts_with('g')
                    && lower.chars().nth(1).is_some_and(|c| c.is_ascii_digit()))
            })
            .cloned()
            .collect();

        // Add new goal tag
        let goal_tag = new_goal.to_lowercase();
        if !labels.iter().any(|l| l.to_lowercase() == goal_tag) {
            labels.push(goal_tag);
        }

        // Merge with explicitly provided labels
        if let Some(extra) = params.labels {
            for l in extra {
                if !labels
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(&l))
                {
                    labels.push(l);
                }
            }
        }
        Some(labels)
    } else {
        params.labels
    };

    let input = UpdateIssueInput {
        title: params.title,
        description: params.description,
        state: params.state,
        labels,
        assignees: params.assignees,
        priority: params.priority,
        parent_id: None,
        markdown: params.markdown.unwrap_or(true),
    };
    let issue = provider.update_issue(&params.key, input).await?;

    // Set custom fields (e.g., Goals) injected by enricher via goalId → cf_goals → customFields
    if let Some(cf) = args.get("customFields").and_then(|v| v.as_array())
        && !cf.is_empty()
        && let Err(e) = provider.set_custom_fields(&params.key, cf).await
    {
        tracing::warn!(error = %e, "Failed to set custom fields on updated epic");
    }

    Ok(ToolOutput::SingleIssue(Box::new(issue)))
}

/// List of all tool names supported by the executor.
pub const SUPPORTED_TOOLS: &[&str] = &[
    "get_issues",
    "get_issue",
    "get_issue_comments",
    "get_issue_relations",
    "create_issue",
    "update_issue",
    "add_issue_comment",
    "get_merge_requests",
    "get_merge_request",
    "get_merge_request_discussions",
    "get_merge_request_diffs",
    "create_merge_request",
    "create_merge_request_comment",
    "update_merge_request",
    "get_pipeline",
    "get_job_logs",
    "get_available_statuses",
    "get_users",
    "link_issues",
    "unlink_issues",
    "get_epics",
    "create_epic",
    "update_epic",
    "get_meeting_notes",
    "get_meeting_transcript",
    "search_meeting_notes",
    // Asset tools
    "get_assets",
    "upload_asset",
    "download_asset",
    "delete_asset",
];

// =============================================================================
// Update Merge Request handler
// =============================================================================

#[derive(Deserialize)]
struct UpdateMergeRequestParams {
    key: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    labels: Option<Vec<String>>,
    #[serde(default)]
    draft: Option<bool>,
}

async fn execute_update_merge_request(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: UpdateMergeRequestParams = serde_json::from_value(args.clone())?;
    debug!(key = %params.key, "update_merge_request");

    let input = devboy_core::UpdateMergeRequestInput {
        title: params.title,
        description: params.description,
        state: params.state,
        labels: params.labels,
        draft: params.draft,
    };

    let mr = MergeRequestProvider::update_merge_request(provider, &params.key, input).await?;
    Ok(ToolOutput::SingleMergeRequest(Box::new(mr)))
}

// =============================================================================
// Asset tool handlers
// =============================================================================

#[derive(Deserialize)]
struct GetAssetsParams {
    /// "issue" or "mr"
    context_type: String,
    /// Issue key (e.g. "DEV-123") or MR key (e.g. "mr#42")
    key: String,
}

async fn execute_get_assets(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetAssetsParams = serde_json::from_value(args.clone())?;
    debug!(context_type = %params.context_type, key = %params.key, "get_assets");

    let assets = match params.context_type.as_str() {
        "issue" => IssueProvider::get_issue_attachments(provider, &params.key).await?,
        "mr" | "merge_request" | "pull_request" => {
            MergeRequestProvider::get_mr_attachments(provider, &params.key).await?
        }
        other => {
            return Err(Error::InvalidData(format!(
                "unsupported context_type: '{other}', expected 'issue' or 'mr'"
            )));
        }
    };

    let capabilities = IssueProvider::asset_capabilities(provider);
    let output = serde_json::json!({
        "attachments": assets,
        "count": assets.len(),
        "capabilities": capabilities,
    });
    Ok(ToolOutput::Text(serde_json::to_string_pretty(&output)?))
}

#[derive(Deserialize)]
struct UploadAssetParams {
    /// "issue" or "mr"
    context_type: String,
    /// Issue key or MR key
    key: String,
    /// Original filename
    filename: String,
    /// Base64-encoded file data
    #[serde(rename = "fileData")]
    file_data: String,
}

async fn execute_upload_asset(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: UploadAssetParams = serde_json::from_value(args.clone())?;
    debug!(context_type = %params.context_type, key = %params.key, filename = %params.filename, "upload_asset");

    let data = base64_decode(&params.file_data)?;

    if data.len() > MAX_FILE_SIZE {
        return Err(Error::InvalidData(format!(
            "file '{}' is {} bytes, max allowed is {} bytes",
            params.filename,
            data.len(),
            MAX_FILE_SIZE,
        )));
    }

    let url = match params.context_type.as_str() {
        "issue" => {
            IssueProvider::upload_attachment(provider, &params.key, &params.filename, &data).await?
        }
        other => {
            return Err(Error::InvalidData(format!(
                "upload not supported for context_type: '{other}', use 'issue'"
            )));
        }
    };

    let output = serde_json::json!({
        "success": true,
        "url": url,
        "filename": params.filename,
        "size": data.len(),
    });
    Ok(ToolOutput::Text(serde_json::to_string_pretty(&output)?))
}

#[derive(Deserialize)]
struct DownloadAssetParams {
    /// "issue" or "mr"
    context_type: String,
    /// Issue key or MR key
    key: String,
    /// Asset identifier (provider-specific)
    asset_id: String,
}

async fn execute_download_asset(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: DownloadAssetParams = serde_json::from_value(args.clone())?;
    debug!(context_type = %params.context_type, key = %params.key, asset_id = %params.asset_id, "download_asset");

    let bytes = match params.context_type.as_str() {
        "issue" => {
            IssueProvider::download_attachment(provider, &params.key, &params.asset_id).await?
        }
        "mr" | "merge_request" | "pull_request" => {
            MergeRequestProvider::download_mr_attachment(provider, &params.key, &params.asset_id)
                .await?
        }
        other => {
            return Err(Error::InvalidData(format!(
                "unsupported context_type: '{other}', expected 'issue' or 'mr'"
            )));
        }
    };

    if bytes.len() > MAX_FILE_SIZE {
        return Err(Error::InvalidData(format!(
            "downloaded attachment is {} bytes, max allowed for base64 response is {} bytes",
            bytes.len(),
            MAX_FILE_SIZE,
        )));
    }

    let encoded = base64_encode(&bytes);
    let output = serde_json::json!({
        "success": true,
        "asset_id": params.asset_id,
        "size": bytes.len(),
        "data": encoded,
    });
    Ok(ToolOutput::Text(serde_json::to_string_pretty(&output)?))
}

#[derive(Deserialize)]
struct DeleteAssetParams {
    /// Issue key
    key: String,
    /// Asset identifier
    asset_id: String,
}

async fn execute_delete_asset(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: DeleteAssetParams = serde_json::from_value(args.clone())?;
    debug!(key = %params.key, asset_id = %params.asset_id, "delete_asset");

    IssueProvider::delete_attachment(provider, &params.key, &params.asset_id).await?;

    let output = serde_json::json!({
        "success": true,
        "asset_id": params.asset_id,
        "message": format!("Attachment '{}' deleted from {}", params.asset_id, params.key),
    });
    Ok(ToolOutput::Text(serde_json::to_string_pretty(&output)?))
}

/// Maximum base64 encoded length for MAX_FILE_SIZE bytes.
const MAX_BASE64_LEN: usize = (MAX_FILE_SIZE / 3 + 1) * 4 + 4;

/// Decode base64 with standard or URL-safe alphabet, rejecting
/// oversized inputs *before* allocating the decoded buffer.
fn base64_decode(input: &str) -> Result<Vec<u8>> {
    let trimmed = input.trim();
    if trimmed.len() > MAX_BASE64_LEN {
        return Err(Error::InvalidData(format!(
            "base64 input too large ({} chars), max decoded size is {} bytes",
            trimmed.len(),
            MAX_FILE_SIZE,
        )));
    }
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(trimmed)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(trimmed))
        .map_err(|e| Error::InvalidData(format!("invalid base64: {e}")))
}

/// Encode bytes as standard base64.
fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use devboy_core::{
        Comment, CreateMergeRequestInput, Discussion, FileDiff, Issue, IssueLink, IssueProvider,
        IssueRelations, MergeRequest, MergeRequestProvider, Provider, User,
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
            attachments_count: None,
            parent: None,
            subtasks: vec![],
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
        async fn get_issues(
            &self,
            _filter: IssueFilter,
        ) -> devboy_core::Result<devboy_core::ProviderResult<Issue>> {
            Ok(vec![sample_issue()].into())
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
        async fn get_comments(
            &self,
            _key: &str,
        ) -> devboy_core::Result<devboy_core::ProviderResult<Comment>> {
            Ok(vec![sample_comment()].into())
        }
        async fn add_comment(&self, _key: &str, _body: &str) -> devboy_core::Result<Comment> {
            Ok(sample_comment())
        }
        async fn get_issue_relations(&self, _key: &str) -> devboy_core::Result<IssueRelations> {
            Ok(IssueRelations {
                parent: Some(sample_issue()),
                subtasks: vec![sample_issue()],
                blocks: vec![IssueLink {
                    issue: sample_issue(),
                    link_type: "Blocks".into(),
                }],
                ..Default::default()
            })
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
        ) -> devboy_core::Result<devboy_core::ProviderResult<MergeRequest>> {
            Ok(vec![sample_mr()].into())
        }
        async fn get_merge_request(&self, _key: &str) -> devboy_core::Result<MergeRequest> {
            Ok(sample_mr())
        }
        async fn get_discussions(
            &self,
            _key: &str,
        ) -> devboy_core::Result<devboy_core::ProviderResult<Discussion>> {
            Ok(vec![sample_discussion()].into())
        }
        async fn get_diffs(
            &self,
            _key: &str,
        ) -> devboy_core::Result<devboy_core::ProviderResult<FileDiff>> {
            Ok(vec![sample_diff()].into())
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
    impl devboy_core::PipelineProvider for MockProvider {
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
        assert!(SUPPORTED_TOOLS.contains(&"get_meeting_notes"));
        assert!(SUPPORTED_TOOLS.contains(&"get_meeting_transcript"));
        assert!(SUPPORTED_TOOLS.contains(&"search_meeting_notes"));
        assert_eq!(SUPPORTED_TOOLS.len(), 30);
    }

    // --- Issue tool dispatch tests ---

    #[tokio::test]
    async fn test_dispatch_get_issues() {
        let provider = MockProvider;
        let args = serde_json::json!({"state": "open", "limit": 10});
        let result = dispatch_tool("get_issues", &args, &provider).await.unwrap();
        assert!(matches!(result, ToolOutput::Issues(v, _) if v.len() == 1));
    }

    #[tokio::test]
    async fn test_dispatch_get_issues_empty_args() {
        let provider = MockProvider;
        let result = dispatch_tool("get_issues", &Value::Null, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Issues(_, _)));
    }

    #[tokio::test]
    async fn test_dispatch_get_issue() {
        let provider = MockProvider;
        // With includeComments/includeRelations defaulting to true, returns composite Text
        let args = serde_json::json!({"key": "gh#1"});
        let result = dispatch_tool("get_issue", &args, &provider).await.unwrap();
        assert!(matches!(result, ToolOutput::Text(_)));

        // Without extras, returns SingleIssue
        let args =
            serde_json::json!({"key": "gh#1", "includeComments": false, "includeRelations": false});
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
        assert!(matches!(result, ToolOutput::Comments(v, _) if v.len() == 1));
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

    #[tokio::test]
    async fn test_dispatch_get_issue_relations() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "gh#1"});
        let result = dispatch_tool("get_issue_relations", &args, &provider)
            .await
            .unwrap();
        match result {
            ToolOutput::Relations(relations) => {
                assert!(relations.parent.is_some());
                assert_eq!(relations.subtasks.len(), 1);
                assert_eq!(relations.blocks.len(), 1);
            }
            other => panic!("Expected Relations, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_dispatch_get_issue_relations_missing_key() {
        let provider = MockProvider;
        let result = dispatch_tool("get_issue_relations", &serde_json::json!({}), &provider).await;
        assert!(result.is_err());
    }

    // --- MR tool dispatch tests ---

    #[tokio::test]
    async fn test_dispatch_get_merge_requests() {
        let provider = MockProvider;
        let args = serde_json::json!({"state": "open", "limit": 5});
        let result = dispatch_tool("get_merge_requests", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::MergeRequests(v, _) if v.len() == 1));
    }

    #[tokio::test]
    async fn test_dispatch_get_merge_requests_empty_args() {
        let provider = MockProvider;
        let result = dispatch_tool("get_merge_requests", &Value::Null, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::MergeRequests(_, _)));
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
        assert!(matches!(result, ToolOutput::Discussions(v, _) if v.len() == 1));
    }

    #[tokio::test]
    async fn test_dispatch_get_merge_request_diffs() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "pr#1"});
        let result = dispatch_tool("get_merge_request_diffs", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Diffs(v, _) if v.len() == 1));
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
            fn supported_categories(&self) -> &[devboy_core::ToolCategory] {
                &[devboy_core::ToolCategory::IssueTracker]
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
        assert_eq!(executor.enrichers.len(), 1);
    }

    // --- Pipeline dispatch tests ---

    #[tokio::test]
    async fn test_dispatch_get_pipeline_unsupported() {
        let provider = MockProvider;
        let args = serde_json::json!({"branch": "main"});
        let result = dispatch_tool("get_pipeline", &args, &provider).await;
        // MockProvider doesn't implement get_pipeline → ProviderUnsupported
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_get_job_logs_unsupported() {
        let provider = MockProvider;
        let args = serde_json::json!({"jobId": "123"});
        let result = dispatch_tool("get_job_logs", &args, &provider).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_get_pipeline_with_mr_key() {
        let provider = MockProvider;
        let args = serde_json::json!({"mrKey": "pr#1", "includeFailedLogs": false});
        let result = dispatch_tool("get_pipeline", &args, &provider).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_get_job_logs_with_pattern() {
        let provider = MockProvider;
        let args = serde_json::json!({"jobId": "123", "pattern": "ERROR", "context": 3});
        let result = dispatch_tool("get_job_logs", &args, &provider).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_get_job_logs_paginated() {
        let provider = MockProvider;
        let args = serde_json::json!({"jobId": "123", "offset": 10, "limit": 50});
        let result = dispatch_tool("get_job_logs", &args, &provider).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_get_job_logs_full() {
        let provider = MockProvider;
        let args = serde_json::json!({"jobId": "123", "full": true});
        let result = dispatch_tool("get_job_logs", &args, &provider).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_executor_default() {
        let executor = Executor::default();
        assert!(executor.enrichers.is_empty());
    }

    // --- Status / User / Link / Epic dispatch tests ---

    #[tokio::test]
    async fn test_dispatch_get_available_statuses_unsupported() {
        let provider = MockProvider;
        let result = dispatch_tool("get_available_statuses", &Value::Null, &provider).await;
        // MockProvider returns ProviderUnsupported for get_statuses
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_get_users_unsupported() {
        let provider = MockProvider;
        let args = serde_json::json!({"search": "test"});
        let result = dispatch_tool("get_users", &args, &provider).await;
        // MockProvider uses default impl which returns ProviderUnsupported
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_link_issues_unsupported() {
        let provider = MockProvider;
        let args = serde_json::json!({
            "source_key": "gh#1",
            "target_key": "gh#2",
            "link_type": "blocks"
        });
        let result = dispatch_tool("link_issues", &args, &provider).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_get_epics() {
        let provider = MockProvider;
        let args = serde_json::json!({"state": "open", "limit": 10});
        let result = dispatch_tool("get_epics", &args, &provider).await.unwrap();
        // Returns enriched JSON with goal_id and progress
        assert!(matches!(result, ToolOutput::Text(_)));
    }

    #[tokio::test]
    async fn test_dispatch_get_epics_empty_args() {
        let provider = MockProvider;
        let result = dispatch_tool("get_epics", &Value::Null, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Text(_)));
    }

    #[tokio::test]
    async fn test_dispatch_create_epic() {
        let provider = MockProvider;
        let args = serde_json::json!({"title": "New Epic", "description": "Epic description"});
        let result = dispatch_tool("create_epic", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::SingleIssue(_)));
    }

    #[tokio::test]
    async fn test_dispatch_update_epic() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "gh#1", "title": "Updated Epic"});
        let result = dispatch_tool("update_epic", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::SingleIssue(_)));
    }

    #[tokio::test]
    async fn test_dispatch_link_issues_missing_params() {
        let provider = MockProvider;
        let args = serde_json::json!({"source_key": "gh#1"});
        let result = dispatch_tool("link_issues", &args, &provider).await;
        assert!(result.is_err());
    }

    // --- Mock MeetingNotesProvider tests ---

    struct MockMeetingProvider;

    #[async_trait]
    impl MeetingNotesProvider for MockMeetingProvider {
        fn provider_name(&self) -> &'static str {
            "mock_meetings"
        }

        async fn get_meetings(
            &self,
            _filter: MeetingFilter,
        ) -> devboy_core::Result<devboy_core::ProviderResult<devboy_core::MeetingNote>> {
            Ok(vec![devboy_core::MeetingNote {
                id: "m1".into(),
                title: "Test Meeting".into(),
                ..Default::default()
            }]
            .into())
        }

        async fn get_transcript(
            &self,
            meeting_id: &str,
        ) -> devboy_core::Result<devboy_core::MeetingTranscript> {
            Ok(devboy_core::MeetingTranscript {
                meeting_id: meeting_id.to_string(),
                title: Some("Test Transcript".into()),
                sentences: vec![devboy_core::TranscriptSentence {
                    speaker_id: "s1".into(),
                    speaker_name: Some("Alice".into()),
                    text: "Hello".into(),
                    start_time: 0.0,
                    end_time: 1.0,
                }],
            })
        }

        async fn search_meetings(
            &self,
            _query: &str,
            _filter: MeetingFilter,
        ) -> devboy_core::Result<devboy_core::ProviderResult<devboy_core::MeetingNote>> {
            Ok(vec![devboy_core::MeetingNote {
                id: "m2".into(),
                title: "Search Result Meeting".into(),
                ..Default::default()
            }]
            .into())
        }
    }

    #[tokio::test]
    async fn test_dispatch_get_meeting_notes() {
        let provider = MockMeetingProvider;
        let args = serde_json::json!({"from_date": "2025-01-01", "limit": 10});
        let result = dispatch_meeting_tool("get_meeting_notes", &args, &provider)
            .await
            .unwrap();
        match result {
            ToolOutput::MeetingNotes(meetings, _) => {
                assert_eq!(meetings.len(), 1);
                assert_eq!(meetings[0].title, "Test Meeting");
            }
            other => panic!("Expected MeetingNotes, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_dispatch_get_meeting_transcript() {
        let provider = MockMeetingProvider;
        let args = serde_json::json!({"meeting_id": "m1"});
        let result = dispatch_meeting_tool("get_meeting_transcript", &args, &provider)
            .await
            .unwrap();
        match result {
            ToolOutput::MeetingTranscript(transcript) => {
                assert_eq!(transcript.meeting_id, "m1");
                assert_eq!(transcript.sentences.len(), 1);
                assert_eq!(transcript.sentences[0].speaker_name, Some("Alice".into()));
            }
            other => panic!("Expected MeetingTranscript, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_dispatch_search_meeting_notes() {
        let provider = MockMeetingProvider;
        let args = serde_json::json!({"query": "sprint", "limit": 5});
        let result = dispatch_meeting_tool("search_meeting_notes", &args, &provider)
            .await
            .unwrap();
        match result {
            ToolOutput::MeetingNotes(meetings, _) => {
                assert_eq!(meetings.len(), 1);
                assert_eq!(meetings[0].title, "Search Result Meeting");
            }
            other => panic!("Expected MeetingNotes, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_dispatch_unknown_meeting_tool() {
        let provider = MockMeetingProvider;
        let result = dispatch_meeting_tool("nonexistent_tool", &Value::Null, &provider).await;
        assert!(result.is_err());
    }
}
