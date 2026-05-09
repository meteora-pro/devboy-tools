//! Asset management types for file attachments and content analysis.
//!
//! These types are shared across the provider layer and the `devboy-assets`
//! crate, providing a unified abstraction for working with attached files
//! (screenshots, logs, configs, etc.) across different providers.
//!
//! See ADR-010 for the full design rationale.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// =============================================================================
// AssetContext
// =============================================================================

/// Context to which an asset is attached.
///
/// Different providers support attachments in different contexts — an issue
/// body, an issue comment, a merge request, etc. This enum captures all the
/// supported targets in a provider-agnostic way.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssetContext {
    /// Attachment on an issue body/description.
    Issue {
        /// Issue key (e.g. "DEV-123", "gitlab#42").
        key: String,
    },
    /// Attachment on a comment under an issue.
    IssueComment {
        key: String,
        /// Comment identifier within the issue.
        comment_id: String,
    },
    /// Attachment on a merge request / pull request body.
    MergeRequest {
        /// MR / PR identifier. Named `mr_id` for consistency with
        /// [`AssetContext::MrComment`] so JSON-wire field names are the
        /// same across both variants.
        mr_id: String,
    },
    /// Attachment on a comment/note of a merge request.
    MrComment {
        mr_id: String,
        note_id: String,
    },
    /// Attachment from a messenger chat (Slack, Telegram, etc.).
    Chat {
        chat_id: String,
        message_id: String,
    },
    /// Attachment from a knowledge base page (Confluence, etc.).
    KbPage {
        page_id: String,
    },
}

impl AssetContext {
    /// Short colon-separated string for logging and debugging.
    ///
    /// **Note:** The cache directory layout is handled by
    /// `devboy_assets::CacheManager::dir_for` / `path_for` — this method
    /// is intentionally *not* used for on-disk paths.
    ///
    /// Examples:
    /// - `issue:DEV-123`
    /// - `mr:42`
    /// - `chat:C0123ABC:msg42`
    pub fn slug(&self) -> String {
        match self {
            AssetContext::Issue { key } => format!("issue:{key}"),
            AssetContext::IssueComment { key, comment_id } => {
                format!("issue:{key}:comment:{comment_id}")
            }
            AssetContext::MergeRequest { mr_id } => format!("mr:{mr_id}"),
            AssetContext::MrComment { mr_id, note_id } => format!("mr:{mr_id}:note:{note_id}"),
            AssetContext::Chat {
                chat_id,
                message_id,
            } => format!("chat:{chat_id}:msg:{message_id}"),
            AssetContext::KbPage { page_id } => format!("kb:{page_id}"),
        }
    }

    /// Kind of the context (category used in enrichment and capabilities).
    pub fn kind(&self) -> AssetContextKind {
        match self {
            AssetContext::Issue { .. } => AssetContextKind::Issue,
            AssetContext::IssueComment { .. } => AssetContextKind::IssueComment,
            AssetContext::MergeRequest { .. } => AssetContextKind::MergeRequest,
            AssetContext::MrComment { .. } => AssetContextKind::MrComment,
            AssetContext::Chat { .. } => AssetContextKind::Chat,
            AssetContext::KbPage { .. } => AssetContextKind::KbPage,
        }
    }
}

/// Category of an [`AssetContext`] — used for capability lookup.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AssetContextKind {
    /// Issue body / description.
    Issue,
    IssueComment,
    /// Merge request / pull request body.
    MergeRequest,
    /// Comment / note on a merge request.
    MrComment,
    /// Messenger chat message.
    Chat,
    /// Knowledge base page.
    KbPage,
}

// =============================================================================
// AssetMeta / AssetInput
// =============================================================================

/// Metadata describing an asset — used for listings and enriched responses.
///
/// This type intentionally does NOT contain the file bytes; use
/// [`AssetInput`] for uploads or a dedicated download method to fetch content.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AssetMeta {
    /// Stable identifier for the asset within devboy (UUID or provider id).
    pub id: String,
    pub filename: String,
    /// MIME type (best-effort; may be `None` for unknown binaries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// File size in bytes if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Remote URL at the provider (if available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Creation timestamp (ISO 8601).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Username / display name of the uploader if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Whether the file is currently present in the local cache.
    #[serde(default)]
    pub cached: bool,
    /// Absolute local path if the file is cached locally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    /// SHA-256 checksum of the content if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checksum_sha256: Option<String>,
    /// Result of analysis (Levels 1-2 built-in or Level 3 semantic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis: Option<AssetAnalysis>,
}

