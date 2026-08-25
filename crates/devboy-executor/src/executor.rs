use devboy_core::types::ChatType;
use devboy_core::{
    AddStructureRowsInput, AssignToSprintInput, CreateCommentInput, CreateIssueInput,
    CreateMergeRequestInput, CreatePageParams, CreateStructureInput, Error, GetChatsParams,
    GetForestOptions, GetMessagesParams, GetPipelineInput, GetStructureValuesInput,
    GetUsersOptions, IssueFilter, IssueProvider, JobLogMode, JobLogOptions, KnowledgeBaseProvider,
    ListCustomFieldsParams, ListPagesParams, ListProjectVersionsParams, MeetingFilter,
    MeetingNotesProvider, MergeRequestProvider, MessengerProvider, MoveStructureRowsInput,
    MrFilter, PipelineProvider, Result, SaveStructureViewInput, SearchKbParams,
    SearchMessagesParams, SendMessageParams, SprintState, StructureRowItem, StructureViewColumn,
    ToolCategory, UpdateIssueInput, UpdatePageParams, UpsertProjectVersionInput,
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

/// Parse `tools/call` args into a typed `Params` struct, turning
/// deserialisation failures into `Error::InvalidData` so callers see
/// the exact field that went wrong instead of the tool silently
/// running with defaults (the previous `unwrap_or_default()` path).
///
/// `Value::Null` is accepted as "no arguments" and yields `T::default()`
/// — the MCP spec allows a null `arguments` field for tools whose
/// params are all optional, so rejecting null here would break those
/// call sites. Actual content is validated by `serde_json::from_value`.
fn parse_tool_params<T>(args: &Value, tool: &str) -> Result<T>
where
    T: Default + serde::de::DeserializeOwned,
{
    if args.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid {tool} params: {e}")))
}

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
    asset_manager: Option<devboy_assets::AssetManager>,
}

impl Executor {
    pub fn new() -> Self {
        Self {
            enrichers: Vec::new(),
            asset_manager: None,
        }
    }

    /// Configure an optional local asset cache for download/delete operations.
    pub fn with_asset_manager(mut self, mgr: devboy_assets::AssetManager) -> Self {
        self.asset_manager = Some(mgr);
        self
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
        } else if tool_category == Some(ToolCategory::KnowledgeBase) {
            let provider =
                factory::create_knowledge_base_provider(&ctx.provider, ctx.proxy.as_ref())?;
            dispatch_knowledge_base_tool(tool, &args, provider.as_ref()).await?
        } else if tool_category == Some(ToolCategory::Messenger) {
            let provider = factory::create_messenger_provider(&ctx.provider)?;
            dispatch_messenger_tool(tool, &args, provider.as_ref()).await?
        } else {
            let provider = factory::create_provider(&ctx.provider, ctx.proxy.as_ref())?;
            dispatch_tool(tool, &args, provider.as_ref(), self.asset_manager.as_ref()).await?
        };

        Ok(output)
    }

    /// Execute a tool with a pre-created Provider (for MCP server).
    /// Enrichers are applied if configured.
    pub async fn execute_direct(
        &self,
        tool: &str,
        args: Value,
        provider: &dyn devboy_core::Provider,
    ) -> Result<ToolOutput> {
        let mut args = args;
        // Apply enricher transforms (same as execute())
        let tool_category = Self::tool_category(tool);
        for enricher in &self.enrichers {
            if let Some(cat) = tool_category
                && enricher.supported_categories().contains(&cat)
            {
                enricher.transform_args(tool, &mut args);
            }
        }
        dispatch_tool(tool, &args, provider, self.asset_manager.as_ref()).await
    }

    /// Execute a meeting tool with a pre-created MeetingNotesProvider.
    pub async fn execute_direct_meeting(
        &self,
        tool: &str,
        args: Value,
        provider: &dyn MeetingNotesProvider,
    ) -> Result<ToolOutput> {
        let mut args = args;
        let tool_category = Self::tool_category(tool);
        for enricher in &self.enrichers {
            if let Some(cat) = tool_category
                && enricher.supported_categories().contains(&cat)
            {
                enricher.transform_args(tool, &mut args);
            }
        }
        dispatch_meeting_tool(tool, &args, provider).await
    }

    /// Execute a knowledge base tool with a pre-created KnowledgeBaseProvider.
    pub async fn execute_direct_knowledge_base(
        &self,
        tool: &str,
        args: Value,
        provider: &dyn KnowledgeBaseProvider,
    ) -> Result<ToolOutput> {
        let mut args = args;
        let tool_category = Self::tool_category(tool);
        for enricher in &self.enrichers {
            if let Some(cat) = tool_category
                && enricher.supported_categories().contains(&cat)
            {
                enricher.transform_args(tool, &mut args);
            }
        }
        dispatch_knowledge_base_tool(tool, &args, provider).await
    }

    /// Execute a messenger tool with a pre-created MessengerProvider.
    pub async fn execute_direct_messenger(
        &self,
        tool: &str,
        args: Value,
        provider: &dyn MessengerProvider,
    ) -> Result<ToolOutput> {
        let mut args = args;
        let tool_category = Self::tool_category(tool);
        for enricher in &self.enrichers {
            if let Some(cat) = tool_category
                && enricher.supported_categories().contains(&cat)
            {
                enricher.transform_args(tool, &mut args);
            }
        }
        dispatch_messenger_tool(tool, &args, provider).await
    }

    /// Get the tool category for a tool name, if known.
    pub fn tool_category(tool: &str) -> Option<ToolCategory> {
        crate::tools::base_tool_definitions()
            .iter()
            .find(|t| t.name == tool)
            .map(|t| t.category)
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self::new()
    }
}

// --- Knowledge base tool dispatch ---

/// Dispatch a knowledge base tool call.
async fn dispatch_knowledge_base_tool(
    tool: &str,
    args: &Value,
    provider: &dyn KnowledgeBaseProvider,
) -> Result<ToolOutput> {
    match tool {
        "get_knowledge_base_spaces" => execute_get_knowledge_base_spaces(provider).await,
        "list_knowledge_base_pages" => execute_list_knowledge_base_pages(provider, args).await,
        "get_knowledge_base_page" => execute_get_knowledge_base_page(provider, args).await,
        "create_knowledge_base_page" => execute_create_knowledge_base_page(provider, args).await,
        "update_knowledge_base_page" => execute_update_knowledge_base_page(provider, args).await,
        "search_knowledge_base" => execute_search_knowledge_base(provider, args).await,
        _ => Err(Error::NotFound(format!(
            "unknown knowledge base tool: {tool}"
        ))),
    }
}

// --- Knowledge base tool handlers ---

