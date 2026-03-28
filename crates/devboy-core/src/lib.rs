//! Core traits, types, and error handling for devboy-tools.
//!
//! This crate provides the foundational abstractions used across all devboy components:
//!
//! - **Provider traits**: [`IssueProvider`], [`MergeRequestProvider`], [`Provider`]
//! - **Unified types**: [`Issue`], [`MergeRequest`], [`Discussion`], [`Comment`], [`FileDiff`]
//! - **Configuration**: [`Config`], [`GitHubConfig`], [`GitLabConfig`]
//! - **Error handling**: [`Error`], [`Result`]
//!
//! # Example
//!
//! ```ignore
//! use devboy_core::{IssueProvider, IssueFilter, Issue, Result};
//!
//! async fn list_open_issues(provider: &dyn IssueProvider) -> Result<Vec<Issue>> {
//!     let filter = IssueFilter {
//!         state: Some("opened".to_string()),
//!         limit: Some(10),
//!         ..Default::default()
//!     };
//!     provider.get_issues(filter).await
//! }
//! ```

pub mod config;
pub mod enricher;
pub mod error;
pub mod provider;
pub mod tool_category;
pub mod types;

// Re-export error types
pub use error::{Error, Result};

// Re-export provider traits
pub use provider::{IssueProvider, MergeRequestProvider, PipelineProvider, Provider};

// Re-export all types
pub use types::{
    CodePosition, Comment, CreateCommentInput, CreateIssueInput, CreateMergeRequestInput,
    Discussion, FailedJob, FileDiff, GetPipelineInput, GetUsersOptions, Issue, IssueFilter,
    IssueStatus, JobLogMode, JobLogOptions, JobLogOutput, MergeRequest, MrFilter, Pagination,
    PipelineInfo, PipelineJob, PipelineStage, PipelineStatus, PipelineSummary, Release,
    ReleaseAsset, UpdateIssueInput, User,
};

// Re-export enricher traits and utilities
pub use enricher::{PropertySchema, ToolEnricher, ToolSchema, sanitize_field_name};
pub use tool_category::ToolCategory;

// Re-export config types
pub use config::{
    BuiltinToolsConfig, ClickUpConfig, Config, ContextConfig, FormatPipelineConfig, GitHubConfig,
    GitLabConfig, JiraConfig, ProxyMatchingConfig, ProxyMcpServerConfig,
};