/// Input data for uploading a new asset.
///
/// This type is part of the public `devboy_core::asset` API and is
/// (de)serializable so it can cross crate and MCP tool boundaries. File
/// bytes go through serde's default `Vec<u8>` encoding, which is a JSON
/// array of numbers — MCP tools typically base64-encode the payload in a
/// wrapper struct rather than serializing this type directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetInput {
    /// Filename to use on the provider side.
    pub filename: String,
    /// Raw file bytes.
    pub data: Vec<u8>,
    /// Optional MIME type hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

impl AssetInput {
    /// Create a new input descriptor.
    pub fn new(filename: impl Into<String>, data: Vec<u8>) -> Self {
        Self {
            filename: filename.into(),
            data,
            mime_type: None,
        }
    }

    /// Attach a MIME type hint to the input.
    pub fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }
}

// =============================================================================
// Capabilities
// =============================================================================

/// Per-provider capability matrix for asset operations.
///
/// Each provider declares which CRUD operations it supports for each
/// context kind. The values are used by the enricher to generate
/// `asset_capabilities` entries in tool schemas so that agents can see in
/// advance what operations are available.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AssetCapabilities {
    /// Capabilities for issue bodies.
    #[serde(default)]
    pub issue: ContextCapabilities,
    #[serde(default)]
    pub issue_comment: ContextCapabilities,
    /// Capabilities for merge request bodies.
    #[serde(default)]
    pub merge_request: ContextCapabilities,
    #[serde(default)]
    pub mr_comment: ContextCapabilities,
}

impl AssetCapabilities {
    /// Return the capabilities for a given context kind.
    ///
    /// Chat / KB contexts are out of scope for the initial provider set —
    /// they return a shared empty capability set.
    pub fn for_kind(&self, kind: AssetContextKind) -> &ContextCapabilities {
        match kind {
            AssetContextKind::Issue => &self.issue,
            AssetContextKind::IssueComment => &self.issue_comment,
            AssetContextKind::MergeRequest => &self.merge_request,
            AssetContextKind::MrComment => &self.mr_comment,
            AssetContextKind::Chat | AssetContextKind::KbPage => empty_context_capabilities(),
        }
    }
}

/// Shared sentinel used for unsupported context kinds.
fn empty_context_capabilities() -> &'static ContextCapabilities {
    static EMPTY: std::sync::OnceLock<ContextCapabilities> = std::sync::OnceLock::new();
    EMPTY.get_or_init(ContextCapabilities::default)
}

/// CRUD capabilities for a single context kind.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ContextCapabilities {
    /// Whether uploading new attachments is supported.
    #[serde(default)]
    pub upload: bool,
    /// Whether downloading attachments is supported.
    #[serde(default)]
    pub download: bool,
    /// Whether deleting attachments is supported.
    #[serde(default)]
    pub delete: bool,
    /// Whether listing attachments is supported.
    #[serde(default)]
    pub list: bool,
    /// Max file size in bytes, if the provider advertises a limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_size: Option<u64>,
    /// Allowed MIME type patterns (e.g. `image/*`). Empty means any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_types: Vec<String>,
}

impl ContextCapabilities {
    /// Convenience: all operations enabled with no type restrictions.
    pub fn full() -> Self {
        Self {
            upload: true,
            download: true,
            delete: true,
            list: true,
            max_file_size: None,
            allowed_types: Vec::new(),
        }
    }

    /// Convenience: read-only (download + list).
    pub fn read_only() -> Self {
        Self {
            upload: false,
            download: true,
            delete: false,
            list: true,
            max_file_size: None,
            allowed_types: Vec::new(),
        }
    }
}

// =============================================================================
// AssetAnalysis
// =============================================================================

/// Result of analyzing an asset through the processor pipeline.
///
/// Produced by Levels 1-2 (built-in processors, no LLM) and optionally
/// enriched by Level 3 (semantic LLM analysis).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AssetAnalysis {
    /// Short human-readable summary for the agent (1-3 sentences).
    pub summary: String,
    pub content_kind: ContentKind,
    /// Text extracted from the file if applicable (logs, configs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extractable_text: Option<String>,
    /// Key findings produced by built-in heuristics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key_findings: Vec<String>,
    /// Additional metadata (dimensions, duration, line counts, ...).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,
    /// Level 3 semantic analysis result, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic: Option<SemanticAnalysis>,
}

