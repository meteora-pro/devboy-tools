//! Core traits, types, and error handling for devboy-tools.
//!
//! This crate provides the foundational abstractions used across all devboy components:
//!
//! - **Provider traits**: [`IssueProvider`], [`MergeRequestProvider`], [`MessengerProvider`], [`Provider`]
//! - **Unified types**: [`Issue`], [`MergeRequest`], [`MessengerChat`], [`MessengerMessage`], [`Discussion`], [`Comment`], [`FileDiff`]
//! - **Configuration**: [`Config`], [`GitHubConfig`], [`GitLabConfig`]
//! - **Error handling**: [`Error`], [`Result`]
//!
//! # Example
//!
//! ```ignore
//! use devboy_core::{IssueProvider, IssueFilter, Issue, ProviderResult, Result};
//!
//! async fn list_open_issues(provider: &dyn IssueProvider) -> Result<ProviderResult<Issue>> {
//!     let filter = IssueFilter {
//!         state: Some("opened".to_string()),
//!         limit: Some(10),
//!         ..Default::default()
//!     };
//!     provider.get_issues(filter).await
//! }
//! ```

pub mod asset;
pub mod config;
pub mod enricher;
pub mod error;
pub mod provider;
pub mod remote_config;
#[cfg(feature = "sentry")]
pub mod sentry_integration;
pub mod tool_category;
pub mod types;

// Re-export error types
pub use error::{Error, Result};

// Re-export provider traits
pub use provider::{
    IssueProvider, MeetingNotesProvider, MergeRequestProvider, MessengerProvider, PipelineProvider,
    Provider,
};

// Re-export all types
pub use types::{
    AddStructureGeneratorInput, AddStructureRowsInput, AssignToSprintInput, CodePosition, Comment,
    CreateCommentInput, CreateIssueInput, CreateMergeRequestInput, CreateStructureInput,
    Discussion, FailedJob, FileDiff, ForestModifyResult, GetChatsParams, GetForestOptions,
    GetMessagesParams, GetPipelineInput, GetStructureValuesInput, GetUsersOptions, Issue,
    IssueFilter, IssueLink, IssueRelations, IssueStatus, JobLogMode, JobLogOptions, JobLogOutput,
    MeetingFilter, MeetingNote, MeetingSpeaker, MeetingTranscript, MergeRequest, MessageAttachment,
    MessageAuthor, MessengerChat, MessengerMessage, MoveStructureRowsInput, MrFilter, Pagination,
    PipelineInfo, PipelineJob, PipelineStage, PipelineStatus, PipelineSummary, ProviderResult,
    Release, ReleaseAsset, SaveStructureViewInput, SearchMessagesParams, SendMessageParams,
    SortInfo, SortOrder, Sprint, SprintState, Structure, StructureColumnValue, StructureForest,
    StructureGenerator, StructureNode, StructureRowItem, StructureRowValues, StructureValues,
    StructureView, StructureViewColumn, SyncStructureGeneratorInput, TranscriptSentence,
    UpdateIssueInput, UpdateMergeRequestInput, UpdateStructureAutomationInput, User,
};

// Re-export enricher traits and utilities
pub use enricher::{PropertySchema, ToolEnricher, ToolSchema, sanitize_field_name};
pub use tool_category::ToolCategory;

// Re-export asset management types
pub use asset::{
    AssetAnalysis, AssetCapabilities, AssetContext, AssetContextKind, AssetInput, AssetMeta,
    ContentKind, ContextCapabilities, MarkdownAttachment, SemanticAnalysis, filename_from_url,
    parse_markdown_attachments,
};

// Re-export config types
pub use config::{
    BuiltinToolsConfig, ClickUpConfig, Config, ContextConfig, FirefliesConfig,
    FormatPipelineConfig, GitHubConfig, GitLabConfig, JiraConfig, ProxyConfig, ProxyMatchingConfig,
    ProxyMcpServerConfig, ProxyRoutingConfig, ProxyRoutingOverride, ProxySecretsConfig,
    ProxyTelemetryConfig, ProxyToolRule, RemoteConfigSettings, RoutingStrategy, SentryConfig,
    SlackConfig, default_slack_required_scopes, matches_glob, routing_strategy_slug,
};
