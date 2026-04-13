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
        /// Issue key.
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
        /// MR / PR identifier.
        mr_id: String,
        /// Note / comment identifier.
        note_id: String,
    },
    /// Attachment from a messenger chat (Slack, Telegram, etc.).
    Chat {
        /// Chat identifier.
        chat_id: String,
        /// Message identifier.
        message_id: String,
    },
    /// Attachment from a knowledge base page (Confluence, etc.).
    KbPage {
        /// Page identifier.
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
    /// Comment on an issue.
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
    /// Original filename.
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
    /// Capabilities for issue comments.
    #[serde(default)]
    pub issue_comment: ContextCapabilities,
    /// Capabilities for merge request bodies.
    #[serde(default)]
    pub merge_request: ContextCapabilities,
    /// Capabilities for merge request notes/comments.
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
    /// Category of the content.
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
    /// Video file.
    Video,
    /// Document file (PDF, DOCX, ...).
    Document,
    /// Structured data (CSV, XLSX, JSON, YAML).
    Data,
    /// Binary content of an unknown kind.
    #[default]
    Binary,
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
}