/// Result of a Level 3 semantic (LLM-based) analysis.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SemanticAnalysis {
    /// Summary produced by the LLM.
    pub summary: String,
    /// Key findings identified by the LLM.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<String>,
    /// Prompt used for the analysis (for caching and debugging).
    pub prompt_used: String,
    /// Model identifier used (e.g. "claude-sonnet-4").
    pub model: String,
    /// Whether this result was served from cache.
    #[serde(default)]
    pub cached: bool,
}

/// High-level kind of content stored in an asset.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    /// Text-based content (logs, plain text, source code).
    Text,
    /// Raster or vector image.
    Image,
    Video,
    /// Document file (PDF, DOCX, ...).
    Document,
    /// Structured data (CSV, XLSX, JSON, YAML).
    Data,
    /// Binary content of an unknown kind.
    #[default]
    Binary,
}

// =============================================================================
// Markdown parsing helpers
// =============================================================================

/// Extract attachments embedded in a markdown string.
///
/// Recognizes both image syntax (`![alt](url)`) and link syntax
/// (`[text](url)`). The result is deduplicated by URL and returned in the
/// order the references appear in the source. Inputs without any markdown
/// links produce an empty vector.
///
/// This helper is used by providers like GitLab and GitHub that embed
/// attachments directly into issue / MR bodies and comments rather than
/// exposing a dedicated attachments API.
///
/// **No filtering is applied** — every `[text](url)` and `![alt](url)`
/// reference is returned, including plain web links. Callers that only
/// want downloadable files should filter by scheme, host, or file
/// extension as appropriate for their provider. The extracted `filename`
/// is derived from the markdown alt text / link text when available and
/// falls back to the final path segment of the URL.
pub fn parse_markdown_attachments(markdown: &str) -> Vec<MarkdownAttachment> {
    let mut out: Vec<MarkdownAttachment> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let bytes = markdown.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Look for `[` (link) or `![` (image).
        let is_image = i + 1 < bytes.len() && bytes[i] == b'!' && bytes[i + 1] == b'[';
        let is_link = bytes[i] == b'[';
        if !is_image && !is_link {
            i += 1;
            continue;
        }

        let text_start = if is_image { i + 2 } else { i + 1 };
        let Some(text_end_rel) = find_matching(&bytes[text_start..], b'[', b']') else {
            i += 1;
            continue;
        };
        let text_end = text_start + text_end_rel;

        // Must be immediately followed by `(`.
        if text_end + 1 >= bytes.len() || bytes[text_end + 1] != b'(' {
            i = text_end + 1;
            continue;
        }
        let url_start = text_end + 2;
        let Some(url_end_rel) = find_matching(&bytes[url_start..], b'(', b')') else {
            i = text_end + 1;
            continue;
        };
        let url_end = url_start + url_end_rel;

        let text = std::str::from_utf8(&bytes[text_start..text_end])
            .unwrap_or("")
            .trim()
            .to_string();
        let url_raw = std::str::from_utf8(&bytes[url_start..url_end])
            .unwrap_or("")
            .trim();
        // Strip optional title: `[foo](url "title")`
        let url = match url_raw.split_once(char::is_whitespace) {
            Some((head, _tail)) => head.trim(),
            None => url_raw,
        };
        // Handle angle-bracket wrapped URLs: `[text](<url>)`
        let url = url
            .strip_prefix('<')
            .and_then(|s| s.strip_suffix('>'))
            .unwrap_or(url)
            .to_string();

        if !url.is_empty() && seen.insert(url.clone()) {
            let filename = if !text.is_empty() && !looks_like_url(&text) {
                text
            } else {
                filename_from_url(&url)
            };
            out.push(MarkdownAttachment {
                filename,
                url,
                is_image,
            });
        }

        i = url_end + 1;
    }

    // Also parse HTML <img> tags — GitHub's Web UI inserts attachments
    // as `<img src="..." alt="..." />` rather than markdown `![]()`.
    parse_html_img_tags(markdown, &mut out, &mut seen);

    out
}

