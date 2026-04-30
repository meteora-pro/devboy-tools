//! Format `ToolOutput` to text using the format pipeline.
//!
//! This module bridges the executor's typed output with the pipeline's
//! text formatting. Supports TOON (default) and JSON output formats.

use devboy_core::{Pagination, Result, SortInfo};
use devboy_format_pipeline::{OutputFormat, Pipeline, PipelineConfig};
use serde::Serialize;

use crate::output::ToolOutput;

/// Metadata about formatting result — compression stats, token estimates.
///
/// Per Paper 2 §Savings Accounting, every quoted savings number must
/// distinguish three orthogonal sources and name the baseline/tokenizer
/// against which the percentages are taken. The split fields below
/// (`dedup_savings_pct`, `encoder_savings_pct`, `combined_savings_pct`,
/// `baseline`, `tokenizer`) encode that contract on the live response.
///
/// For typed-domain transforms (issues / merge_requests / …), the
/// encoder runs without an L0 dedup hop, so `dedup_savings_pct == 0.0`
/// and `combined == encoder`. The cross-turn dedup contribution is
/// reported separately by `devboy-mcp::layered::SessionPipeline` via the
/// telemetry sink.
#[derive(Debug, Clone, Serialize)]
pub struct FormatMetadata {
    /// Size of raw JSON input (UTF-8 bytes)
    pub raw_chars: usize,
    /// Size of formatted output (UTF-8 bytes)
    pub output_chars: usize,
    /// Size of TOON/JSON output BEFORE budget trimming (UTF-8 bytes).
    /// If no trimming occurred, equals output_chars.
    /// toon_saved = raw_chars - pre_trim_chars
    /// trimmed_chars = pre_trim_chars - output_chars
    pub pre_trim_chars: usize,
    /// Estimated token count under the active tokenizer.
    pub estimated_tokens: usize,
    /// Compression ratio: output_chars / raw_chars (< 1.0 = savings)
    pub compression_ratio: f32,
    /// Output format used
    pub format: String,
    /// Whether output was truncated by budget trimming
    pub truncated: bool,
    /// Total items before truncation (e.g., 50 issues)
    pub total_items: Option<usize>,
    /// Items included after truncation (e.g., 20 issues)
    pub included_items: usize,
    /// Whether response was split into chunks (budget exceeded)
    pub chunked: bool,
    /// Number of chunks generated (1 = no chunking, >1 = chunked)
    pub total_chunks: usize,
    /// Which chunk was requested (1 = first/default, >1 = navigation)
    pub chunk_number: usize,
    /// Pagination metadata from the provider (offset, limit, total, has_more)
    pub provider_pagination: Option<Pagination>,
    /// Sort metadata from the provider (current sort, available sorts)
    pub provider_sort: Option<SortInfo>,
    /// L0 dedup savings as a fraction of baseline tokens (0.0 = no
    /// dedup hit on this response). Always `0.0` on the typed-domain
    /// path; populated by the MCP-server's layered pipeline when a
    /// hint is emitted.
    #[serde(default)]
    pub dedup_savings_pct: f32,
    /// L1/L2 encoder savings as a fraction of baseline tokens, computed
    /// over the *L0-miss* portion of the response. Equals
    /// `1.0 - encoded_tokens / baseline_tokens` for the typed-domain
    /// path.
    #[serde(default)]
    pub encoder_savings_pct: f32,
    /// Multiplicative combination of dedup and encoder savings, per
    /// the §Savings Accounting reporting rule: `combined = dedup +
    /// (1 - dedup) * encoder`.
    #[serde(default)]
    pub combined_savings_pct: f32,
    /// Baseline against which the percentages are taken
    /// (e.g. `"json_pretty"`, `"json_compact"`, `"toon"`). Required by
    /// the reporting rule — savings without a named baseline are not
    /// comparable across systems.
    #[serde(default)]
    pub baseline: String,
    /// Tokenizer used to compute `estimated_tokens` and the savings
    /// percentages above (e.g. `"o200k_base"`, `"cl100k_base"`,
    /// `"heuristic"`).
    #[serde(default)]
    pub tokenizer: String,
}

/// Result of formatting a tool output — content + metadata.
#[derive(Debug, Clone, Serialize)]
pub struct FormatResult {
    /// Formatted text content (TOON, JSON, etc.)
    pub content: String,
    /// Formatting metadata (sizes, compression, tokens)
    pub metadata: FormatMetadata,
}