async fn execute_get_knowledge_base_spaces(
    provider: &dyn KnowledgeBaseProvider,
) -> Result<ToolOutput> {
    let result = provider.get_spaces().await?;
    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::KnowledgeBaseSpaces(result.items, Some(meta)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListKnowledgeBasePagesParams {
    space_key: String,
    limit: Option<u32>,
    offset: Option<u32>,
    cursor: Option<String>,
    search: Option<String>,
    parent_id: Option<String>,
}

async fn execute_list_knowledge_base_pages(
    provider: &dyn KnowledgeBaseProvider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: ListKnowledgeBasePagesParams =
        serde_json::from_value(args.clone()).map_err(|e| {
            Error::InvalidData(format!("invalid list_knowledge_base_pages params: {e}"))
        })?;
    let result = provider
        .list_pages(ListPagesParams {
            space_key: params.space_key,
            limit: params.limit,
            offset: params.offset,
            cursor: params.cursor,
            search: params.search,
            parent_id: params.parent_id,
        })
        .await?;
    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::KnowledgeBasePages(result.items, Some(meta)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetKnowledgeBasePageParams {
    page_id: String,
}

async fn execute_get_knowledge_base_page(
    provider: &dyn KnowledgeBaseProvider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetKnowledgeBasePageParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid get_knowledge_base_page params: {e}")))?;
    let page = provider.get_page(&params.page_id).await?;
    Ok(ToolOutput::KnowledgeBasePage(Box::new(page)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateKnowledgeBasePageParams {
    space_key: String,
    title: String,
    content: String,
    #[serde(default)]
    content_type: Option<String>,
    parent_id: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
}

async fn execute_create_knowledge_base_page(
    provider: &dyn KnowledgeBaseProvider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: CreateKnowledgeBasePageParams =
        serde_json::from_value(args.clone()).map_err(|e| {
            Error::InvalidData(format!("invalid create_knowledge_base_page params: {e}"))
        })?;
    let page = provider
        .create_page(CreatePageParams {
            space_key: params.space_key,
            title: params.title,
            content: params.content,
            content_type: params.content_type,
            parent_id: params.parent_id,
            labels: params.labels,
        })
        .await?;
    Ok(ToolOutput::KnowledgeBasePageSummary(Box::new(page)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateKnowledgeBasePageParams {
    page_id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    content_type: Option<String>,
    version: Option<u32>,
    #[serde(default)]
    labels: Option<Vec<String>>,
    parent_id: Option<String>,
}

async fn execute_update_knowledge_base_page(
    provider: &dyn KnowledgeBaseProvider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: UpdateKnowledgeBasePageParams =
        serde_json::from_value(args.clone()).map_err(|e| {
            Error::InvalidData(format!("invalid update_knowledge_base_page params: {e}"))
        })?;
    let page = provider
        .update_page(UpdatePageParams {
            page_id: params.page_id,
            title: params.title,
            content: params.content,
            content_type: params.content_type,
            version: params.version,
            labels: params.labels,
            parent_id: params.parent_id,
        })
        .await?;
    Ok(ToolOutput::KnowledgeBasePageSummary(Box::new(page)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchKnowledgeBaseParams {
    query: String,
    space_key: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
    #[serde(default)]
    raw_query: bool,
}

async fn execute_search_knowledge_base(
    provider: &dyn KnowledgeBaseProvider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: SearchKnowledgeBaseParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid search_knowledge_base params: {e}")))?;
    let result = provider
        .search(SearchKbParams {
            query: params.query,
            space_key: params.space_key,
            cursor: params.cursor,
            limit: params.limit,
            raw_query: params.raw_query,
        })
        .await?;
    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::KnowledgeBasePages(result.items, Some(meta)))
}

// --- Messenger tool dispatch ---

/// Dispatch a messenger tool call.
async fn dispatch_messenger_tool(
    tool: &str,
    args: &Value,
    provider: &dyn MessengerProvider,
) -> Result<ToolOutput> {
    match tool {
        "get_messenger_chats" => execute_get_messenger_chats(provider, args).await,
        "get_chat_messages" => execute_get_chat_messages(provider, args).await,
        "search_chat_messages" => execute_search_chat_messages(provider, args).await,
        "send_message" => execute_send_message(provider, args).await,
        _ => Err(Error::NotFound(format!("unknown messenger tool: {tool}"))),
    }
}

// --- Messenger tool handlers ---

#[derive(Deserialize, Default)]
struct GetMessengerChatsParams {
    search: Option<String>,
    chat_type: Option<ChatType>,
    limit: Option<u32>,
    cursor: Option<String>,
    include_inactive: Option<bool>,
}

async fn execute_get_messenger_chats(
    provider: &dyn MessengerProvider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetMessengerChatsParams = parse_tool_params(args, "get_messenger_chats")?;
    let request = GetChatsParams {
        search: params.search,
        chat_type: params.chat_type,
        limit: params.limit,
        cursor: params.cursor,
        include_inactive: params.include_inactive,
    };
    let result = provider.get_chats(request).await?;
    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::MessengerChats(result.items, Some(meta)))
}

#[derive(Deserialize)]
struct GetChatMessagesParams {
    chat_id: String,
    limit: Option<u32>,
    cursor: Option<String>,
    thread_id: Option<String>,
    since: Option<String>,
    until: Option<String>,
}

async fn execute_get_chat_messages(
    provider: &dyn MessengerProvider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetChatMessagesParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("missing 'chat_id' parameter: {e}")))?;
    let request = GetMessagesParams {
        chat_id: params.chat_id,
        limit: params.limit,
        cursor: params.cursor,
        thread_id: params.thread_id,
        since: params.since,
        until: params.until,
    };
    let result = provider.get_messages(request).await?;
    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::MessengerMessages(result.items, Some(meta)))
}

#[derive(Deserialize)]
struct SearchChatMessagesParams {
    query: String,
    chat_id: Option<String>,
    limit: Option<u32>,
    cursor: Option<String>,
    since: Option<String>,
    until: Option<String>,
}

async fn execute_search_chat_messages(
    provider: &dyn MessengerProvider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: SearchChatMessagesParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("missing 'query' parameter: {e}")))?;
    let request = SearchMessagesParams {
        query: params.query,
        chat_id: params.chat_id,
        limit: params.limit,
        cursor: params.cursor,
        since: params.since,
        until: params.until,
    };
    let result = provider.search_messages(request).await?;
    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::MessengerMessages(result.items, Some(meta)))
}

#[derive(Deserialize)]
struct SendMessengerMessageParams {
    chat_id: String,
    text: String,
    thread_id: Option<String>,
    reply_to_id: Option<String>,
}

async fn execute_send_message(
    provider: &dyn MessengerProvider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: SendMessengerMessageParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid send_message params: {e}")))?;
    let request = SendMessageParams {
        chat_id: params.chat_id,
        text: params.text,
        thread_id: params.thread_id,
        reply_to_id: params.reply_to_id,
        attachments: vec![],
    };
    let message = provider.send_message(request).await?;
    Ok(ToolOutput::SingleMessage(Box::new(message)))
}

// --- Tool dispatch ---

/// Dispatch a tool call to the appropriate provider method.
async fn dispatch_tool(
    tool: &str,
    args: &Value,
    provider: &dyn devboy_core::Provider,
    asset_manager: Option<&devboy_assets::AssetManager>,
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
        "download_asset" => execute_download_asset(provider, args, asset_manager).await,
        "delete_asset" => execute_delete_asset(provider, args, asset_manager).await,

        // Jira Structure tools
        "get_structures" => execute_get_structures(provider).await,
        "get_structure_forest" => execute_get_structure_forest(provider, args).await,
        "add_structure_rows" => execute_add_structure_rows(provider, args).await,
        "move_structure_rows" => execute_move_structure_rows(provider, args).await,
        "remove_structure_row" => execute_remove_structure_row(provider, args).await,
        "get_structure_values" => execute_get_structure_values(provider, args).await,
        "get_structure_views" => execute_get_structure_views(provider, args).await,
        "save_structure_view" => execute_save_structure_view(provider, args).await,
        "create_structure" => execute_create_structure(provider, args).await,

        // Project versions / fixVersion (issue #238)
        "list_project_versions" => execute_list_project_versions(provider, args).await,
        "upsert_project_version" => execute_upsert_project_version(provider, args).await,

        // Agile / Sprint (issue #198)
        "get_board_sprints" => execute_get_board_sprints(provider, args).await,
        "assign_to_sprint" => execute_assign_to_sprint(provider, args).await,

        // Custom-field discovery
        "get_custom_fields" => execute_get_custom_fields(provider, args).await,

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
    let params: GetMeetingNotesParams = parse_tool_params(args, "get_meeting_notes")?;
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
    #[serde(rename = "projectKey")]
    project_key: Option<String>,
    #[serde(rename = "nativeQuery")]
    native_query: Option<String>,
    /// Token budget for response size control (consumed by format layer via execute_and_format).
    #[allow(dead_code)]
    budget: Option<usize>,
}

async fn execute_get_issues(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetIssuesParams = parse_tool_params(args, "get_issues")?;
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
        project_key: params.project_key,
        native_query: params.native_query,
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
    #[serde(alias = "parentId")]
    parent: Option<String>,
    markdown: Option<bool>,
    #[serde(rename = "projectId")]
    project_id: Option<String>,
    #[serde(rename = "issueType")]
    issue_type: Option<String>,
    /// Jira component names (issue #197). Serde rejects non-string-array
    /// input instead of silently dropping entries (Copilot review on PR #205).
    #[serde(default)]
    components: Vec<String>,
    /// Jira fix-version names. Same shape and semantics as `components`.
    #[serde(default, rename = "fixVersions")]
    fix_versions: Vec<String>,
    /// Jira parent epic key. Resolved via the field-id lookup so callers
    /// don't need to know the instance's `customfield_*` number.
    #[serde(default, rename = "epicKey")]
    epic_key: Option<String>,
    /// Jira sprint id. Numeric agile-board sprint id.
    #[serde(default, rename = "sprintId")]
    sprint_id: Option<i64>,
    /// Jira Epic Name. Required by Server/DC + Cloud company-managed
    /// when `issueType == "Epic"`.
    #[serde(default, rename = "epicName")]
    epic_name: Option<String>,
}

async fn execute_create_issue(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: CreateIssueParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid create_issue params: {e}")))?;
    let custom_fields = args.get("customFields").cloned();
    let input = CreateIssueInput {
        title: params.title,
        description: params.description,
        labels: params.labels,
        assignees: params.assignees,
        priority: params.priority,
        parent: params.parent,
        markdown: params.markdown.unwrap_or(true),
        project_id: params.project_id,
        issue_type: params.issue_type,
        custom_fields,
        components: params.components,
        fix_versions: params.fix_versions,
        epic_key: params.epic_key,
        sprint_id: params.sprint_id,
        epic_name: params.epic_name,
    };
    let issue = provider.create_issue(input).await?;

    // Set custom fields via separate API call (ClickUp uses Array format)
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
    /// Provider-specific status name (#288). For ClickUp: any custom
    /// status from `get_available_statuses` (e.g. "in progress",
    /// "review"). Currently a no-op for other providers.
    #[serde(default)]
    status: Option<String>,
    labels: Option<Vec<String>>,
    assignees: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    priority: Option<String>,
    #[serde(rename = "parentId")]
    parent_id: Option<String>,
    markdown: Option<bool>,
    /// Jira component names (issue #197). `None` (key absent) leaves
    /// components untouched; `Some([])` clears all; `Some([...])` replaces.
    /// Serde-parsed so non-array / non-string input errors fast.
    #[serde(default)]
    components: Option<Vec<String>>,
    /// Jira fix-version names. Same shape and semantics as `components`.
    #[serde(default, rename = "fixVersions")]
    fix_versions: Option<Vec<String>>,
    /// Jira parent epic key.
    #[serde(default, rename = "epicKey")]
    epic_key: Option<String>,
    /// Jira sprint id.
    #[serde(default, rename = "sprintId")]
    sprint_id: Option<i64>,
    /// Jira Epic Name (Epic-typed issues).
    #[serde(default, rename = "epicName")]
    epic_name: Option<String>,
}

async fn execute_update_issue(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: UpdateIssueParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid update_issue params: {e}")))?;
    let custom_fields = args.get("customFields").cloned();
    let input = UpdateIssueInput {
        title: params.title,
        description: params.description,
        state: params.state,
        status: params.status,
        labels: params.labels,
        assignees: params.assignees,
        priority: params.priority,
        parent_id: params.parent_id,
        markdown: params.markdown.unwrap_or(true),
        custom_fields,
        components: params.components,
        fix_versions: params.fix_versions,
        epic_key: params.epic_key,
        sprint_id: params.sprint_id,
        epic_name: params.epic_name,
    };
    let key = params.key;
    let issue = provider.update_issue(&key, input).await?;

    // Set custom fields via separate API call (ClickUp uses Array format)
    if let Some(cf) = args.get("customFields").and_then(|v| v.as_array())
        && !cf.is_empty()
        && let Err(e) = provider.set_custom_fields(&key, cf).await
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
    let params: GetMergeRequestsParams = parse_tool_params(args, "get_merge_requests")?;
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

/// Params for get_merge_request_discussions. The schema has always advertised
/// `limit`/`offset`, but they were deserialized into [`KeyParam`] and silently
/// dropped — advertised-but-ignored is worse than absent. `mrKey` alias
/// matches [`CreateMrCommentParams`]: agents routinely send the camelCase key.
#[derive(Deserialize)]
struct DiscussionsParams {
    #[serde(alias = "mrKey")]
    key: String,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
    /// Token budget for response size control (consumed by format layer via execute_and_format).
    #[serde(default)]
    #[allow(dead_code)]
    budget: Option<usize>,
}

async fn execute_get_merge_request_discussions(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: DiscussionsParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("missing 'key' parameter: {e}")))?;
    let result = provider.get_discussions(&params.key).await?;
    let provider_has_more = result.pagination.as_ref().is_some_and(|p| p.has_more);
    let total = result.items.len();

    let offset = params.offset.unwrap_or(0);
    let mut items = result.items;
    if offset > 0 || params.limit.is_some() {
        items = items
            .into_iter()
            .skip(offset)
            .take(params.limit.unwrap_or(usize::MAX))
            .collect();
    }
    // Slicing must be visible in the metadata: a page that looks complete but
    // isn't is exactly the failure mode this tool had.
    let pagination = if offset > 0 || params.limit.is_some() || provider_has_more {
        Some(devboy_core::Pagination {
            offset: offset as u32,
            limit: items.len() as u32,
            total: Some(total as u32),
            has_more: provider_has_more || offset + items.len() < total,
            next_cursor: None,
        })
    } else {
        None
    };
    let meta = ResultMeta {
        pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::Discussions(items, Some(meta)))
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
    #[serde(alias = "mrKey")]
    key: String,
    body: String,
    #[serde(alias = "filePath")]
    file_path: Option<String>,
    line: Option<u32>,
    #[serde(alias = "lineType")]
    line_type: Option<String>,
    #[serde(alias = "commitSha")]
    commit_sha: Option<String>,
    #[serde(alias = "discussionId")]
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
    let params: GetPipelineParams = parse_tool_params(args, "get_pipeline")?;
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
    let params: GetUsersParams = parse_tool_params(args, "get_users")?;
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
    let params: GetEpicsParams = parse_tool_params(args, "get_epics")?;
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
        project_key: None,
        native_query: None,
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
        project_id: None,
        issue_type: None,
        custom_fields: args.get("customFields").cloned(),
        components: Vec::new(),
        fix_versions: Vec::new(),
        epic_key: None,
        sprint_id: None,
        epic_name: None,
    };
    let issue = provider.create_issue(input).await?;

    // Set custom fields via separate API call (ClickUp uses Array format)
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
    /// Provider-specific status name (#288). Same semantics as
    /// `UpdateIssueParams::status` — epics are stored as ClickUp tasks,
    /// so the same custom-status workflow applies (e.g. "in progress",
    /// "review", "complete").
    #[serde(default)]
    status: Option<String>,
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
        status: params.status,
        labels,
        assignees: params.assignees,
        priority: params.priority,
        parent_id: None,
        markdown: params.markdown.unwrap_or(true),
        custom_fields: args.get("customFields").cloned(),
        components: None,
        fix_versions: None,
        epic_key: None,
        sprint_id: None,
        epic_name: None,
    };
    let key = params.key;
    let issue = provider.update_issue(&key, input).await?;

    // Set custom fields via separate API call (ClickUp uses Array format)
    if let Some(cf) = args.get("customFields").and_then(|v| v.as_array())
        && !cf.is_empty()
        && let Err(e) = provider.set_custom_fields(&key, cf).await
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
    // Knowledge base tools
    "get_knowledge_base_spaces",
    "list_knowledge_base_pages",
    "get_knowledge_base_page",
    "create_knowledge_base_page",
    "update_knowledge_base_page",
    "search_knowledge_base",
    // Messenger tools
    "get_messenger_chats",
    "get_chat_messages",
    "search_chat_messages",
    "send_message",
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

    let capabilities =
        serde_json::to_value(IssueProvider::asset_capabilities(provider)).unwrap_or_default();
    let count = assets.len();
    let attachments: Vec<serde_json::Value> = assets
        .into_iter()
        .map(|a| serde_json::to_value(a).unwrap_or_default())
        .collect();
    Ok(ToolOutput::AssetList {
        attachments,
        count,
        capabilities,
    })
}

#[derive(Deserialize)]
struct UploadAssetParams {
    /// "issue" or "mr"
    context_type: String,
    key: String,
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

    let size = data.len();
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

    Ok(ToolOutput::AssetUploaded {
        url,
        filename: params.filename,
        size,
    })
}

#[derive(Deserialize)]
struct DownloadAssetParams {
    /// "issue" or "mr"
    context_type: String,
    key: String,
    /// Asset identifier (provider-specific)
    asset_id: String,
}

async fn execute_download_asset(
    provider: &dyn devboy_core::Provider,
    args: &Value,
    asset_manager: Option<&devboy_assets::AssetManager>,
) -> Result<ToolOutput> {
    let params: DownloadAssetParams = serde_json::from_value(args.clone())?;
    debug!(context_type = %params.context_type, key = %params.key, asset_id = %params.asset_id, "download_asset");

    // Check local cache first.
    if let Some(mgr) = asset_manager
        && let Ok(Some(resolved)) = mgr.get(&params.asset_id)
    {
        return Ok(ToolOutput::AssetDownloaded {
            asset_id: params.asset_id,
            size: resolved.asset.size as usize,
            local_path: Some(resolved.absolute_path.to_string_lossy().into_owned()),
            data: None,
            cached: true,
        });
    }

    // Not cached — download from provider.
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

    // Store in cache if available.
    if let Some(mgr) = asset_manager {
        let context = match params.context_type.as_str() {
            "mr" | "merge_request" | "pull_request" => devboy_core::AssetContext::MergeRequest {
                mr_id: params.key.clone(),
            },
            _ => devboy_core::AssetContext::Issue {
                key: params.key.clone(),
            },
        };
        let filename = devboy_core::filename_from_url(&params.asset_id);
        match mgr.store(devboy_assets::StoreRequest {
            context,
            asset_id: Some(&params.asset_id),
            filename: &filename,
            mime_type: None,
            remote_url: None,
            data: &bytes,
        }) {
            Ok(cached) => {
                let abs = mgr.cache_dir().join(&cached.local_path);
                return Ok(ToolOutput::AssetDownloaded {
                    asset_id: cached.id,
                    size: cached.size as usize,
                    local_path: Some(abs.to_string_lossy().into_owned()),
                    data: None,
                    cached: true,
                });
            }
            Err(e) => {
                tracing::warn!(?e, "failed to cache asset, returning base64 fallback");
            }
        }
    }

    // Fallback: return base64-encoded content.
    if bytes.len() > MAX_FILE_SIZE {
        return Err(Error::InvalidData(format!(
            "downloaded attachment is {} bytes, max allowed for base64 response is {} bytes",
            bytes.len(),
            MAX_FILE_SIZE,
        )));
    }

    let encoded = base64_encode(&bytes);
    Ok(ToolOutput::AssetDownloaded {
        asset_id: params.asset_id,
        size: bytes.len(),
        local_path: None,
        data: Some(encoded),
        cached: false,
    })
}

#[derive(Deserialize)]
struct DeleteAssetParams {
    key: String,
    asset_id: String,
}

async fn execute_delete_asset(
    provider: &dyn devboy_core::Provider,
    args: &Value,
    asset_manager: Option<&devboy_assets::AssetManager>,
) -> Result<ToolOutput> {
    let params: DeleteAssetParams = serde_json::from_value(args.clone())?;
    debug!(key = %params.key, asset_id = %params.asset_id, "delete_asset");

    IssueProvider::delete_attachment(provider, &params.key, &params.asset_id).await?;

    // Evict from local cache so stale files aren't served.
    if let Some(mgr) = asset_manager
        && let Err(e) = mgr.delete(&params.asset_id)
    {
        tracing::warn!(?e, asset_id = %params.asset_id, "failed to evict deleted asset from cache");
    }

    let message = format!(
        "Attachment '{}' deleted from {}",
        params.asset_id, params.key
    );
    Ok(ToolOutput::AssetDeleted {
        asset_id: params.asset_id,
        message,
    })
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

// =============================================================================
// Jira Structure tool handlers
// =============================================================================

async fn execute_get_structures(provider: &dyn devboy_core::Provider) -> Result<ToolOutput> {
    let result = provider.get_structures().await?;
    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::Structures(result.items, Some(meta)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetStructureForestParams {
    structure_id: u64,
    offset: Option<u64>,
    limit: Option<u64>,
}

async fn execute_get_structure_forest(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetStructureForestParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("missing 'structureId': {e}")))?;
    let forest = provider
        .get_structure_forest(
            params.structure_id,
            GetForestOptions {
                offset: params.offset,
                limit: Some(params.limit.unwrap_or(200)),
            },
        )
        .await?;
    Ok(ToolOutput::StructureForest(Box::new(forest)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddStructureRowsParams {
    structure_id: u64,
    items: Vec<Value>,
    under: Option<u64>,
    after: Option<u64>,
    forest_version: Option<u64>,
}

/// Turn a single `items[]` entry from `add_structure_rows` into a
/// `StructureRowItem`. The tool schema can only express a list of
/// strings, so callers wanting to set `item_type` or nested fields
/// are forced to pass JSON inside a string. Accept both:
///
/// - bare string → `{ item_id: s, item_type: None }`
/// - JSON object (either as a real object or a string that parses as
///   one) → `serde_json::from_value`
///
/// Malformed input surfaces as `InvalidData` rather than silently
/// dropping values through `unwrap_or_default()`.
fn parse_structure_row_item(v: Value) -> Result<StructureRowItem> {
    if let Some(s) = v.as_str() {
        if let Ok(parsed) = serde_json::from_str::<Value>(s)
            && parsed.is_object()
        {
            return serde_json::from_value(parsed)
                .map_err(|e| Error::InvalidData(format!("invalid structure row item JSON: {e}")));
        }
        return Ok(StructureRowItem {
            item_id: s.to_string(),
            item_type: None,
        });
    }
    serde_json::from_value(v)
        .map_err(|e| Error::InvalidData(format!("invalid structure row item: {e}")))
}

/// Turn a `columns[]` entry from `get_structure_values` /
/// `save_structure_view` into a `StructureViewColumn`. Same dual
/// shape as `parse_structure_row_item`: bare string means
/// `{ field: Some(s) }`, anything else (or a JSON-object string) is
/// deserialised as a full spec. Errors propagate.
fn parse_structure_column_spec(v: Value) -> Result<StructureViewColumn> {
    if let Some(s) = v.as_str() {
        if let Ok(parsed) = serde_json::from_str::<Value>(s)
            && parsed.is_object()
        {
            return serde_json::from_value(parsed).map_err(|e| {
                Error::InvalidData(format!("invalid structure column spec JSON: {e}"))
            });
        }
        return Ok(StructureViewColumn {
            field: Some(s.to_string()),
            ..Default::default()
        });
    }
    serde_json::from_value(v)
        .map_err(|e| Error::InvalidData(format!("invalid structure column spec: {e}")))
}

async fn execute_add_structure_rows(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: AddStructureRowsParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid add_structure_rows params: {e}")))?;

    let items: Vec<StructureRowItem> = params
        .items
        .into_iter()
        .map(parse_structure_row_item)
        .collect::<Result<Vec<_>>>()?;

    let result = provider
        .add_structure_rows(
            params.structure_id,
            AddStructureRowsInput {
                items,
                under: params.under,
                after: params.after,
                forest_version: params.forest_version,
            },
        )
        .await?;
    Ok(ToolOutput::ForestModified(result))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MoveStructureRowsParams {
    structure_id: u64,
    row_ids: Vec<u64>,
    under: Option<u64>,
    after: Option<u64>,
    forest_version: Option<u64>,
}

async fn execute_move_structure_rows(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: MoveStructureRowsParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid move_structure_rows params: {e}")))?;
    let result = provider
        .move_structure_rows(
            params.structure_id,
            MoveStructureRowsInput {
                row_ids: params.row_ids,
                under: params.under,
                after: params.after,
                forest_version: params.forest_version,
            },
        )
        .await?;
    Ok(ToolOutput::ForestModified(result))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveStructureRowParams {
    structure_id: u64,
    row_id: u64,
}

async fn execute_remove_structure_row(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: RemoveStructureRowParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid remove_structure_row params: {e}")))?;
    provider
        .remove_structure_row(params.structure_id, params.row_id)
        .await?;
    Ok(ToolOutput::Text(format!(
        "Row {} removed from structure {}",
        params.row_id, params.structure_id
    )))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetStructureValuesParams {
    structure_id: u64,
    rows: Vec<u64>,
    columns: Vec<Value>,
}

async fn execute_get_structure_values(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetStructureValuesParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid get_structure_values params: {e}")))?;

    let columns: Vec<StructureViewColumn> = params
        .columns
        .into_iter()
        .map(parse_structure_column_spec)
        .collect::<Result<Vec<_>>>()?;

    let result = provider
        .get_structure_values(GetStructureValuesInput {
            structure_id: params.structure_id,
            rows: params.rows,
            columns,
        })
        .await?;
    Ok(ToolOutput::StructureValues(Box::new(result)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetStructureViewsParams {
    structure_id: u64,
    view_id: Option<u64>,
}

async fn execute_get_structure_views(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetStructureViewsParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid get_structure_views params: {e}")))?;
    let views = provider
        .get_structure_views(params.structure_id, params.view_id)
        .await?;
    Ok(ToolOutput::StructureViews(views, None))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveStructureViewParams {
    id: Option<u64>,
    structure_id: u64,
    name: String,
    columns: Option<Vec<Value>>,
    group_by: Option<String>,
    sort_by: Option<String>,
    filter: Option<String>,
}

async fn execute_save_structure_view(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: SaveStructureViewParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid save_structure_view params: {e}")))?;

    let columns: Option<Vec<StructureViewColumn>> = params
        .columns
        .map(|cols| {
            cols.into_iter()
                .map(parse_structure_column_spec)
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;

    let view = provider
        .save_structure_view(SaveStructureViewInput {
            id: params.id,
            structure_id: params.structure_id,
            name: params.name,
            columns,
            group_by: params.group_by,
            sort_by: params.sort_by,
            filter: params.filter,
        })
        .await?;
    Ok(ToolOutput::StructureViews(vec![view], None))
}

#[derive(Deserialize)]
struct CreateStructureParams {
    name: String,
    description: Option<String>,
}

async fn execute_create_structure(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: CreateStructureParams = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("missing 'name': {e}")))?;
    let structure = provider
        .create_structure(CreateStructureInput {
            name: params.name,
            description: params.description,
        })
        .await?;
    Ok(ToolOutput::Structures(vec![structure], None))
}

// =============================================================================
// Project versions / fixVersion handlers (issue #238)
// =============================================================================

/// Tri-state filter for `released` / `archived` — accepts the strings
/// `"true"`, `"false"`, `"all"` (default `"all"` → no filter).
fn parse_tri_filter(s: Option<&str>) -> Result<Option<bool>> {
    match s.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        None | Some("") | Some("all") | Some("any") => Ok(None),
        Some("true") | Some("yes") | Some("1") => Ok(Some(true)),
        Some("false") | Some("no") | Some("0") => Ok(Some(false)),
        Some(other) => Err(Error::InvalidData(format!(
            "expected 'true' | 'false' | 'all', got '{other}'"
        ))),
    }
}

/// Validate that a string is an ISO 8601 calendar date in `YYYY-MM-DD`
/// form. Jira accepts that exact shape on `releaseDate`/`startDate`
/// payloads; anything else (timestamps, slashes, locale formats) gets
/// rejected with a 400 by the server, so catch it client-side with a
/// clear error pointing at the offending field.
fn validate_iso_date(field: &str, value: &str) -> Result<()> {
    let bytes = value.as_bytes();
    let shape_ok = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[8..].iter().all(u8::is_ascii_digit);
    if !shape_ok {
        return Err(Error::InvalidData(format!(
            "{field} must be an ISO 8601 calendar date (YYYY-MM-DD), got '{value}'"
        )));
    }
    let month: u32 = value[5..7].parse().unwrap();
    let day: u32 = value[8..10].parse().unwrap();
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(Error::InvalidData(format!(
            "{field} = '{value}' is not a valid calendar date"
        )));
    }
    Ok(())
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ListProjectVersionsArgs {
    project: Option<String>,
    released: Option<String>,
    archived: Option<String>,
    limit: Option<u32>,
    include_issue_count: Option<bool>,
}

async fn execute_list_project_versions(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: ListProjectVersionsArgs = parse_tool_params(args, "list_project_versions")?;

    // Paper 1 / TrimTree defaults: hide archived noise + cap at 20 most
    // recent. Defaults only apply when the caller omits the field —
    // explicit `"all"` must round-trip as `None` (no filter).
    let archived = match params.archived.as_deref() {
        None => Some(false),
        Some(s) => parse_tri_filter(Some(s))?,
    };
    let released = match params.released.as_deref() {
        None => None,
        Some(s) => parse_tri_filter(Some(s))?,
    };
    // `limit: 0` would round-trip to a useless empty list — reject up
    // front instead of letting the schema's `min: 1` get bypassed by a
    // raw call site (Codex review on PR #239).
    if let Some(0) = params.limit {
        return Err(Error::InvalidData(
            "limit must be at least 1 (use the default by omitting the field)".into(),
        ));
    }
    let limit = params.limit.unwrap_or(20).min(200);

    let result = provider
        .list_project_versions(ListProjectVersionsParams {
            project: params.project.unwrap_or_default(),
            released,
            archived,
            limit: Some(limit),
            include_issue_count: params.include_issue_count.unwrap_or(false),
        })
        .await?;

    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::ProjectVersions(result.items, Some(meta)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpsertProjectVersionArgs {
    project: Option<String>,
    name: String,
    description: Option<String>,
    start_date: Option<String>,
    release_date: Option<String>,
    released: Option<bool>,
    archived: Option<bool>,
}

async fn execute_upsert_project_version(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: UpsertProjectVersionArgs = serde_json::from_value(args.clone())
        .map_err(|e| Error::InvalidData(format!("invalid upsert_project_version params: {e}")))?;

    // Codex review on PR #239 — validate inputs before they cross the
    // wire so the failure points at the parameter, not at "Jira said 400".
    if let Some(ref d) = params.start_date {
        validate_iso_date("startDate", d)?;
    }
    if let Some(ref d) = params.release_date {
        validate_iso_date("releaseDate", d)?;
    }

    let version = provider
        .upsert_project_version(UpsertProjectVersionInput {
            project: params.project.unwrap_or_default(),
            name: params.name,
            description: params.description,
            start_date: params.start_date,
            release_date: params.release_date,
            released: params.released,
            archived: params.archived,
        })
        .await?;

    Ok(ToolOutput::SingleProjectVersion(Box::new(version)))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GetBoardSprintsArgs {
    board_id: u64,
    /// Optional state filter: `active`, `future`, `closed`, or `all`
    /// (default `all`).
    state: Option<String>,
}

async fn execute_get_board_sprints(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetBoardSprintsArgs = parse_tool_params(args, "get_board_sprints")?;
    let state = match params.state.as_deref() {
        None | Some("all") => SprintState::All,
        Some("active") => SprintState::Active,
        Some("future") => SprintState::Future,
        Some("closed") => SprintState::Closed,
        Some(other) => {
            return Err(Error::InvalidData(format!(
                "invalid sprint state `{other}` — expected one of: active, future, closed, all"
            )));
        }
    };

    let result = provider.get_board_sprints(params.board_id, state).await?;
    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::Sprints(result.items, Some(meta)))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AssignToSprintArgs {
    sprint_id: u64,
    issue_keys: Vec<String>,
}

async fn execute_assign_to_sprint(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: AssignToSprintArgs = parse_tool_params(args, "assign_to_sprint")?;
    if params.issue_keys.is_empty() {
        return Err(Error::InvalidData(
            "issueKeys must contain at least one issue key".into(),
        ));
    }
    let count = params.issue_keys.len();
    provider
        .assign_to_sprint(AssignToSprintInput {
            sprint_id: params.sprint_id,
            issue_keys: params.issue_keys,
        })
        .await?;
    Ok(ToolOutput::Text(format!(
        "Moved {count} issue(s) to sprint {}.",
        params.sprint_id
    )))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GetCustomFieldsArgs {
    project: Option<String>,
    issue_type: Option<String>,
    search: Option<String>,
    limit: Option<u32>,
}

async fn execute_get_custom_fields(
    provider: &dyn devboy_core::Provider,
    args: &Value,
) -> Result<ToolOutput> {
    let params: GetCustomFieldsArgs = parse_tool_params(args, "get_custom_fields")?;
    if let Some(0) = params.limit {
        return Err(Error::InvalidData(
            "limit must be at least 1 (use the default by omitting the field)".into(),
        ));
    }
    let result = provider
        .list_custom_fields(ListCustomFieldsParams {
            project: params.project,
            issue_type: params.issue_type,
            search: params.search,
            limit: params.limit,
        })
        .await?;
    let meta = ResultMeta {
        pagination: result.pagination,
        sort_info: result.sort_info,
    };
    Ok(ToolOutput::CustomFields(result.items, Some(meta)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use devboy_core::{
        Comment, CreateMergeRequestInput, Discussion, FileDiff, Issue, IssueLink, IssueProvider,
        IssueRelations, KbPage, KbPageContent, KbSpace, KnowledgeBaseProvider, MergeRequest,
        MergeRequestProvider, Provider, User,
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
            custom_fields: std::collections::HashMap::new(),
            ..Default::default()
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

    fn sample_kb_space() -> KbSpace {
        KbSpace {
            id: "space-1".into(),
            key: "ENG".into(),
            name: "Engineering".into(),
            ..Default::default()
        }
    }

    fn sample_kb_page() -> KbPage {
        KbPage {
            id: "page-1".into(),
            title: "Architecture".into(),
            space_key: Some("ENG".into()),
            ..Default::default()
        }
    }

    fn sample_kb_page_content() -> KbPageContent {
        KbPageContent {
            page: sample_kb_page(),
            content: "<p>body</p>".into(),
            content_type: "storage".into(),
            ancestors: vec![],
            labels: vec!["docs".into()],
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
        async fn get_structures(
            &self,
        ) -> devboy_core::Result<devboy_core::ProviderResult<devboy_core::Structure>> {
            Ok(vec![sample_structure()].into())
        }
        async fn get_structure_forest(
            &self,
            structure_id: u64,
            _options: devboy_core::GetForestOptions,
        ) -> devboy_core::Result<devboy_core::StructureForest> {
            Ok(sample_forest(structure_id))
        }
        async fn add_structure_rows(
            &self,
            _structure_id: u64,
            input: devboy_core::AddStructureRowsInput,
        ) -> devboy_core::Result<devboy_core::ForestModifyResult> {
            Ok(devboy_core::ForestModifyResult {
                version: 2,
                affected_count: input.items.len(),
            })
        }
        async fn move_structure_rows(
            &self,
            _structure_id: u64,
            input: devboy_core::MoveStructureRowsInput,
        ) -> devboy_core::Result<devboy_core::ForestModifyResult> {
            Ok(devboy_core::ForestModifyResult {
                version: 3,
                affected_count: input.row_ids.len(),
            })
        }
        async fn remove_structure_row(
            &self,
            _structure_id: u64,
            _row_id: u64,
        ) -> devboy_core::Result<()> {
            Ok(())
        }
        async fn get_structure_values(
            &self,
            input: devboy_core::GetStructureValuesInput,
        ) -> devboy_core::Result<devboy_core::StructureValues> {
            Ok(devboy_core::StructureValues {
                structure_id: input.structure_id,
                values: vec![],
            })
        }
        async fn get_structure_views(
            &self,
            structure_id: u64,
            _view_id: Option<u64>,
        ) -> devboy_core::Result<Vec<devboy_core::StructureView>> {
            Ok(vec![sample_view(structure_id)])
        }
        async fn save_structure_view(
            &self,
            input: devboy_core::SaveStructureViewInput,
        ) -> devboy_core::Result<devboy_core::StructureView> {
            Ok(devboy_core::StructureView {
                id: input.id.unwrap_or(99),
                name: input.name,
                structure_id: input.structure_id,
                ..Default::default()
            })
        }
        async fn create_structure(
            &self,
            input: devboy_core::CreateStructureInput,
        ) -> devboy_core::Result<devboy_core::Structure> {
            Ok(devboy_core::Structure {
                id: 42,
                name: input.name,
                description: input.description,
            })
        }
        async fn list_project_versions(
            &self,
            params: devboy_core::ListProjectVersionsParams,
        ) -> devboy_core::Result<devboy_core::ProviderResult<devboy_core::ProjectVersion>> {
            // Echo applied filters back through the data so dispatch
            // tests can pin behaviour without sniffing call args.
            let mut name = format!(
                "v-released={:?}-archived={:?}-limit={:?}-expand={}",
                params.released, params.archived, params.limit, params.include_issue_count
            );
            if !params.project.is_empty() {
                name.push_str(&format!("-project={}", params.project));
            }
            Ok(vec![devboy_core::ProjectVersion {
                id: "1".into(),
                project: if params.project.is_empty() {
                    "MOCK".into()
                } else {
                    params.project
                },
                name,
                description: Some("desc".into()),
                start_date: None,
                release_date: Some("2026-01-01".into()),
                released: false,
                archived: false,
                overdue: None,
                issue_count: Some(0),
                unresolved_issue_count: None,
                source: "mock".into(),
            }]
            .into())
        }
        async fn upsert_project_version(
            &self,
            input: devboy_core::UpsertProjectVersionInput,
        ) -> devboy_core::Result<devboy_core::ProjectVersion> {
            Ok(devboy_core::ProjectVersion {
                id: "777".into(),
                project: if input.project.is_empty() {
                    "MOCK".into()
                } else {
                    input.project
                },
                name: input.name,
                description: input.description,
                start_date: input.start_date,
                release_date: input.release_date,
                released: input.released.unwrap_or(false),
                archived: input.archived.unwrap_or(false),
                overdue: None,
                issue_count: None,
                unresolved_issue_count: None,
                source: "mock".into(),
            })
        }
        async fn get_board_sprints(
            &self,
            board_id: u64,
            state: devboy_core::SprintState,
        ) -> devboy_core::Result<devboy_core::ProviderResult<devboy_core::Sprint>> {
            // Echo applied filters into the name so dispatch tests can
            // pin behaviour without sniffing call args.
            Ok(vec![devboy_core::Sprint {
                id: 1,
                name: format!("sprint-board={board_id}-state={state:?}"),
                state: "active".into(),
                origin_board_id: Some(board_id),
                start_date: None,
                end_date: None,
                goal: None,
            }]
            .into())
        }
        async fn assign_to_sprint(
            &self,
            _input: devboy_core::AssignToSprintInput,
        ) -> devboy_core::Result<()> {
            Ok(())
        }
        async fn list_custom_fields(
            &self,
            params: devboy_core::ListCustomFieldsParams,
        ) -> devboy_core::Result<devboy_core::ProviderResult<devboy_core::CustomFieldDescriptor>>
        {
            // Return a few fixed entries so dispatch tests can pin
            // filter and limit behaviour.
            let mut all = vec![
                devboy_core::CustomFieldDescriptor {
                    id: "customfield_10014".into(),
                    name: "Epic Link".into(),
                    field_type: "any".into(),
                    description: None,
                    native: None,
                },
                devboy_core::CustomFieldDescriptor {
                    id: "customfield_10011".into(),
                    name: "Epic Name".into(),
                    field_type: "string".into(),
                    description: None,
                    native: None,
                },
                devboy_core::CustomFieldDescriptor {
                    id: "customfield_10020".into(),
                    name: "Sprint".into(),
                    field_type: "array".into(),
                    description: None,
                    native: None,
                },
            ];
            if let Some(needle) = params.search.as_deref().map(str::to_lowercase) {
                all.retain(|f| f.name.to_lowercase().contains(&needle));
            }
            let total = all.len() as u32;
            let limit = params.limit.unwrap_or(50);
            if (limit as usize) < all.len() {
                all.truncate(limit as usize);
            }
            let pagination = devboy_core::Pagination {
                offset: 0,
                limit,
                total: Some(total),
                has_more: (all.len() as u32) < total,
                next_cursor: None,
            };
            Ok(devboy_core::ProviderResult::new(all).with_pagination(pagination))
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
    impl KnowledgeBaseProvider for MockProvider {
        fn provider_name(&self) -> &'static str {
            "mock"
        }

        async fn get_spaces(&self) -> devboy_core::Result<devboy_core::ProviderResult<KbSpace>> {
            Ok(vec![sample_kb_space()].into())
        }

        async fn list_pages(
            &self,
            _params: ListPagesParams,
        ) -> devboy_core::Result<devboy_core::ProviderResult<KbPage>> {
            Ok(vec![sample_kb_page()].into())
        }

        async fn get_page(&self, _page_id: &str) -> devboy_core::Result<KbPageContent> {
            Ok(sample_kb_page_content())
        }

        async fn create_page(
            &self,
            _params: devboy_core::CreatePageParams,
        ) -> devboy_core::Result<KbPage> {
            Ok(sample_kb_page())
        }

        async fn update_page(
            &self,
            _params: devboy_core::UpdatePageParams,
        ) -> devboy_core::Result<KbPage> {
            Ok(sample_kb_page())
        }

        async fn search(
            &self,
            _params: SearchKbParams,
        ) -> devboy_core::Result<devboy_core::ProviderResult<KbPage>> {
            Ok(vec![sample_kb_page()].into())
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
        assert!(SUPPORTED_TOOLS.contains(&"get_knowledge_base_spaces"));
        assert!(SUPPORTED_TOOLS.contains(&"list_knowledge_base_pages"));
        assert!(SUPPORTED_TOOLS.contains(&"get_knowledge_base_page"));
        assert!(SUPPORTED_TOOLS.contains(&"create_knowledge_base_page"));
        assert!(SUPPORTED_TOOLS.contains(&"update_knowledge_base_page"));
        assert!(SUPPORTED_TOOLS.contains(&"search_knowledge_base"));
        assert!(SUPPORTED_TOOLS.contains(&"get_messenger_chats"));
        assert!(SUPPORTED_TOOLS.contains(&"get_chat_messages"));
        assert!(SUPPORTED_TOOLS.contains(&"search_chat_messages"));
        assert!(SUPPORTED_TOOLS.contains(&"send_message"));
        assert_eq!(SUPPORTED_TOOLS.len(), 40);
    }

    #[tokio::test]
    async fn test_dispatch_get_knowledge_base_spaces() {
        let provider = MockProvider;
        let result =
            dispatch_knowledge_base_tool("get_knowledge_base_spaces", &Value::Null, &provider)
                .await
                .unwrap();
        assert!(matches!(result, ToolOutput::KnowledgeBaseSpaces(v, _) if v.len() == 1));
    }

    #[tokio::test]
    async fn test_dispatch_list_knowledge_base_pages() {
        let provider = MockProvider;
        let args = serde_json::json!({"spaceKey": "ENG", "limit": 10});
        let result = dispatch_knowledge_base_tool("list_knowledge_base_pages", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::KnowledgeBasePages(v, _) if v.len() == 1));
    }

    #[tokio::test]
    async fn test_dispatch_get_knowledge_base_page() {
        let provider = MockProvider;
        let args = serde_json::json!({"pageId": "page-1"});
        let result = dispatch_knowledge_base_tool("get_knowledge_base_page", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::KnowledgeBasePage(_)));
    }

    #[tokio::test]
    async fn test_dispatch_create_knowledge_base_page() {
        let provider = MockProvider;
        let args = serde_json::json!({
            "spaceKey": "ENG",
            "title": "New Page",
            "content": "<p>body</p>",
            "contentType": "storage",
            "labels": ["docs"]
        });
        let result = dispatch_knowledge_base_tool("create_knowledge_base_page", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::KnowledgeBasePageSummary(_)));
    }

    #[tokio::test]
    async fn test_dispatch_update_knowledge_base_page() {
        let provider = MockProvider;
        let args = serde_json::json!({
            "pageId": "page-1",
            "title": "Updated",
            "content": "<p>new body</p>",
            "version": 2
        });
        let result = dispatch_knowledge_base_tool("update_knowledge_base_page", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::KnowledgeBasePageSummary(_)));
    }

    #[tokio::test]
    async fn test_dispatch_search_knowledge_base() {
        let provider = MockProvider;
        let args = serde_json::json!({"query": "architecture", "spaceKey": "ENG"});
        let result = dispatch_knowledge_base_tool("search_knowledge_base", &args, &provider)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::KnowledgeBasePages(v, _) if v.len() == 1));
    }

    // --- Issue tool dispatch tests ---

    #[tokio::test]
    async fn test_dispatch_get_issues() {
        let provider = MockProvider;
        let args = serde_json::json!({"state": "open", "limit": 10});
        let result = dispatch_tool("get_issues", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Issues(v, _) if v.len() == 1));
    }

    #[tokio::test]
    async fn test_dispatch_get_issues_empty_args() {
        let provider = MockProvider;
        let result = dispatch_tool("get_issues", &Value::Null, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Issues(_, _)));
    }

    #[tokio::test]
    async fn test_dispatch_get_issues_invalid_params_are_rejected() {
        // Regression for #188: before parse_tool_params the executor
        // silently accepted `{"state": 42}` by falling through to
        // default() and ran the tool without any filter. Now it must
        // surface the deserialisation error instead.
        let provider = MockProvider;
        let args = serde_json::json!({"state": 42});
        let err = dispatch_tool("get_issues", &args, &provider, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, devboy_core::Error::InvalidData(ref msg) if msg.contains("get_issues")),
            "expected InvalidData referencing get_issues, got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_dispatch_get_merge_requests_invalid_params_rejected() {
        let provider = MockProvider;
        let args = serde_json::json!({"limit": "not-a-number"});
        let err = dispatch_tool("get_merge_requests", &args, &provider, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, devboy_core::Error::InvalidData(ref msg) if msg.contains("get_merge_requests")),
            "expected InvalidData referencing get_merge_requests, got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_dispatch_get_pipeline_invalid_params_rejected() {
        let provider = MockProvider;
        let args = serde_json::json!({"includeFailedLogs": "yes"});
        let err = dispatch_tool("get_pipeline", &args, &provider, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, devboy_core::Error::InvalidData(ref msg) if msg.contains("get_pipeline")),
            "expected InvalidData referencing get_pipeline, got {err:?}"
        );
    }

    #[test]
    fn parse_tool_params_null_yields_default() {
        #[derive(Debug, Default, serde::Deserialize)]
        struct P {
            #[allow(dead_code)]
            x: Option<String>,
        }
        let _: P = parse_tool_params(&Value::Null, "test").expect("null → default");
    }

    #[test]
    fn parse_tool_params_empty_object_yields_default() {
        // MCP clients often send `{}` for "no arguments"; the helper
        // must accept it alongside `null`.
        #[derive(Debug, Default, serde::Deserialize)]
        struct P {
            #[allow(dead_code)]
            x: Option<String>,
        }
        let _: P = parse_tool_params(&serde_json::json!({}), "test").expect("{} → default");
    }

    #[test]
    fn parse_tool_params_invalid_maps_to_invalid_data() {
        #[derive(Debug, Default, serde::Deserialize)]
        struct P {
            #[allow(dead_code)]
            n: u32,
        }
        let err = parse_tool_params::<P>(&serde_json::json!({"n": "nope"}), "tool-x").unwrap_err();
        assert!(
            matches!(err, devboy_core::Error::InvalidData(ref msg) if msg.contains("tool-x")),
            "expected InvalidData(tool-x), got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_dispatch_get_issue() {
        let provider = MockProvider;
        // With includeComments/includeRelations defaulting to true, returns composite Text
        let args = serde_json::json!({"key": "gh#1"});
        let result = dispatch_tool("get_issue", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Text(_)));

        // Without extras, returns SingleIssue
        let args =
            serde_json::json!({"key": "gh#1", "includeComments": false, "includeRelations": false});
        let result = dispatch_tool("get_issue", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::SingleIssue(_)));
    }

    #[tokio::test]
    async fn test_dispatch_get_issue_missing_key() {
        let provider = MockProvider;
        let result = dispatch_tool("get_issue", &serde_json::json!({}), &provider, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_get_issue_comments() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "gh#1"});
        let result = dispatch_tool("get_issue_comments", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Comments(v, _) if v.len() == 1));
    }

    #[tokio::test]
    async fn test_dispatch_create_issue() {
        let provider = MockProvider;
        let args =
            serde_json::json!({"title": "New issue", "description": "Body", "labels": ["bug"]});
        let result = dispatch_tool("create_issue", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::SingleIssue(_)));
    }

    #[test]
    fn create_issue_params_accepts_parent_id_alias() {
        let args = serde_json::json!({ "title": "t", "parentId": "DEV-799" });
        let params: CreateIssueParams = serde_json::from_value(args).unwrap();
        assert_eq!(params.parent.as_deref(), Some("DEV-799"));
    }

    #[test]
    fn create_issue_params_still_accepts_parent() {
        let args = serde_json::json!({ "title": "t", "parent": "DEV-799" });
        let params: CreateIssueParams = serde_json::from_value(args).unwrap();
        assert_eq!(params.parent.as_deref(), Some("DEV-799"));
    }

    #[tokio::test]
    async fn test_dispatch_update_issue() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "gh#1", "title": "Updated"});
        let result = dispatch_tool("update_issue", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::SingleIssue(_)));
    }

    #[tokio::test]
    async fn test_dispatch_add_issue_comment() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "gh#1", "body": "A comment"});
        let result = dispatch_tool("add_issue_comment", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Text(ref t) if t.contains("Comment added")));
    }

    #[tokio::test]
    async fn test_dispatch_get_issue_relations() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "gh#1"});
        let result = dispatch_tool("get_issue_relations", &args, &provider, None)
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
        let result = dispatch_tool(
            "get_issue_relations",
            &serde_json::json!({}),
            &provider,
            None,
        )
        .await;
        assert!(result.is_err());
    }

    // --- MR tool dispatch tests ---

    #[tokio::test]
    async fn test_dispatch_get_merge_requests() {
        let provider = MockProvider;
        let args = serde_json::json!({"state": "open", "limit": 5});
        let result = dispatch_tool("get_merge_requests", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::MergeRequests(v, _) if v.len() == 1));
    }

    #[tokio::test]
    async fn test_dispatch_get_merge_requests_empty_args() {
        let provider = MockProvider;
        let result = dispatch_tool("get_merge_requests", &Value::Null, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::MergeRequests(_, _)));
    }

    #[tokio::test]
    async fn test_dispatch_get_merge_request() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "pr#1"});
        let result = dispatch_tool("get_merge_request", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::SingleMergeRequest(_)));
    }

    #[tokio::test]
    async fn test_dispatch_get_merge_request_discussions() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "pr#1"});
        let result = dispatch_tool("get_merge_request_discussions", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Discussions(v, _) if v.len() == 1));
    }

    #[tokio::test]
    async fn test_dispatch_discussions_honors_advertised_limit_offset() {
        // The schema has always advertised limit/offset; they used to be
        // silently dropped (DEV-5447). Slicing must also be visible in
        // pagination metadata — a sliced page must not look complete.
        let provider = MockProvider;

        // offset past the only item → empty page, total still reported
        let args = serde_json::json!({"key": "pr#1", "offset": 1});
        let result = dispatch_tool("get_merge_request_discussions", &args, &provider, None)
            .await
            .unwrap();
        match result {
            ToolOutput::Discussions(items, Some(meta)) => {
                assert!(items.is_empty(), "offset=1 skips the single item");
                let p = meta
                    .pagination
                    .expect("slicing must produce pagination meta");
                assert_eq!(p.total, Some(1));
                assert_eq!(p.offset, 1);
                assert!(!p.has_more);
            }
            other => panic!("unexpected output: {other:?}"),
        }

        // limit larger than the set → everything returned, no has_more
        let args = serde_json::json!({"key": "pr#1", "limit": 5});
        let result = dispatch_tool("get_merge_request_discussions", &args, &provider, None)
            .await
            .unwrap();
        match result {
            ToolOutput::Discussions(items, Some(meta)) => {
                assert_eq!(items.len(), 1);
                let p = meta
                    .pagination
                    .expect("explicit limit must produce pagination meta");
                assert_eq!(p.total, Some(1));
                assert!(!p.has_more);
            }
            other => panic!("unexpected output: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_dispatch_discussions_accepts_mr_key_alias() {
        // Agents routinely send mrKey (the e2e suite does too); it used to
        // fail with "missing field 'key'".
        let provider = MockProvider;
        let args = serde_json::json!({"mrKey": "pr#1"});
        let result = dispatch_tool("get_merge_request_discussions", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Discussions(v, _) if v.len() == 1));
    }

    #[tokio::test]
    async fn test_dispatch_get_merge_request_diffs() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "pr#1"});
        let result = dispatch_tool("get_merge_request_diffs", &args, &provider, None)
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
        let result = dispatch_tool("create_merge_request", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::SingleMergeRequest(_)));
    }

    #[tokio::test]
    async fn test_dispatch_create_merge_request_comment_general() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "pr#1", "body": "LGTM"});
        let result = dispatch_tool("create_merge_request_comment", &args, &provider, None)
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
        let result = dispatch_tool("create_merge_request_comment", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Text(ref t) if t.contains("Comment added")));
    }

    #[test]
    fn test_create_merge_request_comment_params_accept_camel_case() {
        let args = serde_json::json!({
            "mrKey": "mr#566",
            "body": "reply",
            "filePath": "src/main.rs",
            "line": 12,
            "lineType": "new",
            "commitSha": "abc123",
            "discussionId": "788adb16c57805c9a5d59272c944cddea381a605"
        });

        let params: CreateMrCommentParams = serde_json::from_value(args).unwrap();
        assert_eq!(params.key, "mr#566");
        assert_eq!(params.file_path.as_deref(), Some("src/main.rs"));
        assert_eq!(params.line_type.as_deref(), Some("new"));
        assert_eq!(params.commit_sha.as_deref(), Some("abc123"));
        assert_eq!(
            params.discussion_id.as_deref(),
            Some("788adb16c57805c9a5d59272c944cddea381a605")
        );
    }

    #[test]
    fn test_create_merge_request_comment_params_still_accept_snake_case() {
        // Belt-and-suspenders: the camelCase aliases must not break the
        // original snake_case payload shape, which the MCP schema
        // declares and which some callers (our own skills included)
        // send today.
        let args = serde_json::json!({
            "key": "mr#566",
            "body": "reply",
            "file_path": "src/main.rs",
            "line": 12,
            "line_type": "new",
            "commit_sha": "abc123",
            "discussion_id": "788adb16c57805c9a5d59272c944cddea381a605"
        });

        let params: CreateMrCommentParams = serde_json::from_value(args).unwrap();
        assert_eq!(params.key, "mr#566");
        assert_eq!(params.file_path.as_deref(), Some("src/main.rs"));
        assert_eq!(params.line_type.as_deref(), Some("new"));
        assert_eq!(params.commit_sha.as_deref(), Some("abc123"));
        assert_eq!(
            params.discussion_id.as_deref(),
            Some("788adb16c57805c9a5d59272c944cddea381a605")
        );
    }

    #[tokio::test]
    async fn test_dispatch_create_merge_request_comment_accepts_camel_case_args() {
        // End-to-end: the executor's `dispatch_tool` path must accept
        // the same camelCase payload that real MCP clients (and our
        // skills) send, otherwise the alias would only help direct
        // `from_value` callers.
        let provider = MockProvider;
        let args = serde_json::json!({
            "mrKey": "mr#1",
            "body": "threaded reply",
            "discussionId": "abc123"
        });
        let result = dispatch_tool("create_merge_request_comment", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Text(ref t) if t.contains("Comment added")));
    }

    #[tokio::test]
    async fn test_dispatch_unknown_tool() {
        let provider = MockProvider;
        let result = dispatch_tool("nonexistent_tool", &Value::Null, &provider, None).await;
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
        let result = dispatch_tool("get_pipeline", &args, &provider, None).await;
        // MockProvider doesn't implement get_pipeline → ProviderUnsupported
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_get_job_logs_unsupported() {
        let provider = MockProvider;
        let args = serde_json::json!({"jobId": "123"});
        let result = dispatch_tool("get_job_logs", &args, &provider, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_get_pipeline_with_mr_key() {
        let provider = MockProvider;
        let args = serde_json::json!({"mrKey": "pr#1", "includeFailedLogs": false});
        let result = dispatch_tool("get_pipeline", &args, &provider, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_get_job_logs_with_pattern() {
        let provider = MockProvider;
        let args = serde_json::json!({"jobId": "123", "pattern": "ERROR", "context": 3});
        let result = dispatch_tool("get_job_logs", &args, &provider, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_get_job_logs_paginated() {
        let provider = MockProvider;
        let args = serde_json::json!({"jobId": "123", "offset": 10, "limit": 50});
        let result = dispatch_tool("get_job_logs", &args, &provider, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_get_job_logs_full() {
        let provider = MockProvider;
        let args = serde_json::json!({"jobId": "123", "full": true});
        let result = dispatch_tool("get_job_logs", &args, &provider, None).await;
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
        let result = dispatch_tool("get_available_statuses", &Value::Null, &provider, None).await;
        // MockProvider returns ProviderUnsupported for get_statuses
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_get_users_unsupported() {
        let provider = MockProvider;
        let args = serde_json::json!({"search": "test"});
        let result = dispatch_tool("get_users", &args, &provider, None).await;
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
        let result = dispatch_tool("link_issues", &args, &provider, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_get_epics() {
        let provider = MockProvider;
        let args = serde_json::json!({"state": "open", "limit": 10});
        let result = dispatch_tool("get_epics", &args, &provider, None)
            .await
            .unwrap();
        // Returns enriched JSON with goal_id and progress
        assert!(matches!(result, ToolOutput::Text(_)));
    }

    #[tokio::test]
    async fn test_dispatch_get_epics_empty_args() {
        let provider = MockProvider;
        let result = dispatch_tool("get_epics", &Value::Null, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Text(_)));
    }

    #[tokio::test]
    async fn test_dispatch_create_epic() {
        let provider = MockProvider;
        let args = serde_json::json!({"title": "New Epic", "description": "Epic description"});
        let result = dispatch_tool("create_epic", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::SingleIssue(_)));
    }

    #[tokio::test]
    async fn test_dispatch_update_epic() {
        let provider = MockProvider;
        let args = serde_json::json!({"key": "gh#1", "title": "Updated Epic"});
        let result = dispatch_tool("update_epic", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::SingleIssue(_)));
    }

    #[tokio::test]
    async fn test_dispatch_link_issues_missing_params() {
        let provider = MockProvider;
        let args = serde_json::json!({"source_key": "gh#1"});
        let result = dispatch_tool("link_issues", &args, &provider, None).await;
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

    // =========================================================================
    // Structure tool dispatch tests
    // =========================================================================

    fn sample_structure() -> devboy_core::Structure {
        devboy_core::Structure {
            id: 1,
            name: "Q1 Plan".into(),
            description: Some("Quarter 1 planning".into()),
        }
    }

    fn sample_forest(structure_id: u64) -> devboy_core::StructureForest {
        devboy_core::StructureForest {
            version: 1,
            structure_id,
            tree: vec![devboy_core::StructureNode {
                row_id: 100,
                item_id: Some("PROJ-1".into()),
                item_type: Some("issue".into()),
                children: vec![],
            }],
            total_count: Some(1),
        }
    }

    fn sample_view(structure_id: u64) -> devboy_core::StructureView {
        devboy_core::StructureView {
            id: 10,
            name: "Default".into(),
            structure_id,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_dispatch_get_structures() {
        let provider = MockProvider;
        let result = dispatch_tool("get_structures", &Value::Null, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Structures(ref items, _) if items.len() == 1));
        assert_eq!(result.type_name(), "structures");
    }

    #[tokio::test]
    async fn test_dispatch_get_structure_forest() {
        let provider = MockProvider;
        let args = serde_json::json!({"structureId": 1});
        let result = dispatch_tool("get_structure_forest", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::StructureForest(_)));
        assert_eq!(result.type_name(), "structure_forest");
    }

    #[tokio::test]
    async fn test_dispatch_get_structure_forest_missing_id() {
        let provider = MockProvider;
        let result = dispatch_tool("get_structure_forest", &Value::Null, &provider, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_dispatch_add_structure_rows() {
        let provider = MockProvider;
        let args = serde_json::json!({
            "structureId": 1,
            "items": ["PROJ-1", "PROJ-2"],
            "under": 100
        });
        let result = dispatch_tool("add_structure_rows", &args, &provider, None)
            .await
            .unwrap();
        match result {
            ToolOutput::ForestModified(r) => {
                assert_eq!(r.version, 2);
                assert_eq!(r.affected_count, 2);
            }
            _ => panic!("expected ForestModified"),
        }
    }

    #[tokio::test]
    async fn test_dispatch_move_structure_rows() {
        let provider = MockProvider;
        let args = serde_json::json!({
            "structureId": 1,
            "rowIds": [100, 101],
            "under": 200
        });
        let result = dispatch_tool("move_structure_rows", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::ForestModified(_)));
    }

    #[tokio::test]
    async fn test_dispatch_remove_structure_row() {
        let provider = MockProvider;
        let args = serde_json::json!({"structureId": 1, "rowId": 100});
        let result = dispatch_tool("remove_structure_row", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::Text(_)));
    }

    #[tokio::test]
    async fn test_dispatch_get_structure_values() {
        let provider = MockProvider;
        let args = serde_json::json!({
            "structureId": 1,
            "rows": [100],
            "columns": ["summary", {"field": "status"}]
        });
        let result = dispatch_tool("get_structure_values", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::StructureValues(_)));
    }

    #[tokio::test]
    async fn test_dispatch_get_structure_views() {
        let provider = MockProvider;
        let args = serde_json::json!({"structureId": 1});
        let result = dispatch_tool("get_structure_views", &args, &provider, None)
            .await
            .unwrap();
        assert!(matches!(result, ToolOutput::StructureViews(views, _) if views.len() == 1));
    }

    #[tokio::test]
    async fn test_dispatch_save_structure_view() {
        let provider = MockProvider;
        let args = serde_json::json!({
            "structureId": 1,
            "name": "Sprint View"
        });
        let result = dispatch_tool("save_structure_view", &args, &provider, None)
            .await
            .unwrap();
        assert!(
            matches!(result, ToolOutput::StructureViews(views, _) if views[0].name == "Sprint View")
        );
    }

    #[tokio::test]
    async fn test_dispatch_create_structure() {
        let provider = MockProvider;
        let args = serde_json::json!({"name": "New Structure", "description": "Test"});
        let result = dispatch_tool("create_structure", &args, &provider, None)
            .await
            .unwrap();
        match result {
            ToolOutput::Structures(items, _) => {
                assert_eq!(items[0].name, "New Structure");
                assert_eq!(items[0].id, 42);
            }
            _ => panic!("expected Structures"),
        }
    }

    // -------------------------------------------------------------------
    // Project versions / fixVersion (issue #238)
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn test_dispatch_list_project_versions_applies_paper_defaults() {
        // No filter args → archived defaults to false, limit to 20.
        let provider = MockProvider;
        let result = dispatch_tool(
            "list_project_versions",
            &serde_json::json!({}),
            &provider,
            None,
        )
        .await
        .unwrap();
        match result {
            ToolOutput::ProjectVersions(items, _) => {
                let echoed = &items[0].name;
                assert!(echoed.contains("released=None"), "got {echoed}");
                assert!(echoed.contains("archived=Some(false)"), "got {echoed}");
                assert!(echoed.contains("limit=Some(20)"), "got {echoed}");
                assert!(echoed.contains("expand=false"), "got {echoed}");
            }
            other => panic!("expected ProjectVersions, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_dispatch_list_project_versions_explicit_filters_override_defaults() {
        let provider = MockProvider;
        let args = serde_json::json!({
            "project": "PROJ",
            "released": "true",
            "archived": "all",
            "limit": 5,
            "includeIssueCount": true,
        });
        let result = dispatch_tool("list_project_versions", &args, &provider, None)
            .await
            .unwrap();
        match result {
            ToolOutput::ProjectVersions(items, _) => {
                let echoed = &items[0].name;
                assert!(echoed.contains("released=Some(true)"), "got {echoed}");
                assert!(echoed.contains("archived=None"), "got {echoed}");
                assert!(echoed.contains("limit=Some(5)"), "got {echoed}");
                assert!(echoed.contains("expand=true"), "got {echoed}");
                assert_eq!(items[0].project, "PROJ");
            }
            other => panic!("expected ProjectVersions, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_dispatch_list_project_versions_rejects_unknown_filter() {
        let provider = MockProvider;
        let err = dispatch_tool(
            "list_project_versions",
            &serde_json::json!({"released": "maybe"}),
            &provider,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, devboy_core::Error::InvalidData(ref m) if m.contains("'maybe'")),
            "expected InvalidData about 'maybe', got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_dispatch_upsert_project_version_returns_single() {
        let provider = MockProvider;
        let args = serde_json::json!({
            "project": "PROJ",
            "name": "3.18.0",
            "description": "release notes",
            "released": true,
            "releaseDate": "2026-05-01",
        });
        let result = dispatch_tool("upsert_project_version", &args, &provider, None)
            .await
            .unwrap();
        match result {
            ToolOutput::SingleProjectVersion(v) => {
                assert_eq!(v.name, "3.18.0");
                assert_eq!(v.project, "PROJ");
                assert!(v.released);
                assert_eq!(v.release_date.as_deref(), Some("2026-05-01"));
                assert_eq!(v.description.as_deref(), Some("release notes"));
            }
            other => panic!("expected SingleProjectVersion, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_dispatch_upsert_project_version_requires_name() {
        let provider = MockProvider;
        let err = dispatch_tool(
            "upsert_project_version",
            &serde_json::json!({"project": "PROJ"}),
            &provider,
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, devboy_core::Error::InvalidData(_)));
    }

    #[test]
    fn parse_tri_filter_accepts_canonical_strings() {
        assert_eq!(parse_tri_filter(None).unwrap(), None);
        assert_eq!(parse_tri_filter(Some("all")).unwrap(), None);
        assert_eq!(parse_tri_filter(Some("True")).unwrap(), Some(true));
        assert_eq!(parse_tri_filter(Some("false")).unwrap(), Some(false));
        assert_eq!(parse_tri_filter(Some("yes")).unwrap(), Some(true));
        assert_eq!(parse_tri_filter(Some("0")).unwrap(), Some(false));
        assert!(parse_tri_filter(Some("maybe")).is_err());
    }

    #[test]
    fn validate_iso_date_accepts_yyyy_mm_dd() {
        assert!(validate_iso_date("releaseDate", "2026-05-04").is_ok());
        assert!(validate_iso_date("releaseDate", "2026-12-31").is_ok());
    }

    #[test]
    fn validate_iso_date_rejects_other_shapes() {
        // Wrong shape
        assert!(validate_iso_date("releaseDate", "2026/05/04").is_err());
        assert!(validate_iso_date("releaseDate", "2026-5-4").is_err());
        assert!(validate_iso_date("releaseDate", "2026-05-04T00:00:00Z").is_err());
        assert!(validate_iso_date("releaseDate", "tomorrow").is_err());
        // Out-of-range month / day
        assert!(validate_iso_date("releaseDate", "2026-13-01").is_err());
        assert!(validate_iso_date("releaseDate", "2026-00-15").is_err());
        assert!(validate_iso_date("releaseDate", "2026-05-32").is_err());
    }

    #[tokio::test]
    async fn test_dispatch_upsert_project_version_rejects_bad_date() {
        let provider = MockProvider;
        let err = dispatch_tool(
            "upsert_project_version",
            &serde_json::json!({"name": "3.18.0", "releaseDate": "next friday"}),
            &provider,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, devboy_core::Error::InvalidData(ref m) if m.contains("releaseDate")),
            "expected InvalidData about releaseDate, got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_dispatch_list_project_versions_rejects_zero_limit() {
        let provider = MockProvider;
        let err = dispatch_tool(
            "list_project_versions",
            &serde_json::json!({"limit": 0}),
            &provider,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, devboy_core::Error::InvalidData(ref m) if m.contains("limit")),
            "expected InvalidData about limit, got {err:?}"
        );
    }

    // -------------------------------------------------------------------
    // Agile / Sprint dispatch (issue #198)
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn test_dispatch_get_board_sprints_default_state_is_all() {
        let provider = MockProvider;
        let result = dispatch_tool(
            "get_board_sprints",
            &serde_json::json!({"boardId": 7}),
            &provider,
            None,
        )
        .await
        .unwrap();
        match result {
            ToolOutput::Sprints(items, _) => {
                assert_eq!(items.len(), 1);
                assert!(items[0].name.contains("board=7"), "got {}", items[0].name);
                assert!(items[0].name.contains("state=All"), "got {}", items[0].name);
            }
            other => panic!("expected Sprints, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_dispatch_get_board_sprints_state_filter_round_trips() {
        let provider = MockProvider;
        let result = dispatch_tool(
            "get_board_sprints",
            &serde_json::json!({"boardId": 9, "state": "active"}),
            &provider,
            None,
        )
        .await
        .unwrap();
        match result {
            ToolOutput::Sprints(items, _) => {
                assert!(
                    items[0].name.contains("state=Active"),
                    "got {}",
                    items[0].name
                );
            }
            other => panic!("expected Sprints, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_dispatch_get_board_sprints_rejects_unknown_state() {
        let provider = MockProvider;
        let err = dispatch_tool(
            "get_board_sprints",
            &serde_json::json!({"boardId": 1, "state": "wat"}),
            &provider,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, devboy_core::Error::InvalidData(ref m) if m.contains("wat")),
            "expected InvalidData mentioning the bad value, got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_dispatch_assign_to_sprint_returns_text_summary() {
        let provider = MockProvider;
        let result = dispatch_tool(
            "assign_to_sprint",
            &serde_json::json!({
                "sprintId": 42,
                "issueKeys": ["PROJ-1", "PROJ-2"],
            }),
            &provider,
            None,
        )
        .await
        .unwrap();
        match result {
            ToolOutput::Text(msg) => {
                assert!(msg.contains("2 issue"), "got {msg}");
                assert!(msg.contains("42"), "got {msg}");
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_dispatch_assign_to_sprint_rejects_empty_issue_keys() {
        let provider = MockProvider;
        let err = dispatch_tool(
            "assign_to_sprint",
            &serde_json::json!({"sprintId": 1, "issueKeys": []}),
            &provider,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, devboy_core::Error::InvalidData(ref m) if m.contains("issueKeys")),
            "expected InvalidData about issueKeys, got {err:?}"
        );
    }

    // -------------------------------------------------------------------
    // get_custom_fields dispatch
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn test_dispatch_get_custom_fields_returns_all_entries_by_default() {
        let provider = MockProvider;
        let result = dispatch_tool("get_custom_fields", &serde_json::json!({}), &provider, None)
            .await
            .unwrap();
        match result {
            ToolOutput::CustomFields(items, _) => {
                assert_eq!(items.len(), 3);
                let names: Vec<_> = items.iter().map(|f| f.name.as_str()).collect();
                assert!(names.contains(&"Epic Link"));
                assert!(names.contains(&"Sprint"));
            }
            other => panic!("expected CustomFields, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_dispatch_get_custom_fields_search_filters_by_substring() {
        let provider = MockProvider;
        let result = dispatch_tool(
            "get_custom_fields",
            &serde_json::json!({"search": "epic"}),
            &provider,
            None,
        )
        .await
        .unwrap();
        match result {
            ToolOutput::CustomFields(items, _) => {
                assert_eq!(items.len(), 2);
                for f in items {
                    assert!(f.name.to_lowercase().contains("epic"));
                }
            }
            other => panic!("expected CustomFields, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_dispatch_get_custom_fields_rejects_zero_limit() {
        let provider = MockProvider;
        let err = dispatch_tool(
            "get_custom_fields",
            &serde_json::json!({"limit": 0}),
            &provider,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, devboy_core::Error::InvalidData(ref m) if m.contains("limit")),
            "expected InvalidData about limit, got {err:?}"
        );
    }

    // -------------------------------------------------------------------
    // Regression: structure-specific arg parsing
    // -------------------------------------------------------------------

    #[test]
    fn parse_row_item_bare_string_becomes_item_id() {
        let item = parse_structure_row_item(serde_json::json!("PROJ-1")).unwrap();
        assert_eq!(item.item_id, "PROJ-1");
        assert!(item.item_type.is_none());
    }

    #[test]
    fn parse_row_item_json_object_string_parses_fields() {
        let item = parse_structure_row_item(serde_json::json!(
            "{\"item_id\":\"PROJ-2\",\"item_type\":\"issue\"}"
        ))
        .unwrap();
        assert_eq!(item.item_id, "PROJ-2");
        assert_eq!(item.item_type.as_deref(), Some("issue"));
    }

    #[test]
    fn parse_row_item_malformed_json_object_is_error() {
        // Valid JSON object but fields do not match StructureRowItem.
        let err = parse_structure_row_item(serde_json::json!("{\"wrong\":true}")).unwrap_err();
        assert!(matches!(err, Error::InvalidData(_)));
    }

    #[test]
    fn parse_column_spec_bare_string_sets_field() {
        let col = parse_structure_column_spec(serde_json::json!("summary")).unwrap();
        assert_eq!(col.field.as_deref(), Some("summary"));
        assert!(col.formula.is_none());
    }

    #[test]
    fn parse_column_spec_formula_json_string_parses() {
        let col = parse_structure_column_spec(serde_json::json!(
            "{\"formula\":\"SUM(\\\"Story Points\\\")\"}"
        ))
        .unwrap();
        assert!(col.field.is_none());
        assert_eq!(col.formula.as_deref(), Some("SUM(\"Story Points\")"));
    }

    #[test]
    fn parse_column_spec_object_value_is_deserialised() {
        // A real JSON object (not a stringified one) should also work.
        let col = parse_structure_column_spec(serde_json::json!({"field": "status", "width": 120}))
            .unwrap();
        assert_eq!(col.field.as_deref(), Some("status"));
        assert_eq!(col.width, Some(120));
    }
}