/// Extract `src` URLs from HTML `<img>` tags.
fn parse_html_img_tags(
    html: &str,
    out: &mut Vec<MarkdownAttachment>,
    seen: &mut std::collections::HashSet<String>,
) {
    let lower = html.to_ascii_lowercase();
    let mut search_from = 0;
    while let Some(tag_start) = lower[search_from..].find("<img ") {
        let abs_start = search_from + tag_start;
        let Some(tag_end_rel) = html[abs_start..].find('>') else {
            break;
        };
        let tag = &html[abs_start..abs_start + tag_end_rel + 1];

        // Extract src="..."
        let url = extract_html_attr(tag, "src").unwrap_or_default();
        let alt = extract_html_attr(tag, "alt").unwrap_or_default();

        if !url.is_empty() && seen.insert(url.clone()) {
            let filename = if !alt.is_empty() && alt != "Image" && !looks_like_url(&alt) {
                alt
            } else {
                filename_from_url(&url)
            };
            out.push(MarkdownAttachment {
                filename,
                url,
                is_image: true,
            });
        }

        search_from = abs_start + tag_end_rel + 1;
    }
}

/// Extract the value of an HTML attribute from a tag string.
fn extract_html_attr(tag: &str, attr_name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let pattern = format!("{attr_name}=\"");
    let start = lower.find(&pattern)? + pattern.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// A single attachment reference found in a markdown document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownAttachment {
    /// Best-effort filename (from alt text or URL path).
    pub filename: String,
    /// Absolute or relative URL as written in the markdown.
    pub url: String,
    /// `true` if the reference was an image (`![]()`), `false` for a link.
    pub is_image: bool,
}