/// Format a `ToolOutput` to text using the pipeline.
///
/// Returns `FormatResult` with content and metadata (compression stats, token estimates).
///
/// # Arguments
/// * `output` — typed result from executor
/// * `format` — output format string ("toon", "json"), defaults to "toon"
/// * `tool_name` — tool name (reserved for future strategy resolution)
/// * `config` — optional pipeline config override
pub fn format_output(
    output: ToolOutput,
    format: Option<&str>,
    _tool_name: Option<&str>,
    config: Option<PipelineConfig>,
) -> Result<FormatResult> {
    let output_format = match format {
        Some("json") => OutputFormat::Json,
        Some("mckp") => OutputFormat::Mckp,
        _ => OutputFormat::Toon,
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

    let format_name = match output_format {
        OutputFormat::Json => "json",
        OutputFormat::Toon => "toon",
        OutputFormat::Mckp => "mckp",
    };

    // Active tokenizer + baseline are reported alongside savings numbers
    // so downstream consumers can compare measurements taken under
    // different conditions. The typed-domain path always serialises
    // through `serde_json::to_string_pretty` first, so the implicit
    // baseline is `json_pretty`.
    //
    // Honest disclosure (Copilot review on PR #207): the typed-domain
    // pipeline does not yet plumb the runtime `AdaptiveConfig.profiles
    // .tokenizer` choice into this code path, so token counts are
    // produced by the chars/3.5 heuristic regardless of what the
    // operator configured. We label that explicitly here; BPE-accurate
    // counts apply on the L0/L1/L2 hot path inside `LayeredPipeline`
    // and via `tune analyze`, not on `format_output`. Tracked as a
    // follow-up in §Implementation Status.
    let baseline = "json_pretty";
    let tokenizer = "heuristic";
    let token_counter = devboy_format_pipeline::Tokenizer::Heuristic;

    let requested_chunk = pipeline_config.chunk.unwrap_or(1);
    let pipeline = Pipeline::with_config(pipeline_config);

    // Extract provider metadata before consuming output
    let provider_pagination = output.result_meta().and_then(|m| m.pagination.clone());
    let provider_sort = output.result_meta().and_then(|m| m.sort_info.clone());

    // Helper: convert TransformOutput to FormatResult
    let baseline_for_helper = baseline.to_string();
    let tokenizer_for_helper = tokenizer.to_string();
    let to_result = |t: devboy_format_pipeline::TransformOutput,
                     pag: Option<Pagination>,
                     sort: Option<SortInfo>|
     -> FormatResult {
        // output_chars = pure content size (TOON/JSON), used for compression metrics
        // content includes hints/chunk index on top, but metrics should reflect pipeline savings
        let content_chars = t.output_chars;
        let content = t.to_string_with_hints();
        let raw_chars = if t.raw_chars > 0 {
            t.raw_chars
        } else {
            content_chars
        };
        let pre_trim = if t.pre_trim_chars > 0 {
            t.pre_trim_chars
        } else {
            content_chars
        };
        // Extract chunk metrics from page_index
        let (chunked, total_chunks) = match &t.page_index {
            Some(idx) if idx.total_pages > 1 => (true, idx.total_pages),
            _ => (false, 1),
        };
        let chunk_number = requested_chunk;

        // §Savings Accounting — *token*-denominated, not byte-denominated.
        // We can't see the raw input here so we approximate baseline
        // tokens from `raw_chars` using the same tokenizer; the encoder
        // savings then live in token space (which is what the LLM is
        // billed on), independent of the chars/token ratio of either
        // format. Fixes Copilot review on PR #207.
        let baseline_tokens = if raw_chars > 0 {
            // `Tokenizer::Heuristic` matches the `estimated_tokens`
            // formula below; if a future change starts plumbing the
            // active profile, both must move together.
            (raw_chars as f64 / 3.5).ceil() as usize
        } else {
            0
        };
        let final_tokens = (content_chars as f64 / 3.5).ceil() as usize;
        let encoder_savings_pct = if baseline_tokens > 0 {
            ((baseline_tokens.saturating_sub(final_tokens)) as f32) / (baseline_tokens as f32)
        } else {
            0.0
        };
        // Typed-domain path has no L0 dedup hop, so combined == encoder.
        let combined_savings_pct = encoder_savings_pct;

        FormatResult {
            metadata: FormatMetadata {
                raw_chars,
                // output_chars = pipeline content size (without hints/chunk index)
                // Used for compression ratio and saved tokens calculation
                output_chars: content_chars,
                pre_trim_chars: pre_trim,
                // estimated_tokens = full output including hints (what LLM actually consumes)
                estimated_tokens: token_counter.count(&content),
                compression_ratio: if raw_chars > 0 {
                    content_chars as f32 / raw_chars as f32
                } else {
                    1.0
                },
                format: format_name.to_string(),
                truncated: t.truncated,
                total_items: t.total_count,
                included_items: t.included_count,
                chunked,
                total_chunks,
                chunk_number,
                provider_pagination: pag,
                provider_sort: sort,
                dedup_savings_pct: 0.0,
                encoder_savings_pct,
                combined_savings_pct,
                baseline: baseline_for_helper.clone(),
                tokenizer: tokenizer_for_helper.clone(),
            },
            content,
        }
    };

    // Helper: wrap plain text (no pipeline transform)
    let baseline_for_text = baseline.to_string();
    let tokenizer_for_text = tokenizer.to_string();
    let text_result =
        |text: String, pag: Option<Pagination>, sort: Option<SortInfo>| -> FormatResult {
            let chars = text.len();
            FormatResult {
                metadata: FormatMetadata {
                    raw_chars: chars,
                    output_chars: chars,
                    pre_trim_chars: chars,
                    estimated_tokens: token_counter.count(&text),
                    compression_ratio: 1.0,
                    format: "text".to_string(),
                    truncated: false,
                    total_items: None,
                    included_items: 0,
                    chunked: false,
                    total_chunks: 1,
                    chunk_number: 1,
                    provider_pagination: pag,
                    provider_sort: sort,
                    dedup_savings_pct: 0.0,
                    encoder_savings_pct: 0.0,
                    combined_savings_pct: 0.0,
                    baseline: baseline_for_text.clone(),
                    tokenizer: tokenizer_for_text.clone(),
                },
                content: text,
            }
        };

    match output {
        ToolOutput::Issues(issues, _) => Ok(to_result(
            pipeline.transform_issues(issues)?,
            provider_pagination,
            provider_sort,
        )),
        ToolOutput::SingleIssue(issue) => Ok(to_result(
            pipeline.transform_issues(vec![*issue])?,
            None,
            None,
        )),
        ToolOutput::MergeRequests(mrs, _) => Ok(to_result(
            pipeline.transform_merge_requests(mrs)?,
            provider_pagination,
            provider_sort,
        )),
        ToolOutput::SingleMergeRequest(mr) => Ok(to_result(
            pipeline.transform_merge_requests(vec![*mr])?,
            None,
            None,
        )),
        ToolOutput::Discussions(discussions, _) => Ok(to_result(
            pipeline.transform_discussions(discussions)?,
            provider_pagination,
            provider_sort,
        )),
        ToolOutput::Diffs(diffs, _) => Ok(to_result(
            pipeline.transform_diffs(diffs)?,
            provider_pagination,
            provider_sort,
        )),
        ToolOutput::Comments(comments, _) => Ok(to_result(
            pipeline.transform_comments(comments)?,
            provider_pagination,
            provider_sort,
        )),
        ToolOutput::Pipeline(info) => Ok(text_result(format_pipeline(&info), None, None)),
        ToolOutput::JobLog(log) => Ok(text_result(format_job_log(&log), None, None)),
        ToolOutput::Statuses(statuses, _) => Ok(text_result(
            format_statuses(&statuses),
            provider_pagination,
            provider_sort,
        )),
        ToolOutput::Users(users, _) => Ok(text_result(
            format_users(&users),
            provider_pagination,
            provider_sort,
        )),
        ToolOutput::MeetingNotes(meetings, _) => Ok(text_result(
            format_meeting_notes(&meetings),
            provider_pagination,
            provider_sort,
        )),
        ToolOutput::MeetingTranscript(transcript) => Ok(text_result(
            format_meeting_transcript(&transcript),
            None,
            None,
        )),
        ToolOutput::KnowledgeBaseSpaces(spaces, _) => Ok(text_result(
            format_knowledge_base_spaces(&spaces),
            provider_pagination,
            provider_sort,
        )),
        ToolOutput::KnowledgeBasePages(pages, _) => Ok(text_result(
            format_knowledge_base_pages(&pages),
            provider_pagination,
            provider_sort,
        )),
        ToolOutput::KnowledgeBasePageSummary(page) => {
            Ok(text_result(format_knowledge_base_page_summary(&page), None, None))
        }
        ToolOutput::KnowledgeBasePage(page) => {
            Ok(text_result(format_knowledge_base_page(&page), None, None))
        }
        ToolOutput::Relations(relations) => {
            let json = serde_json::to_string_pretty(&*relations).map_err(|e| {
                devboy_core::Error::InvalidData(format!("failed to serialize relations: {e}"))
            })?;
            Ok(text_result(json, None, None))
        }
        ToolOutput::MessengerChats(chats, _) => Ok(text_result(
            format_messenger_chats(&chats),
            provider_pagination,
            provider_sort,
        )),
        ToolOutput::MessengerMessages(messages, _) => Ok(text_result(
            format_messenger_messages(&messages),
            provider_pagination,
            provider_sort,
        )),
        ToolOutput::SingleMessage(message) => Ok(text_result(
            format_single_messenger_message(&message),
            None,
            None,
        )),
        ToolOutput::AssetList {
            attachments,
            count,
            capabilities,
        } => {
            let output = serde_json::json!({
                "attachments": attachments,
                "count": count,
                "capabilities": capabilities,
            });
            Ok(text_result(
                serde_json::to_string_pretty(&output).unwrap_or_default(),
                None,
                None,
            ))
        }
        ToolOutput::AssetDownloaded {
            asset_id,
            size,
            local_path,
            data,
            cached,
        } => {
            let output = serde_json::json!({
                "success": true,
                "asset_id": asset_id,
                "size": size,
                "local_path": local_path,
                "data": data,
                "cached": cached,
            });
            Ok(text_result(
                serde_json::to_string_pretty(&output).unwrap_or_default(),
                None,
                None,
            ))
        }
        ToolOutput::AssetUploaded {
            url,
            filename,
            size,
        } => {
            let output = serde_json::json!({
                "success": true,
                "url": url,
                "filename": filename,
                "size": size,
            });
            Ok(text_result(
                serde_json::to_string_pretty(&output).unwrap_or_default(),
                None,
                None,
            ))
        }
        ToolOutput::AssetDeleted { asset_id, message } => {
            let output = serde_json::json!({
                "success": true,
                "asset_id": asset_id,
                "message": message,
            });
            Ok(text_result(
                serde_json::to_string_pretty(&output).unwrap_or_default(),
                None,
                None,
            ))
        }
        // Jira Structure outputs — serialize as JSON. Match the
        // `Relations` branch above: surface serialisation errors as
        // `InvalidData` rather than silently emitting an empty body.
        ToolOutput::Structures(items, _meta) => {
            let json = serde_json::to_string_pretty(&items).map_err(|e| {
                devboy_core::Error::InvalidData(format!("failed to serialize structures: {e}"))
            })?;
            Ok(text_result(json, None, None))
        }
        ToolOutput::StructureForest(forest) => {
            let json = serde_json::to_string_pretty(&*forest).map_err(|e| {
                devboy_core::Error::InvalidData(format!(
                    "failed to serialize structure forest: {e}"
                ))
            })?;
            Ok(text_result(json, None, None))
        }
        ToolOutput::StructureValues(values) => {
            let json = serde_json::to_string_pretty(&*values).map_err(|e| {
                devboy_core::Error::InvalidData(format!(
                    "failed to serialize structure values: {e}"
                ))
            })?;
            Ok(text_result(json, None, None))
        }
        ToolOutput::StructureViews(views, _meta) => {
            let json = serde_json::to_string_pretty(&views).map_err(|e| {
                devboy_core::Error::InvalidData(format!("failed to serialize structure views: {e}"))
            })?;
            Ok(text_result(json, None, None))
        }
        ToolOutput::ForestModified(result) => {
            let json = serde_json::to_string_pretty(&result).map_err(|e| {
                devboy_core::Error::InvalidData(format!(
                    "failed to serialize forest modification result: {e}"
                ))
            })?;
            Ok(text_result(json, None, None))
        }
        ToolOutput::Text(text) => Ok(text_result(text, None, None)),
    }
}

/// Format messenger chats as readable text.
fn format_messenger_chats(chats: &[devboy_core::MessengerChat]) -> String {
    if chats.is_empty() {
        return "No chats found.".to_string();
    }

    let mut output = format!("# Messenger Chats ({})\n\n", chats.len());
    for chat in chats {
        let description = chat.description.as_deref().unwrap_or("-");
        let members = chat
            .member_count
            .map(|count| count.to_string())
            .unwrap_or_else(|| "-".to_string());
        let active = if chat.is_active { "active" } else { "inactive" };
        let chat_type = match chat.chat_type {
            devboy_core::types::ChatType::Direct => "direct",
            devboy_core::types::ChatType::Group => "group",
            devboy_core::types::ChatType::Channel => "channel",
        };
        output.push_str(&format!(
            "- {} [{}] id=`{}` members={} status={} desc={}\n",
            chat.name, chat_type, chat.id, members, active, description
        ));
    }
    output
}

/// Format messenger messages as readable text.
fn format_messenger_messages(messages: &[devboy_core::MessengerMessage]) -> String {
    if messages.is_empty() {
        return "No messages found.".to_string();
    }

    let mut output = format!("# Messages ({})\n\n", messages.len());
    for message in messages {
        output.push_str(&format_single_messenger_message(message));
        output.push('\n');
    }
    output
}

/// Format a single messenger message as one line.
fn format_single_messenger_message(message: &devboy_core::MessengerMessage) -> String {
    let text = message.text.replace('\r', "\\r").replace('\n', "\\n");
    let mut line = format!(
        "- [{}] {} ({}) in `{}`: {}",
        message.timestamp, message.author.name, message.author.id, message.chat_id, text
    );
    if let Some(thread_id) = message.thread_id.as_deref() {
        line.push_str(&format!(" thread=`{}`", thread_id));
    }
    if !message.attachments.is_empty() {
        line.push_str(&format!(" attachments={}", message.attachments.len()));
    }
    line
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

/// Format meeting notes as markdown.
fn format_meeting_notes(meetings: &[devboy_core::MeetingNote]) -> String {
    if meetings.is_empty() {
        return "No meeting notes found.".to_string();
    }

    let mut output = format!("# Meeting Notes ({} results)\n\n", meetings.len());

    for m in meetings {
        output.push_str(&format!("## {}\n", m.title));
        if let Some(ref date) = m.meeting_date {
            output.push_str(&format!("**Date:** {date}\n"));
        }
        if let Some(secs) = m.duration_seconds {
            let mins = secs / 60;
            output.push_str(&format!("**Duration:** {mins} min\n"));
        }
        if let Some(ref host) = m.host_email {
            output.push_str(&format!("**Host:** {host}\n"));
        }
        if !m.participants.is_empty() {
            output.push_str(&format!(
                "**Participants:** {}\n",
                m.participants.join(", ")
            ));
        }
        if let Some(ref summary) = m.summary {
            output.push_str(&format!("\n{summary}\n"));
        }
        if !m.action_items.is_empty() {
            output.push_str("\n**Action Items:**\n");
            for item in &m.action_items {
                output.push_str(&format!("- {item}\n"));
            }
        }
        if !m.keywords.is_empty() {
            output.push_str(&format!("**Keywords:** {}\n", m.keywords.join(", ")));
        }
        output.push('\n');
    }

    output
}

/// Format a meeting transcript as compact text.
fn format_meeting_transcript(transcript: &devboy_core::MeetingTranscript) -> String {
    let title = transcript.title.as_deref().unwrap_or("Meeting Transcript");
    let mut output = format!("# {title}\n\n");
    output.push_str(&format!(
        "Showing {} sentences\n\n",
        transcript.sentences.len()
    ));

    for s in &transcript.sentences {
        let fallback = if s.speaker_id.is_empty() {
            "Unknown speaker".to_string()
        } else {
            format!("Speaker {}", s.speaker_id)
        };
        let speaker = s.speaker_name.as_deref().unwrap_or(&fallback);
        let time = format_time(s.start_time);
        output.push_str(&format!("[{time}] {speaker}: {}\n", s.text));
    }

    output
}

fn format_knowledge_base_spaces(spaces: &[devboy_core::KbSpace]) -> String {
    if spaces.is_empty() {
        return "No knowledge base spaces found.".to_string();
    }

    let mut output = format!("# Knowledge Base Spaces ({})\n\n", spaces.len());
    for space in spaces {
        output.push_str(&format!("- {} (`{}`)\n", space.name, space.key));
        if let Some(description) = &space.description {
            output.push_str(&format!("  {description}\n"));
        }
        if let Some(url) = &space.url {
            output.push_str(&format!("  {url}\n"));
        }
    }
    output
}

fn format_knowledge_base_pages(pages: &[devboy_core::KbPage]) -> String {
    if pages.is_empty() {
        return "No knowledge base pages found.".to_string();
    }

    let mut output = format!("# Knowledge Base Pages ({})\n\n", pages.len());
    for page in pages {
        output.push_str(&format!("- {} (`{}`)\n", page.title, page.id));
        if let Some(space_key) = &page.space_key {
            output.push_str(&format!("  space: {space_key}\n"));
        }
        if let Some(author) = &page.author {
            output.push_str(&format!("  author: {author}\n"));
        }
        if let Some(last_modified) = &page.last_modified {
            output.push_str(&format!("  updated: {last_modified}\n"));
        }
        if let Some(excerpt) = &page.excerpt {
            output.push_str(&format!("  excerpt: {excerpt}\n"));
        }
        if let Some(url) = &page.url {
            output.push_str(&format!("  {url}\n"));
        }
    }
    output
}

fn format_knowledge_base_page_summary(page: &devboy_core::KbPage) -> String {
    let mut output = format!("# Knowledge Base Page\n\n{} (`{}`)\n", page.title, page.id);
    if let Some(space_key) = &page.space_key {
        output.push_str(&format!("space: {space_key}\n"));
    }
    if let Some(author) = &page.author {
        output.push_str(&format!("author: {author}\n"));
    }
    if let Some(last_modified) = &page.last_modified {
        output.push_str(&format!("updated: {last_modified}\n"));
    }
    if let Some(url) = &page.url {
        output.push_str(&format!("url: {url}\n"));
    }
    output
}

fn format_knowledge_base_page(page: &devboy_core::KbPageContent) -> String {
    let mut output = format!("# {}\n\n", page.page.title);
    output.push_str(&format!("id: `{}`\n", page.page.id));
    if let Some(space_key) = &page.page.space_key {
        output.push_str(&format!("space: `{space_key}`\n"));
    }
    output.push_str(&format!("content_type: `{}`\n", page.content_type));
    if !page.labels.is_empty() {
        output.push_str(&format!("labels: {}\n", page.labels.join(", ")));
    }
    if !page.ancestors.is_empty() {
        let chain = page
            .ancestors
            .iter()
            .map(|ancestor| ancestor.title.as_str())
            .collect::<Vec<_>>()
            .join(" > ");
        output.push_str(&format!("ancestors: {chain}\n"));
    }
    if let Some(url) = &page.page.url {
        output.push_str(&format!("url: {url}\n"));
    }
    output.push('\n');
    output.push_str(&page.content);
    output
}

/// Format seconds as [MM:SS] or [HH:MM:SS].
fn format_time(seconds: f64) -> String {
    let total_secs = seconds as u64;
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes:02}:{secs:02}")
    }
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
        &info.sha[..info
            .sha
            .char_indices()
            .nth(7)
            .map(|(i, _)| i)
            .unwrap_or(info.sha.len())]
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
) -> Result<FormatResult> {
    // Extract format and budget from args before execution
    let format = args
        .get("format")
        .and_then(|v| v.as_str())
        .map(String::from);

    let budget = args
        .get("budget")
        .and_then(|v| v.as_u64())
        .map(|b| b as usize);

    // Apply budget override to pipeline config
    let pipeline_config = if let Some(b) = budget {
        let mut config = pipeline_config.unwrap_or_default();
        // Convert token budget to max_chars (tokens * 3.5)
        config.max_chars = (b as f64 * 3.5).floor() as usize;
        Some(config)
    } else {
        pipeline_config
    };

    let output = executor.execute(tool, args, ctx).await?;
    format_output(output, format.as_deref(), Some(tool), pipeline_config)
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
            attachments_count: None,
            parent: None,
            subtasks: vec![],
        }
    }

    #[test]
    fn test_format_issues_toon() {
        let output = ToolOutput::Issues(vec![sample_issue()], None);
        let result = format_output(output, Some("toon"), None, None)
            .unwrap()
            .content;
        assert!(result.contains("gh#1"));
        assert!(result.contains("Test Issue"));
    }

    #[test]
    fn test_format_metadata_toon_compression() {
        let output = ToolOutput::Issues(vec![sample_issue()], None);
        let result = format_output(output, Some("toon"), None, None).unwrap();

        assert!(result.metadata.raw_chars > 0, "raw_chars should be > 0");
        assert!(
            result.metadata.output_chars > 0,
            "output_chars should be > 0"
        );
        assert!(result.metadata.estimated_tokens > 0, "tokens should be > 0");
        assert_eq!(result.metadata.format, "toon");
        assert!(!result.metadata.truncated);
        // Compression ratio should be reasonable (TOON may slightly expand very small inputs)
        assert!(
            result.metadata.compression_ratio < 2.0,
            "compression_ratio should be reasonable, got {}",
            result.metadata.compression_ratio
        );
    }

    #[test]
    fn test_format_metadata_text_passthrough() {
        let output = ToolOutput::Text("plain text".into());
        let result = format_output(output, None, None, None).unwrap();

        assert_eq!(result.metadata.raw_chars, 10);
        assert_eq!(result.metadata.output_chars, 10);
        assert_eq!(result.metadata.compression_ratio, 1.0);
        assert_eq!(result.metadata.format, "text");
        assert!(!result.metadata.truncated);
    }

    #[test]
    fn test_format_metadata_savings_split() {
        // Multi-issue payload so the encoder actually compresses below
        // the JSON pretty baseline.
        let issues: Vec<_> = (0..20).map(|_| sample_issue()).collect();
        let output = ToolOutput::Issues(issues, None);
        let result = format_output(output, Some("toon"), None, None).unwrap();

        // Typed-domain path: dedup contributes nothing here.
        assert_eq!(result.metadata.dedup_savings_pct, 0.0);
        // Encoder savings must be in [0, 1).
        assert!(
            (0.0..1.0).contains(&result.metadata.encoder_savings_pct),
            "encoder savings out of range: {}",
            result.metadata.encoder_savings_pct
        );
        // Combined == encoder when dedup is zero.
        assert_eq!(
            result.metadata.combined_savings_pct,
            result.metadata.encoder_savings_pct
        );
        // §Savings Accounting demands a named baseline + tokenizer.
        assert_eq!(result.metadata.baseline, "json_pretty");
        assert!(
            !result.metadata.tokenizer.is_empty(),
            "tokenizer must be set"
        );
    }

    #[test]
    fn test_format_metadata_passthrough_savings_zero() {
        // Plain-text passthrough has no encoder hop, so all three savings
        // must be zero — but baseline / tokenizer still populate.
        let output = ToolOutput::Text("nothing to compress".into());
        let result = format_output(output, None, None, None).unwrap();
        assert_eq!(result.metadata.dedup_savings_pct, 0.0);
        assert_eq!(result.metadata.encoder_savings_pct, 0.0);
        assert_eq!(result.metadata.combined_savings_pct, 0.0);
        assert_eq!(result.metadata.baseline, "json_pretty");
        assert!(!result.metadata.tokenizer.is_empty());
    }

    #[test]
    fn test_format_metadata_truncated() {
        let output = ToolOutput::Issues(vec![sample_issue()], None);
        let config = PipelineConfig {
            max_chars: 50, // very small — will truncate
            ..PipelineConfig::default()
        };
        let result = format_output(output, Some("toon"), None, Some(config)).unwrap();

        assert!(result.metadata.truncated);
        // output_chars tracks content size (may include hint text appended after truncation)
        assert!(
            result.metadata.output_chars < result.metadata.raw_chars,
            "truncated output ({}) should be smaller than raw ({})",
            result.metadata.output_chars,
            result.metadata.raw_chars
        );
    }

    #[test]
    fn test_format_issues_json() {
        let output = ToolOutput::Issues(vec![sample_issue()], None);
        let result = format_output(output, Some("json"), None, None)
            .unwrap()
            .content;
        assert!(result.contains("gh#1"));
    }

    #[test]
    fn test_format_issues_toon_explicit() {
        let output = ToolOutput::Issues(vec![sample_issue()], None);
        let result = format_output(output, Some("toon"), None, None)
            .unwrap()
            .content;
        assert!(result.contains("gh#1"));
    }

    #[test]
    fn test_format_text_passthrough() {
        let output = ToolOutput::Text("Comment created".into());
        let result = format_output(output, None, None, None).unwrap().content;
        assert_eq!(result, "Comment created");
    }

    #[test]
    fn test_format_default_is_toon() {
        let output = ToolOutput::Issues(vec![sample_issue()], None);
        let result = format_output(output, None, None, None).unwrap().content;
        assert!(result.contains("gh#1"));
    }

    #[test]
    fn test_format_single_issue() {
        let output = ToolOutput::SingleIssue(Box::new(sample_issue()));
        let result = format_output(output, Some("toon"), None, None)
            .unwrap()
            .content;
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
        let output = ToolOutput::MergeRequests(vec![sample_mr()], None);
        let result = format_output(output, Some("toon"), None, None)
            .unwrap()
            .content;
        assert!(result.contains("pr#1"));
    }

    #[test]
    fn test_format_single_merge_request() {
        let output = ToolOutput::SingleMergeRequest(Box::new(sample_mr()));
        let result = format_output(output, Some("toon"), None, None)
            .unwrap()
            .content;
        assert!(result.contains("pr#1"));
    }

    #[test]
    fn test_format_discussions() {
        let output = ToolOutput::Discussions(
            vec![devboy_core::Discussion {
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
            }],
            None,
        );
        let result = format_output(output, Some("toon"), None, None)
            .unwrap()
            .content;
        assert!(result.contains("Review comment"));
    }

    #[test]
    fn test_format_diffs() {
        let output = ToolOutput::Diffs(
            vec![devboy_core::FileDiff {
                file_path: "src/main.rs".into(),
                old_path: None,
                new_file: false,
                deleted_file: false,
                renamed_file: false,
                diff: "+added line".into(),
                additions: Some(1),
                deletions: Some(0),
            }],
            None,
        );
        let result = format_output(output, Some("toon"), None, None)
            .unwrap()
            .content;
        assert!(result.contains("src/main.rs"));
    }

    #[test]
    fn test_format_comments() {
        let output = ToolOutput::Comments(
            vec![devboy_core::Comment {
                id: "c1".into(),
                body: "A comment body".into(),
                author: None,
                created_at: None,
                updated_at: None,
                position: None,
            }],
            None,
        );
        let result = format_output(output, Some("json"), None, None)
            .unwrap()
            .content;
        assert!(result.contains("A comment body"));
    }

    #[test]
    fn test_format_with_custom_pipeline_config() {
        let output = ToolOutput::Issues(vec![sample_issue()], None);
        let config = PipelineConfig {
            max_chars: 500,
            ..PipelineConfig::default()
        };
        let result = format_output(output, Some("toon"), None, Some(config))
            .unwrap()
            .content;
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
        let result = format_output(output, None, None, None).unwrap().content;
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
        let result = format_output(output, None, None, None).unwrap().content;
        assert!(result.contains("Job Log"));
        assert!(result.contains("202"));
        assert!(result.contains("smart"));
        assert!(result.contains("assertion failed"));
    }

    // --- Pipeline formatting ---

    #[test]
    fn test_format_pipeline_success_status() {
        let output = ToolOutput::Pipeline(Box::new(devboy_core::PipelineInfo {
            id: "200".into(),
            status: devboy_core::PipelineStatus::Success,
            reference: "develop".into(),
            sha: "deadbeefcafe".into(),
            url: None,
            duration: None,
            coverage: None,
            summary: devboy_core::PipelineSummary {
                total: 5,
                success: 5,
                ..Default::default()
            },
            stages: vec![],
            failed_jobs: vec![],
        }));
        let result = format_output(output, None, None, None).unwrap().content;
        assert!(result.contains("Pipeline 200"));
        assert!(result.contains("success"));
        assert!(result.contains("develop"));
        assert!(result.contains("deadbee")); // sha truncated to 7
    }

    #[test]
    fn test_format_pipeline_running_status() {
        let output = ToolOutput::Pipeline(Box::new(devboy_core::PipelineInfo {
            id: "301".into(),
            status: devboy_core::PipelineStatus::Running,
            reference: "feature".into(),
            sha: "1234567890abcdef".into(),
            url: Some("https://ci.example.com/301".into()),
            duration: Some(60),
            coverage: None,
            summary: devboy_core::PipelineSummary {
                total: 3,
                running: 1,
                success: 1,
                pending: 1,
                ..Default::default()
            },
            stages: vec![],
            failed_jobs: vec![],
        }));
        let result = format_output(output, None, None, None).unwrap().content;
        assert!(result.contains("running"));
        assert!(result.contains("https://ci.example.com/301"));
        assert!(result.contains("60s"));
    }

    #[test]
    fn test_format_pipeline_pending_status() {
        let output = ToolOutput::Pipeline(Box::new(devboy_core::PipelineInfo {
            id: "302".into(),
            status: devboy_core::PipelineStatus::Pending,
            reference: "main".into(),
            sha: "aabbccdd".into(),
            url: None,
            duration: None,
            coverage: None,
            summary: Default::default(),
            stages: vec![],
            failed_jobs: vec![],
        }));
        let result = format_output(output, None, None, None).unwrap().content;
        assert!(result.contains("pending"));
    }

    #[test]
    fn test_format_pipeline_canceled_status() {
        let output = ToolOutput::Pipeline(Box::new(devboy_core::PipelineInfo {
            id: "303".into(),
            status: devboy_core::PipelineStatus::Canceled,
            reference: "main".into(),
            sha: "1122334455".into(),
            url: None,
            duration: None,
            coverage: None,
            summary: Default::default(),
            stages: vec![],
            failed_jobs: vec![],
        }));
        let result = format_output(output, None, None, None).unwrap().content;
        assert!(result.contains("canceled"));
    }

    #[test]
    fn test_format_pipeline_with_job_url() {
        let output = ToolOutput::Pipeline(Box::new(devboy_core::PipelineInfo {
            id: "400".into(),
            status: devboy_core::PipelineStatus::Failed,
            reference: "main".into(),
            sha: "abcdef1234567".into(),
            url: None,
            duration: None,
            coverage: None,
            summary: Default::default(),
            stages: vec![devboy_core::PipelineStage {
                name: "test".into(),
                jobs: vec![devboy_core::PipelineJob {
                    id: "j1".into(),
                    name: "unit-test".into(),
                    status: devboy_core::PipelineStatus::Failed,
                    url: Some("https://ci.example.com/jobs/j1".into()),
                    duration: None,
                }],
            }],
            failed_jobs: vec![],
        }));
        let result = format_output(output, None, None, None).unwrap().content;
        assert!(result.contains("[logs](https://ci.example.com/jobs/j1)"));
    }

    #[test]
    fn test_format_pipeline_failed_job_without_snippet() {
        let output = ToolOutput::Pipeline(Box::new(devboy_core::PipelineInfo {
            id: "401".into(),
            status: devboy_core::PipelineStatus::Failed,
            reference: "main".into(),
            sha: "abcdef1234567".into(),
            url: None,
            duration: None,
            coverage: None,
            summary: Default::default(),
            stages: vec![],
            failed_jobs: vec![devboy_core::FailedJob {
                id: "fj1".into(),
                name: "lint".into(),
                url: None,
                error_snippet: None,
            }],
        }));
        let result = format_output(output, None, None, None).unwrap().content;
        assert!(result.contains("lint"));
        assert!(result.contains("fj1"));
        assert!(!result.contains("```")); // no code block when no snippet
    }

    // --- Statuses formatting ---

    #[test]
    fn test_format_statuses() {
        let output = ToolOutput::Statuses(
            vec![
                devboy_core::IssueStatus {
                    id: "1".into(),
                    name: "To Do".into(),
                    category: "todo".into(),
                    color: Some("#blue".into()),
                    order: Some(0),
                },
                devboy_core::IssueStatus {
                    id: "2".into(),
                    name: "In Progress".into(),
                    category: "in_progress".into(),
                    color: None,
                    order: None,
                },
            ],
            None,
        );
        let result = format_output(output, None, None, None).unwrap().content;
        assert!(result.contains("Available Statuses"));
        assert!(result.contains("To Do"));
        assert!(result.contains("In Progress"));
        assert!(result.contains("#blue"));
        assert!(result.contains("todo"));
        assert!(result.contains("| - |")); // None values become "-"
    }

    #[test]
    fn test_format_statuses_empty() {
        let output = ToolOutput::Statuses(vec![], None);
        let result = format_output(output, None, None, None).unwrap().content;
        assert_eq!(result, "No statuses found.");
    }

    // --- Users formatting ---

    #[test]
    fn test_format_users() {
        let output = ToolOutput::Users(
            vec![
                devboy_core::User {
                    id: "u1".into(),
                    username: "johndoe".into(),
                    name: Some("John Doe".into()),
                    email: Some("john@example.com".into()),
                    avatar_url: None,
                },
                devboy_core::User {
                    id: "u2".into(),
                    username: "janesmith".into(),
                    name: None,
                    email: None,
                    avatar_url: None,
                },
            ],
            None,
        );
        let result = format_output(output, None, None, None).unwrap().content;
        assert!(result.contains("# Users"));
        assert!(result.contains("johndoe"));
        assert!(result.contains("John Doe"));
        assert!(result.contains("john@example.com"));
        assert!(result.contains("janesmith"));
        assert!(result.contains("| - |")); // None values
    }

    #[test]
    fn test_format_users_empty() {
        let output = ToolOutput::Users(vec![], None);
        let result = format_output(output, None, None, None).unwrap().content;
        assert_eq!(result, "No users found.");
    }

    // --- JobLog without total_lines ---

    #[test]
    fn test_format_job_log_no_total_lines() {
        let output = ToolOutput::JobLog(Box::new(devboy_core::JobLogOutput {
            job_id: "999".into(),
            job_name: Some("build".into()),
            content: "Building...".into(),
            mode: "full".into(),
            total_lines: None,
        }));
        let result = format_output(output, None, None, None).unwrap().content;
        assert!(result.contains("Job Log (999)"));
        assert!(result.contains("**Mode:** full"));
        assert!(!result.contains("Total lines"));
        assert!(result.contains("Building..."));
    }

    // --- Text passthrough variations ---

    #[test]
    fn test_format_text_empty_string() {
        let output = ToolOutput::Text("".into());
        let result = format_output(output, None, None, None).unwrap().content;
        assert_eq!(result, "");
    }

    #[test]
    fn test_format_text_with_json_format_param() {
        // Even with "json" format, Text variant just passes through
        let output = ToolOutput::Text("raw text".into());
        let result = format_output(output, Some("json"), None, None)
            .unwrap()
            .content;
        assert_eq!(result, "raw text");
    }

    // --- Meeting notes formatting ---

    #[test]
    fn test_format_meeting_notes() {
        let meetings = vec![devboy_core::MeetingNote {
            id: "m1".into(),
            title: "Sprint Planning".into(),
            meeting_date: Some("2025-01-15T10:00:00Z".into()),
            duration_seconds: Some(2700), // 45 min
            host_email: Some("host@example.com".into()),
            participants: vec!["alice@example.com".into(), "bob@example.com".into()],
            action_items: vec!["Review PR #42".into(), "Update docs".into()],
            keywords: vec!["sprint".into(), "planning".into()],
            summary: Some("Discussed sprint goals.".into()),
            ..Default::default()
        }];
        let output = ToolOutput::MeetingNotes(meetings, None);
        let result = format_output(output, None, None, None).unwrap().content;
        assert!(result.contains("Sprint Planning"));
        assert!(result.contains("2025-01-15T10:00:00Z"));
        assert!(result.contains("45 min"));
        assert!(result.contains("host@example.com"));
        assert!(result.contains("alice@example.com"));
        assert!(result.contains("Review PR #42"));
        assert!(result.contains("Update docs"));
        assert!(result.contains("sprint"));
        assert!(result.contains("Discussed sprint goals."));
    }

    #[test]
    fn test_format_meeting_notes_empty() {
        let output = ToolOutput::MeetingNotes(vec![], None);
        let result = format_output(output, None, None, None).unwrap().content;
        assert_eq!(result, "No meeting notes found.");
    }

    #[test]
    fn test_format_meeting_transcript() {
        let transcript = devboy_core::MeetingTranscript {
            meeting_id: "m1".into(),
            title: Some("Sprint Planning".into()),
            sentences: vec![
                devboy_core::TranscriptSentence {
                    speaker_id: "s1".into(),
                    speaker_name: Some("Alice".into()),
                    text: "Let's start the meeting.".into(),
                    start_time: 0.0,
                    end_time: 3.0,
                },
                devboy_core::TranscriptSentence {
                    speaker_id: "s2".into(),
                    speaker_name: Some("Bob".into()),
                    text: "Sounds good.".into(),
                    start_time: 5.0,
                    end_time: 7.0,
                },
            ],
        };
        let output = ToolOutput::MeetingTranscript(Box::new(transcript));
        let result = format_output(output, None, None, None).unwrap().content;
        assert!(result.contains("Sprint Planning"));
        assert!(result.contains("2 sentences"));
        assert!(result.contains("[00:00] Alice: Let's start the meeting."));
        assert!(result.contains("[00:05] Bob: Sounds good."));
    }

    #[test]
    fn test_format_meeting_transcript_unknown_speaker() {
        let transcript = devboy_core::MeetingTranscript {
            meeting_id: "m1".into(),
            title: None,
            sentences: vec![devboy_core::TranscriptSentence {
                speaker_id: "".into(),
                speaker_name: None,
                text: "Hello".into(),
                start_time: 0.0,
                end_time: 1.0,
            }],
        };
        let output = ToolOutput::MeetingTranscript(Box::new(transcript));
        let result = format_output(output, None, None, None).unwrap().content;
        assert!(result.contains("Meeting Transcript"));
        assert!(result.contains("Unknown speaker"));
    }

    // --- Relations formatting ---

    #[test]
    fn test_format_relations() {
        let relations = devboy_core::IssueRelations {
            parent: Some(sample_issue()),
            subtasks: vec![sample_issue()],
            blocks: vec![devboy_core::IssueLink {
                issue: sample_issue(),
                link_type: "Blocks".into(),
            }],
            blocked_by: vec![],
            related_to: vec![],
            duplicates: vec![],
        };
        let output = ToolOutput::Relations(Box::new(relations));
        let result = format_output(output, None, None, None).unwrap().content;
        // Relations format uses JSON serialization
        assert!(result.contains("gh#1"));
        assert!(result.contains("Blocks"));
        assert!(result.contains("Test Issue"));
    }

    #[test]
    fn test_format_relations_empty() {
        let relations = devboy_core::IssueRelations::default();
        let output = ToolOutput::Relations(Box::new(relations));
        let result = format_output(output, None, None, None).unwrap().content;
        // Empty relations should still produce valid JSON
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.is_object());
    }

    // --- format_time edge cases ---

    #[test]
    fn test_format_time_zero() {
        assert_eq!(format_time(0.0), "00:00");
    }

    #[test]
    fn test_format_time_seconds_only() {
        assert_eq!(format_time(45.0), "00:45");
    }

    #[test]
    fn test_format_time_minutes_and_seconds() {
        assert_eq!(format_time(125.0), "02:05");
    }

    #[test]
    fn test_format_time_hours() {
        assert_eq!(format_time(3661.0), "01:01:01");
    }

    #[test]
    fn test_format_time_fractional_seconds() {
        // Fractional seconds are truncated
        assert_eq!(format_time(59.9), "00:59");
    }
}