/// Find the index of the matching `close` byte for an open character, with
/// simple bracket/parenthesis nesting support. Returns `None` if unmatched.
fn find_matching(bytes: &[u8], open: u8, close: u8) -> Option<usize> {
    let mut depth: usize = 1;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Cheap heuristic — is a string "a URL" (we use this to decide whether the
/// link text is informative enough to be used as a filename).
fn looks_like_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://") || s.starts_with("www.")
}

/// Derive a filename from the final path segment of a URL. Query strings
/// and fragments are stripped. Returns `"attachment"` if nothing sensible
/// can be extracted (e.g. the URL has no path beyond the host).
pub fn filename_from_url(url: &str) -> String {
    let no_query = url.split_once('?').map(|(p, _)| p).unwrap_or(url);
    let no_frag = no_query.split_once('#').map(|(p, _)| p).unwrap_or(no_query);

    // Strip scheme + host so that `https://x/` does not incorrectly surface
    // the host `x` as a filename. We only look at the path portion.
    let path = match no_frag.split_once("://") {
        Some((_scheme, rest)) => rest.split_once('/').map(|(_host, p)| p).unwrap_or(""),
        None => no_frag,
    };

    let last = path
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or("");
    if last.is_empty() {
        "attachment".to_string()
    } else {
        last.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_context_slug_formats() {
        let issue = AssetContext::Issue {
            key: "DEV-123".into(),
        };
        assert_eq!(issue.slug(), "issue:DEV-123");

        let mr = AssetContext::MergeRequest { mr_id: "42".into() };
        assert_eq!(mr.slug(), "mr:42");

        let mr_note = AssetContext::MrComment {
            mr_id: "42".into(),
            note_id: "7".into(),
        };
        assert_eq!(mr_note.slug(), "mr:42:note:7");

        let issue_comment = AssetContext::IssueComment {
            key: "DEV-1".into(),
            comment_id: "99".into(),
        };
        assert_eq!(issue_comment.slug(), "issue:DEV-1:comment:99");

        let chat = AssetContext::Chat {
            chat_id: "C0123".into(),
            message_id: "m5".into(),
        };
        assert_eq!(chat.slug(), "chat:C0123:msg:m5");

        let kb = AssetContext::KbPage {
            page_id: "p7".into(),
        };
        assert_eq!(kb.slug(), "kb:p7");
    }

    #[test]
    fn asset_context_kind_maps_correctly() {
        assert_eq!(
            AssetContext::Issue { key: "x".into() }.kind(),
            AssetContextKind::Issue,
        );
        assert_eq!(
            AssetContext::MergeRequest { mr_id: "1".into() }.kind(),
            AssetContextKind::MergeRequest,
        );
    }

    #[test]
    fn capabilities_full_and_read_only() {
        let full = ContextCapabilities::full();
        assert!(full.upload && full.download && full.delete && full.list);

        let ro = ContextCapabilities::read_only();
        assert!(!ro.upload && ro.download && !ro.delete && ro.list);
    }

    #[test]
    fn asset_capabilities_for_kind() {
        let caps = AssetCapabilities {
            issue: ContextCapabilities::full(),
            merge_request: ContextCapabilities::read_only(),
            ..Default::default()
        };

        assert!(caps.for_kind(AssetContextKind::Issue).upload);
        assert!(!caps.for_kind(AssetContextKind::MergeRequest).upload);
        assert!(caps.for_kind(AssetContextKind::MergeRequest).download);
        // Out-of-scope kinds fall back to empty caps.
        assert!(!caps.for_kind(AssetContextKind::Chat).download);
    }

    #[test]
    fn asset_input_builder() {
        let input = AssetInput::new("a.png", vec![1, 2, 3]).with_mime_type("image/png");
        assert_eq!(input.filename, "a.png");
        assert_eq!(input.data, vec![1, 2, 3]);
        assert_eq!(input.mime_type.as_deref(), Some("image/png"));
    }

    #[test]
    fn asset_input_serde_roundtrip() {
        let input = AssetInput::new("x.bin", vec![0, 1, 2]).with_mime_type("application/octet");
        let json = serde_json::to_string(&input).unwrap();
        let back: AssetInput = serde_json::from_str(&json).unwrap();
        assert_eq!(back.filename, "x.bin");
        assert_eq!(back.data, vec![0, 1, 2]);
        assert_eq!(back.mime_type.as_deref(), Some("application/octet"));

        // mime_type omitted when None — the shape stays small on the wire.
        let without_mime = AssetInput::new("y.txt", vec![]);
        let json = serde_json::to_string(&without_mime).unwrap();
        assert!(!json.contains("mime_type"), "unexpected field: {json}");
    }

    #[test]
    fn asset_meta_serde_roundtrip() {
        let mut meta = AssetMeta {
            id: "a1".into(),
            filename: "screen.png".into(),
            mime_type: Some("image/png".into()),
            size: Some(1234),
            url: Some("https://x/y".into()),
            created_at: Some("2026-04-11T00:00:00Z".into()),
            author: Some("alice".into()),
            cached: true,
            local_path: Some("/tmp/cache/a1.png".into()),
            checksum_sha256: Some("deadbeef".into()),
            analysis: None,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: AssetMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, back);

        // With analysis attached.
        meta.analysis = Some(AssetAnalysis {
            summary: "1 error".into(),
            content_kind: ContentKind::Text,
            extractable_text: Some("ERROR line".into()),
            key_findings: vec!["panic".into()],
            metadata: HashMap::new(),
            semantic: None,
        });
        let json = serde_json::to_string(&meta).unwrap();
        let back: AssetMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(meta, back);
    }

    #[test]
    fn asset_meta_skips_empty_optionals_when_serialized() {
        let meta = AssetMeta {
            id: "a1".into(),
            filename: "x".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&meta).unwrap();
        // `cached` defaults to false and we don't add skip_serializing_if
        // for it, but optional fields should not appear.
        assert!(!json.contains("mime_type"));
        assert!(!json.contains("analysis"));
        assert!(!json.contains("author"));
    }

    #[test]
    fn asset_capabilities_serde_roundtrip() {
        let caps = AssetCapabilities {
            issue: ContextCapabilities::full(),
            issue_comment: ContextCapabilities::read_only(),
            merge_request: ContextCapabilities {
                upload: true,
                download: true,
                delete: false,
                list: true,
                max_file_size: Some(10_485_760),
                allowed_types: vec!["image/*".into()],
            },
            mr_comment: ContextCapabilities::default(),
        };
        let json = serde_json::to_string(&caps).unwrap();
        let back: AssetCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(caps, back);
    }

    #[test]
    fn asset_analysis_with_semantic_serde_roundtrip() {
        let mut metadata = HashMap::new();
        metadata.insert("line_count".into(), serde_json::json!(5432));
        let analysis = AssetAnalysis {
            summary: "error log with 12 ERRORs".into(),
            content_kind: ContentKind::Text,
            extractable_text: Some("ERROR at line 147".into()),
            key_findings: vec!["12 ERROR lines".into(), "race condition suspected".into()],
            metadata,
            semantic: Some(SemanticAnalysis {
                summary: "Redis connection drops under load.".into(),
                findings: vec!["timeout after 30s".into()],
                prompt_used: "find db errors".into(),
                model: "claude-sonnet-4".into(),
                cached: false,
            }),
        };
        let json = serde_json::to_string(&analysis).unwrap();
        let back: AssetAnalysis = serde_json::from_str(&json).unwrap();
        assert_eq!(analysis, back);
    }

    #[test]
    fn content_kind_serde() {
        for kind in [
            ContentKind::Text,
            ContentKind::Image,
            ContentKind::Video,
            ContentKind::Document,
            ContentKind::Data,
            ContentKind::Binary,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            let back: ContentKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn asset_context_kind_serde() {
        for kind in [
            AssetContextKind::Issue,
            AssetContextKind::IssueComment,
            AssetContextKind::MergeRequest,
            AssetContextKind::MrComment,
            AssetContextKind::Chat,
            AssetContextKind::KbPage,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            let back: AssetContextKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn asset_context_all_variants_roundtrip() {
        let variants = vec![
            AssetContext::Issue {
                key: "DEV-1".into(),
            },
            AssetContext::IssueComment {
                key: "DEV-1".into(),
                comment_id: "c1".into(),
            },
            AssetContext::MergeRequest { mr_id: "42".into() },
            AssetContext::MrComment {
                mr_id: "42".into(),
                note_id: "n1".into(),
            },
            AssetContext::Chat {
                chat_id: "C1".into(),
                message_id: "m1".into(),
            },
            AssetContext::KbPage {
                page_id: "p1".into(),
            },
        ];
        for ctx in variants {
            let json = serde_json::to_string(&ctx).unwrap();
            let back: AssetContext = serde_json::from_str(&json).unwrap();
            assert_eq!(ctx, back);

            // Also exercise `kind()` / `slug()` for every variant so the
            // match arms stay covered.
            assert!(!ctx.slug().is_empty());
            let _ = ctx.kind();
        }
    }

    #[test]
    fn asset_context_serde_roundtrip() {
        let ctx = AssetContext::IssueComment {
            key: "DEV-5".into(),
            comment_id: "42".into(),
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let back: AssetContext = serde_json::from_str(&json).unwrap();
        assert_eq!(ctx, back);
    }

    #[test]
    fn content_kind_default_is_binary() {
        assert_eq!(ContentKind::default(), ContentKind::Binary);
    }

    #[test]
    fn filename_from_url_strips_query_and_fragment() {
        assert_eq!(
            filename_from_url("https://x/y/z/report.log?token=abc#top"),
            "report.log"
        );
        assert_eq!(filename_from_url("https://x/"), "attachment");
        assert_eq!(filename_from_url(""), "attachment");
    }

    #[test]
    fn markdown_parses_image_and_link_syntax() {
        let md = "Hello ![screenshot](https://cdn.example.com/a/b/screen.png) and \
                  a [log](https://cdn.example.com/run-42.log).";
        let attachments = parse_markdown_attachments(md);
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].filename, "screenshot");
        assert_eq!(attachments[0].url, "https://cdn.example.com/a/b/screen.png");
        assert!(attachments[0].is_image);
        assert_eq!(attachments[1].filename, "log");
        assert!(!attachments[1].is_image);
    }

    #[test]
    fn markdown_deduplicates_by_url() {
        let md = "![a](https://x/1.png) and again ![b](https://x/1.png)";
        let attachments = parse_markdown_attachments(md);
        assert_eq!(attachments.len(), 1);
        // The first reference wins.
        assert_eq!(attachments[0].filename, "a");
    }

    #[test]
    fn markdown_handles_titles_and_spaces() {
        let md = "[spec](https://x/spec.pdf \"Specification\")";
        let attachments = parse_markdown_attachments(md);
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].url, "https://x/spec.pdf");
        assert_eq!(attachments[0].filename, "spec");
    }

    #[test]
    fn markdown_ignores_unmatched_brackets() {
        let md = "Unclosed [foo( and then a good ![g](https://x/g.png)";
        let attachments = parse_markdown_attachments(md);
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].url, "https://x/g.png");
    }

    #[test]
    fn markdown_falls_back_to_url_when_text_is_url() {
        let md = "[https://x/a.png](https://x/a.png)";
        let attachments = parse_markdown_attachments(md);
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].filename, "a.png");
    }

    #[test]
    fn markdown_empty_and_plain_text() {
        assert!(parse_markdown_attachments("").is_empty());
        assert!(parse_markdown_attachments("no links here at all").is_empty());
    }

    #[test]
    fn markdown_strips_angle_bracket_urls() {
        let md = "[spec](<https://example.com/spec.pdf>)";
        let attachments = parse_markdown_attachments(md);
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].url, "https://example.com/spec.pdf");
        assert_eq!(attachments[0].filename, "spec");

        // Image variant
        let md = "![shot](<https://cdn.example.com/img.png>)";
        let attachments = parse_markdown_attachments(md);
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].url, "https://cdn.example.com/img.png");
    }
}
