//! DevBoy CLI - Command-line interface for devboy-tools.

mod doctor;
mod skills_cmd;
mod update_check;
mod upgrade;

use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use devboy_clickup::ClickUpClient;
use devboy_core::{
    BuiltinToolsConfig, ClickUpConfig, Config, ContextConfig, GitHubConfig, GitLabConfig,
    IssueFilter, IssueProvider, JiraConfig, MergeRequestProvider, MrFilter, Provider,
    ProxyMcpServerConfig, SlackConfig, routing_strategy_slug,
};
use devboy_github::GitHubClient;
use devboy_gitlab::GitLabClient;
use devboy_jira::JiraClient;
use devboy_mcp::protocol::ToolDefinition;
use devboy_mcp::routing::{ProxyStatus, RoutingEngine};
use devboy_mcp::signature_match::{ToolCatalogue, build_report};
use devboy_mcp::telemetry::{TelemetryAuth, TelemetryPipeline};
use devboy_mcp::{
    JSONRPC_VERSION, JsonRpcRequest, KNOWN_BUILTIN_TOOLS, McpProxyClient, McpServer, ProxyManager,
    ProxyTransport, RequestId,
};
use devboy_slack::SlackClient;
use devboy_storage::{ChainStore, CredentialStore, wrap_with_cache};
use dialoguer::{Confirm, Input, MultiSelect, Password};
use doctor::{DoctorOptions, OutputFormat};
use tracing_subscriber::EnvFilter;
#[cfg(feature = "sentry")]
use tracing_subscriber::prelude::*;

/// Proxy transport type for MCP servers.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum TransportType {
    /// Modern HTTP POST-based transport with mcp-session-id header
    #[value(name = "streamable-http")]
    StreamableHttp,
    /// Legacy MCP transport using GET for SSE stream, POST for requests
    #[value(name = "sse")]
    Sse,
}

impl TransportType {
    fn as_str(&self) -> &'static str {
        match self {
            TransportType::StreamableHttp => "streamable-http",
            TransportType::Sse => "sse",
        }
    }
}

/// Authentication type for proxy servers.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum AuthType {
    /// Bearer token authentication
    Bearer,
    /// API key authentication
    #[value(name = "api_key")]
    ApiKey,
    /// No authentication
    None,
}

impl AuthType {
    fn as_str(&self) -> &'static str {
        match self {
            AuthType::Bearer => "bearer",
            AuthType::ApiKey => "api_key",
            AuthType::None => "none",
        }
    }
}

/// Build version string with commit info for --version output.
const BUILD_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (commit ",
    env!("DEVBOY_BUILD_COMMIT"),
    ", built ",
    env!("DEVBOY_BUILD_TIMESTAMP"),
    ")",
);

/// Full release string for Sentry (e.g., "devboy-tools@0.16.0+abc1234").
#[cfg(feature = "sentry")]
fn sentry_release() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let commit = env!("DEVBOY_BUILD_COMMIT");
    if commit.is_empty() {
        format!("devboy-tools@{version}")
    } else {
        format!("devboy-tools@{version}+{commit}")
    }
}

#[derive(Parser)]
#[command(name = "devboy")]
#[command(author, version = BUILD_VERSION, about = "DevBoy - AI-powered development tools", long_about = None)]
struct Cli {
    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new .devboy.toml configuration file
    Init {
        /// Skip prompts and use defaults (required in non-TTY environments)
        #[arg(short, long)]
        yes: bool,

        /// Show what would be done without making changes
        #[arg(long)]
        dry_run: bool,

        /// Overwrite existing config (creates timestamped backup)
        #[arg(short, long)]
        force: bool,

        /// Register devboy as MCP server in Claude Code
        #[arg(long)]
        claude: bool,

        /// Register devboy as MCP server in Kimi CLI
        #[arg(long)]
        kimi: bool,

        /// Register devboy as MCP server in Codex CLI
        #[arg(long)]
        codex_cli: bool,

        /// Register devboy as MCP server in Copilot CLI
        #[arg(long)]
        copilot: bool,

        /// Register devboy as MCP server in Gemini CLI
        #[arg(long)]
        gemini: bool,

        /// Register devboy as MCP server in OpenCode
        #[arg(long)]
        opencode: bool,

        /// Register devboy as MCP server in ForgeCode
        #[arg(long)]
        forge: bool,

        /// Context name for the configuration
        #[arg(short, long)]
        context: Option<String>,

        /// Add MCP proxy server URL
        #[arg(long)]
        proxy: Option<String>,

        /// Only configure proxy, skip git remote auto-detection
        #[arg(long, requires = "proxy")]
        proxy_only: bool,

        /// Proxy server name (default: "proxy")
        #[arg(long, requires = "proxy")]
        proxy_name: Option<String>,

        /// Proxy transport type (default: streamable-http)
        #[arg(long, requires = "proxy", value_enum)]
        proxy_transport: Option<TransportType>,

        /// Keychain key for proxy token (e.g., "proxy.token")
        #[arg(long, requires = "proxy")]
        proxy_token_key: Option<String>,

        /// Proxy token value (will be stored in keychain automatically)
        #[arg(long, requires = "proxy")]
        proxy_token: Option<String>,

        /// Proxy auth type (default: bearer if token provided, else none)
        #[arg(long, requires = "proxy", value_enum)]
        proxy_auth_type: Option<AuthType>,

        /// Remote config URL (fetched on startup, TOML response merged with local config)
        #[arg(long)]
        remote_config_url: Option<String>,

        /// Bearer token for remote config endpoint
        #[arg(long, requires = "remote_config_url")]
        remote_config_token: Option<String>,

        /// Force git remote auto-detection even when `--remote-config-url` is set.
        ///
        /// By default, passing `--remote-config-url` suppresses local git auto-detection
        /// (remote config is treated as the source of truth for integrations). Use
        /// `--detect-git` to restore the pre-existing behaviour and auto-add a local
        /// GitHub/GitLab context alongside the remote config.
        #[arg(long)]
        detect_git: bool,
    },

    /// Start the MCP server (stdio mode for AI assistants)
    Mcp {
        /// Skip loading config file, use only environment variables
        #[arg(long)]
        no_config: bool,
    },

    /// Configuration management
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    /// Context (profile) management
    Context {
        #[command(subcommand)]
        command: ContextCommands,
    },

    /// Get information about issues
    Issues {
        /// Filter by state
        #[arg(short, long, default_value = "open", value_parser = ["open", "closed", "all"])]
        state: String,

        /// Maximum number of issues to display
        #[arg(short, long, default_value = "20")]
        limit: u32,
    },

    /// Get information about merge requests / pull requests
    Mrs {
        /// Filter by state
        #[arg(short, long, default_value = "open", value_parser = ["open", "merged", "closed", "all"])]
        state: String,

        /// Maximum number of MRs to display
        #[arg(short, long, default_value = "20")]
        limit: u32,
    },

    /// Test provider connection
    Test {
        /// Provider to test (github, gitlab, clickup, jira, slack)
        provider: String,
    },

    /// Interact with upstream MCP proxy servers
    Proxy {
        #[command(subcommand)]
        command: ProxyCommands,
    },

    /// Manage built-in tools (enable/disable). Interactive mode when run without subcommand.
    Tools {
        #[command(subcommand)]
        command: Option<ToolsCommands>,
    },

    /// Generate reference documentation from the live binary
    /// (built-in tools or CLI surface).
    Docs {
        #[command(subcommand)]
        command: DocsCommands,
    },

    /// Manage skills — procedural recipes installed alongside the tool bundle
    Skills {
        #[command(subcommand)]
        command: SkillsCommands,
    },

    /// Write to a skill's self-feedback session trace (ADR-015)
    Trace {
        #[command(subcommand)]
        command: TraceCommands,
    },

    /// Run diagnostic checks for the local DevBoy setup
    Doctor {
        /// Output machine-readable JSON
        #[arg(long, value_enum)]
        format: Option<DoctorOutputFormat>,

        /// List available check IDs and exit
        #[arg(long)]
        list_checks: bool,

        /// Run only the specified check IDs (comma-delimited or repeated)
        #[arg(long, value_delimiter = ',')]
        checks: Vec<String>,
    },

    /// Upgrade devboy to the latest version
    Upgrade {
        /// Only check for updates, don't install
        #[arg(long)]
        check: bool,
    },

    /// Benchmark format pipeline on real open-source project data (JSON vs TOON)
    Benchmark {
        /// GitHub owner (e.g., "kubernetes")
        #[arg(short, long, default_value = "facebook")]
        owner: String,

        /// GitHub repo (e.g., "kubernetes")
        #[arg(short, long, default_value = "react")]
        repo: String,

        /// Token budget (default: 8000)
        #[arg(short, long, default_value = "8000")]
        budget: usize,

        /// Maximum issues to fetch (default: 30)
        #[arg(short = 'n', long, default_value = "30")]
        limit: u32,

        /// GitHub token (optional, for higher rate limits). Set GITHUB_TOKEN env var.
        #[arg(long)]
        token: Option<String>,
    },

    /// Format JSON from stdin through the TOON pipeline (pipe mode)
    ///
    /// Usage: cat issues.json | devboy format-pipeline
    #[command(name = "format-pipeline")]
    FormatPipeline {
        /// Data type: issues, merge_requests, diffs, comments, discussions (auto-detected if omitted)
        #[arg(short = 't', long, value_name = "TYPE")]
        data_type: Option<String>,

        /// Token budget (default: 8000, minimum: 1)
        #[arg(short, long, default_value = "8000")]
        budget: usize,

        /// Trimming strategy
        #[arg(short, long, value_parser = ["element_count", "cascading", "size_proportional", "thread_level", "head_tail", "default"])]
        strategy: Option<String>,

        /// Trim level
        #[arg(short, long, default_value = "full", value_parser = ["full", "standard", "minimal"])]
        level: String,

        /// Output format (JSON mode still applies budget trimming)
        #[arg(short, long, default_value = "toon", value_parser = ["toon", "json"])]
        format: String,

        /// Print token savings stats to stderr
        #[arg(long)]
        stats: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum DoctorOutputFormat {
    Console,
    Json,
}

impl From<DoctorOutputFormat> for OutputFormat {
    fn from(value: DoctorOutputFormat) -> Self {
        match value {
            DoctorOutputFormat::Console => OutputFormat::Console,
            DoctorOutputFormat::Json => OutputFormat::Json,
        }
    }
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Set a configuration value
    Set {
        /// Config key (e.g., github.owner, gitlab.url)
        key: String,
        /// Config value. `allow_hyphen_values` handles the common case of negative
        /// numbers like `-1` — clap no longer treats them as flags but passes them to
        /// the domain validator which returns a clear validation error.
        #[arg(allow_hyphen_values = true)]
        value: String,
    },

    /// Set a secret value (stored in OS keychain)
    SetSecret {
        /// Secret key (e.g., github.token, gitlab.token)
        key: String,
        /// Secret value (will be stored securely)
        value: String,
    },

    /// Get a configuration value
    Get {
        /// Config key (e.g., github.owner, gitlab.url)
        key: String,
    },

    /// List all configuration
    List,

    /// Show configuration file path
    Path,
}

#[derive(Subcommand)]
enum ProxyCommands {
    /// Add a new proxy server configuration
    Add {
        /// Proxy server name
        name: String,

        /// Proxy server URL
        #[arg(long)]
        url: String,

        /// Transport type (default: streamable-http)
        #[arg(long, default_value = "streamable-http", value_enum)]
        transport: TransportType,

        /// Keychain key for proxy token (e.g., "proxy.token")
        #[arg(long)]
        token_key: Option<String>,

        /// Token value (will be stored in keychain automatically)
        #[arg(long)]
        token: Option<String>,

        /// Auth type (default: bearer if token provided, else none)
        #[arg(long, value_enum)]
        auth_type: Option<AuthType>,

        /// Overwrite existing proxy with same name
        #[arg(short, long)]
        force: bool,
    },

    /// Remove a proxy server configuration
    Remove {
        /// Proxy server name to remove
        name: String,
    },

    /// List available tools from all upstream proxy servers
    Tools {
        /// Show tool descriptions
        #[arg(long)]
        descriptions: bool,
    },

    /// Call a tool on an upstream proxy server
    Call {
        /// Tool name (e.g., devboy-cloud__get_issues)
        tool: String,
        /// JSON arguments (optional)
        #[arg(default_value = "{}")]
        args: String,
    },

    /// Inspect transparent-routing status: matched tools, overrides, schema issues
    Status {
        /// Output as JSON instead of a human-readable report
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ContextCommands {
    /// List available contexts and show active one
    List,
    /// Switch active context
    Use { name: String },
}

#[derive(Subcommand)]
enum ToolsCommands {
    /// List all built-in tools and their status (enabled/disabled)
    List,
    /// Disable specific built-in tools
    Disable {
        /// Tool names to disable
        #[arg(required = true, num_args = 1..)]
        names: Vec<String>,
    },
    /// Enable specific built-in tools (remove from disabled list)
    Enable {
        /// Tool names to enable
        #[arg(required = true, num_args = 1..)]
        names: Vec<String>,
    },
    /// Reset all built-in tools filtering (re-enable everything)
    Reset,
    /// Call a built-in tool directly
    Call {
        /// Tool name (e.g., get_issues, list_contexts)
        name: String,
        /// JSON arguments (optional)
        #[arg(default_value = "{}")]
        args: String,
    },
    /// Generate auto-built reference docs for built-in tools
    Docs {
        /// Output format
        #[arg(long, value_enum, default_value_t = ToolsDocsFormat::Markdown)]
        format: ToolsDocsFormat,
        /// Write to a file instead of stdout
        #[arg(long)]
        output: Option<PathBuf>,
        /// Compare the rendered docs to an existing file and exit non-zero if they differ.
        /// Useful in CI: `--check --output docs/guide/reference/tools.md`.
        #[arg(long, requires = "output")]
        check: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ToolsDocsFormat {
    Markdown,
    Json,
}

#[derive(Subcommand)]
enum DocsCommands {
    /// Render reference docs for every CLI subcommand and flag.
    ///
    /// Sourced from the live `clap` definition — no separate maintenance.
    /// In CI, pair with `--check --output docs/guide/reference/cli.md`
    /// to fail when the committed snapshot drifts from the binary.
    Cli {
        /// Output format (only `markdown` is supported in v1; `json` is
        /// reserved for a future structured walk over `Cli::command()`).
        #[arg(long, value_enum, default_value_t = CliDocsFormat::Markdown)]
        format: CliDocsFormat,
        /// Write to a file instead of stdout.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Compare the rendered docs against `--output` and exit
        /// non-zero on drift. Use in CI / pre-commit hooks.
        #[arg(long, requires = "output")]
        check: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CliDocsFormat {
    Markdown,
}

impl From<ToolsDocsFormat> for devboy_executor::tool_docs::DocsFormat {
    fn from(value: ToolsDocsFormat) -> Self {
        match value {
            ToolsDocsFormat::Markdown => devboy_executor::tool_docs::DocsFormat::Markdown,
            ToolsDocsFormat::Json => devboy_executor::tool_docs::DocsFormat::Json,
        }
    }
}

#[derive(Subcommand)]
enum SkillsCommands {
    /// List every skill the embedded source knows about
    List {
        /// Filter by category id (e.g. `self-bootstrap`, `issue-tracking`)
        #[arg(long)]
        category: Option<String>,
    },
    /// Print a skill's full `SKILL.md` contents
    Show {
        /// Skill name
        name: String,
    },
    /// Install one or more skills to the resolved target(s)
    Install {
        /// Skill names. Ignored when `--all` or `--category` is used.
        names: Vec<String>,
        /// Install every skill the embedded source knows about.
        #[arg(long, conflicts_with = "category")]
        all: bool,
        /// Install every skill in the given category.
        #[arg(long)]
        category: Option<String>,
        /// Install globally at `~/.agents/skills/` instead of repo-local.
        #[arg(long, conflicts_with = "local")]
        global: bool,
        /// When combined with `--agent`, install under the repo instead of the user home.
        #[arg(long)]
        local: bool,
        /// Install for one or more agents (claude, codex, cursor, kimi, all).
        /// Accepts the value repeatedly or comma-delimited.
        #[arg(long = "agent", value_delimiter = ',')]
        agents: Vec<String>,
        /// Overwrite user-modified files.
        #[arg(long)]
        force: bool,
        /// Show what would happen without touching the filesystem.
        #[arg(long)]
        dry_run: bool,
    },
    /// Upgrade previously installed skills (shortcut for `install` on what the manifest already knows).
    Upgrade {
        /// Upgrade only these skills (default: every skill in the manifest).
        names: Vec<String>,
        /// Upgrade the global target at `~/.agents/skills/`.
        #[arg(long, conflicts_with = "local")]
        global: bool,
        /// When combined with `--agent`, upgrade under the repo instead of the user home.
        #[arg(long)]
        local: bool,
        /// Apply to these agents' install paths.
        #[arg(long = "agent", value_delimiter = ',')]
        agents: Vec<String>,
        /// Overwrite user-modified files.
        #[arg(long)]
        force: bool,
        /// Show what would happen without touching the filesystem.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove installed skills from the resolved target(s).
    Remove {
        /// Skill names to remove.
        #[arg(required = true, num_args = 1..)]
        names: Vec<String>,
        /// Counterpart to `--agent` (see `install`).
        #[arg(long, conflicts_with = "local")]
        global: bool,
        /// Counterpart to `--agent` (see `install`).
        #[arg(long)]
        local: bool,
        /// Apply to these agents' install paths.
        #[arg(long = "agent", value_delimiter = ',')]
        agents: Vec<String>,
        /// Fail if a requested skill is not present at the target.
        #[arg(long)]
        strict: bool,
        /// Show what would happen without touching the filesystem.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum TraceCommands {
    /// Start a new session trace. Prints a single JSON line with
    /// `session_id`, `session_dir`, `trace_path` — skills capture that
    /// and pass the values to subsequent `event` / `end` invocations.
    Begin {
        /// Skill name. Shown as the directory under the date, also
        /// recorded in every event.
        #[arg(long)]
        skill: String,
        /// Write under `~/.devboy/sessions/` instead of the repo-local
        /// `<repo>/.devboy/sessions/`.
        #[arg(long, conflicts_with = "dir")]
        global: bool,
        /// Write under an explicit directory (mostly for testing).
        #[arg(long)]
        dir: Option<String>,
    },
    /// Append one event to an existing session.
    Event {
        /// Session directory printed by `trace begin`.
        #[arg(long)]
        session_dir: String,
        /// Session id printed by `trace begin`.
        #[arg(long)]
        session_id: String,
        /// Skill name.
        #[arg(long)]
        skill: String,
        /// Event phase. Supported values: `start`, `decision`, `tool_call`,
        /// `tool_result`, `verify`, `artifact`, `note`, `end`.
        #[arg(long)]
        phase: String,
        /// JSON payload. Defaults to `{}` if omitted.
        #[arg(long, default_value = "{}")]
        payload: String,
    },
    /// Finalise a session — writes the closing event and updates
    /// `meta.json` with the outcome + aggregated counts.
    End {
        /// Session directory printed by `trace begin`.
        #[arg(long)]
        session_dir: String,
        /// Session id printed by `trace begin`.
        #[arg(long)]
        session_id: String,
        /// Skill name.
        #[arg(long)]
        skill: String,
        /// Session outcome: `success`, `failure`, `aborted`.
        #[arg(long)]
        outcome: String,
        /// Human-readable summary.
        #[arg(long, default_value = "")]
        summary: String,
    },
}

/// Environment variable to skip keychain operations (for CI testing).
/// When set to "1" or "true", uses in-memory store instead of OS keychain.
const SKIP_KEYCHAIN_ENV: &str = "DEVBOY_SKIP_KEYCHAIN";

/// Environment variable to skip loading config file.
/// When set to "1" or "true", MCP server uses only environment variables.
const NO_CONFIG_ENV: &str = "DEVBOY_NO_CONFIG";

/// Get credential store using the appropriate chain.
///
/// When `DEVBOY_SKIP_KEYCHAIN=1` is set:
/// - Uses CI chain (env vars + memory, no keychain access)
///
/// Otherwise uses the default chain which resolves credentials in this order:
/// 1. Environment variables (`DEVBOY_{PROVIDER}_TOKEN`, then `{PROVIDER}_TOKEN`)
/// 2. OS Keychain (macOS Keychain, Windows Credential Manager, Linux Secret Service)
///
/// This allows CI/CD pipelines to use environment variables while local development
/// uses the keychain seamlessly.
fn get_credential_store() -> Box<dyn CredentialStore> {
    if is_skip_keychain_enabled() {
        tracing::info!("Using CI credential chain (env vars only, keychain disabled)");
        Box::new(ChainStore::ci_chain())
    } else {
        tracing::debug!("Using default credential chain (env vars -> keychain)");
        Box::new(ChainStore::default_chain())
    }
}

/// Check if keychain should be skipped (for CI/containers).
fn is_skip_keychain_enabled() -> bool {
    env_is_truthy(SKIP_KEYCHAIN_ENV)
}

/// Check if config file should be skipped.
fn is_no_config_enabled() -> bool {
    env_is_truthy(NO_CONFIG_ENV)
}

/// Check if an environment variable is set to a truthy value ("1" or "true").
fn env_is_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false)
}

/// Get credential store for init/proxy-add commands.
///
/// Same as `get_credential_store()` - uses CI chain when `DEVBOY_SKIP_KEYCHAIN=1`.
fn get_credential_store_for_init() -> Box<dyn CredentialStore> {
    get_credential_store()
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize Sentry from local config / env vars.
    // May return None if no DSN is configured yet — remote config can provide it later.
    #[cfg(feature = "sentry")]
    let mut _sentry_guard = {
        let config = if is_no_config_enabled() {
            None
        } else {
            load_runtime_config().ok().map(|(c, _)| c)
        };
        devboy_core::sentry_integration::init_sentry(
            config.as_ref().and_then(|c| c.sentry.as_ref()),
            &sentry_release(),
        )
    };

    // Initialize logging
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };

    // stdout is a transport-layer channel for `devboy mcp` (JSON-RPC messages) and
    // any subcommand a script is expected to parse (`tools call` → MCP tool-result
    // JSON, `tools list` → plain-text output users pipe to grep/sed, `format-pipeline`
    // → whatever the pipeline emits to stdout, `proxy status --json`). Route tracing
    // to stderr in all those cases so `devboy tools call get_issues ... | jq` and
    // `devboy mcp` do not get their stdout clobbered with log lines. For plain
    // interactive commands (`init`, `config set`, `doctor`, `issues`) we keep logs
    // on stdout so users see them without having to redirect.
    let needs_stderr_logs = matches!(
        &cli.command,
        Some(Commands::Mcp { .. })
            | Some(Commands::Proxy {
                command: ProxyCommands::Status { json: true, .. },
            })
            | Some(Commands::Tools {
                command: Some(ToolsCommands::Call { .. } | ToolsCommands::List),
            })
            | Some(Commands::FormatPipeline { .. })
    );

    // Always install sentry-tracing layer when feature is enabled.
    // If Sentry is not yet initialized (no DSN), the layer is a no-op.
    // If remote config provides a DSN later, init_sentry() is called again
    // and the layer automatically picks up the new client from Hub::current().
    #[cfg(feature = "sentry")]
    {
        let fmt_layer = if needs_stderr_logs {
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .boxed()
        } else {
            tracing_subscriber::fmt::layer().boxed()
        };
        tracing_subscriber::registry()
            .with(fmt_layer)
            .with(sentry_tracing::layer())
            .with(filter)
            .init();
    }

    #[cfg(not(feature = "sentry"))]
    {
        let builder = tracing_subscriber::fmt().with_env_filter(filter);
        if needs_stderr_logs {
            builder.with_writer(std::io::stderr).init();
        } else {
            builder.init();
        }
    }

    // Run update check in background for interactive commands (skip for mcp, upgrade, and no-command).
    // Spawned as a background task to avoid blocking CLI startup on network calls.
    let update_check_handle = match &cli.command {
        Some(Commands::Mcp { .. })
        | Some(Commands::Upgrade { .. })
        | Some(Commands::Benchmark { .. })
        | Some(Commands::FormatPipeline { .. })
        | None => None,
        _ => Some(tokio::spawn(update_check::check_and_notify())),
    };

    let result: Result<()> = async {
        match cli.command {
            Some(Commands::Init {
                yes,
                dry_run,
                force,
                claude,
                kimi,
                codex_cli,
                copilot,
                gemini,
                opencode,
                forge,
                context,
                proxy,
                proxy_only,
                proxy_name,
                proxy_transport,
                proxy_token_key,
                proxy_token,
                proxy_auth_type,
                remote_config_url,
                remote_config_token,
                detect_git,
            }) => {
                handle_init_command(
                    yes,
                    dry_run,
                    force,
                    claude,
                    kimi,
                    codex_cli,
                    copilot,
                    gemini,
                    opencode,
                    forge,
                    context,
                    proxy,
                    proxy_only,
                    proxy_name,
                    proxy_transport,
                    proxy_token_key,
                    proxy_token,
                    proxy_auth_type,
                    remote_config_url,
                    remote_config_token,
                    detect_git,
                )
                .await?;
            }

            Some(Commands::Mcp { no_config }) => {
                handle_mcp_command(no_config).await?;
            }

            Some(Commands::Config { command }) => {
                handle_config_command(command)?;
            }

            Some(Commands::Context { command }) => {
                handle_context_command(command)?;
            }

            Some(Commands::Issues { state, limit }) => {
                handle_issues_command(&state, limit).await?;
            }

            Some(Commands::Mrs { state, limit }) => {
                handle_mrs_command(&state, limit).await?;
            }

            Some(Commands::Test { provider }) => {
                handle_test_command(&provider).await?;
            }

            Some(Commands::Proxy { command }) => {
                handle_proxy_command(command).await?;
            }

            Some(Commands::Tools { command }) => {
                handle_tools_command(command).await?;
            }

            Some(Commands::Docs { command }) => {
                handle_docs_command(command)?;
            }

            Some(Commands::Skills { command }) => {
                skills_cmd::handle(command).await?;
            }

            Some(Commands::Trace { command }) => {
                skills_cmd::handle_trace(command).await?;
            }

            Some(Commands::Doctor {
                format,
                list_checks,
                checks,
            }) => {
                let exit_code = doctor::handle_doctor_command(DoctorOptions {
                    verbose: cli.verbose,
                    output_format: format.map(Into::into),
                    list_checks,
                    checks,
                })
                .await?;
                std::process::exit(exit_code);
            }

            Some(Commands::Upgrade { check }) => {
                upgrade::run_upgrade(check).await?;
            }

            Some(Commands::Benchmark {
                owner,
                repo,
                budget,
                limit,
                token,
            }) => {
                run_benchmark(&owner, &repo, budget, limit, token.as_deref()).await?;
            }

            Some(Commands::FormatPipeline {
                data_type,
                budget,
                strategy,
                level,
                format,
                stats,
            }) => {
                run_format_pipeline(
                    data_type.as_deref(),
                    budget,
                    strategy.as_deref(),
                    &level,
                    &format,
                    stats,
                )?;
            }

            None => {
                println!("DevBoy - AI-powered development tools");
                println!("Run with --help for usage information");
            }
        }
        Ok(())
    }
    .await;

    // Capture top-level errors to Sentry before returning.
    // Most command handlers propagate errors via `?` without `tracing::error!`,
    // so without this the common non-panicking error path would never reach Sentry.
    #[cfg(feature = "sentry")]
    if let Err(ref e) = result {
        sentry::capture_message(&format!("{e:#}"), sentry::protocol::Level::Error);
        // Flush pending events to ensure they are sent before process exits.
        // The guard's Drop also flushes, but with a very short default timeout.
        if let Some(client) = sentry::Hub::current().client() {
            client.flush(Some(std::time::Duration::from_secs(5)));
        }
    }

    // Wait for background update check to finish so the notification can print
    if let Some(handle) = update_check_handle {
        let _ = handle.await;
    }

    result
}

// =============================================================================
// Init Command
// =============================================================================

/// Output path for the configuration file.
const INIT_CONFIG_FILE: &str = ".devboy.toml";

/// Options collected during init.
#[derive(Debug, Default)]
struct InitOptions {
    context_name: Option<String>,
    github: Option<GitHubConfig>,
    gitlab: Option<GitLabConfig>,
    clickup: Option<ClickUpConfig>,
    jira: Option<JiraConfig>,
    slack: Option<SlackConfig>,
    tokens: Vec<(String, String)>, // (key, value) pairs for keychain
    proxy: Option<ProxyMcpServerConfig>,
    remote_config: Option<devboy_core::RemoteConfigSettings>,
}

#[allow(clippy::too_many_arguments)]
async fn handle_init_command(
    yes: bool,
    dry_run: bool,
    force: bool,
    claude: bool,
    kimi: bool,
    codex_cli: bool,
    copilot: bool,
    gemini: bool,
    opencode: bool,
    forge: bool,
    context_name: Option<String>,
    proxy_url: Option<String>,
    proxy_only: bool,
    proxy_name: Option<String>,
    proxy_transport: Option<TransportType>,
    proxy_token_key: Option<String>,
    proxy_token: Option<String>,
    proxy_auth_type: Option<AuthType>,
    remote_config_url: Option<String>,
    remote_config_token: Option<String>,
    detect_git: bool,
) -> Result<()> {
    let config_path = PathBuf::from(INIT_CONFIG_FILE);
    let is_tty = io::stdin().is_terminal();

    // Check TTY requirement for interactive mode
    if !yes && !is_tty {
        anyhow::bail!(
            "Non-interactive environment detected. Use --yes flag for non-interactive mode."
        );
    }

    // Check for existing config
    if config_path.exists() && !force {
        if dry_run {
            println!(
                "[dry-run] Config file already exists: {}",
                config_path.display()
            );
            println!("[dry-run] Use --force to overwrite (will create backup)");
            return Ok(());
        }
        anyhow::bail!(
            "Config file already exists: {}\nUse --force to overwrite (will create backup)",
            config_path.display()
        );
    }

    let skip_git_detect =
        should_skip_git_detect(proxy_only, remote_config_url.as_deref(), detect_git);

    // Collect options
    let mut options = if skip_git_detect {
        // Skip git remote detection, create minimal config.
        // Reason: either --proxy-only was passed, or --remote-config-url was provided
        // without --detect-git (remote config is the source of truth for integrations,
        // so an auto-detected local provider would almost always be stale/wrong).
        let ctx_name = context_name.unwrap_or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
                .unwrap_or_else(|| "default".to_string())
        });
        InitOptions {
            context_name: Some(ctx_name),
            ..Default::default()
        }
    } else if yes {
        collect_options_auto(context_name)?
    } else {
        collect_options_interactive(context_name)?
    };

    // Save proxy_name for agent registrations before it gets consumed
    // Only use custom name if user explicitly provided --proxy-name
    let claude_server_name = proxy_name.clone();
    let kimi_server_name = proxy_name.clone();
    let codex_server_name = proxy_name.clone();
    let copilot_server_name = proxy_name.clone();
    let gemini_server_name = proxy_name.clone();
    let opencode_server_name = proxy_name.clone();
    let forge_server_name = proxy_name.clone();

    // Add proxy configuration if provided
    if let Some(url) = proxy_url {
        let name = proxy_name.unwrap_or_else(|| "proxy".to_string());
        let transport = proxy_transport
            .unwrap_or(TransportType::StreamableHttp)
            .as_str()
            .to_string();

        // Determine token key: use provided or generate default
        let token_key = if proxy_token.is_some() || proxy_token_key.is_some() {
            Some(proxy_token_key.unwrap_or_else(|| format!("proxy.{}.token", name)))
        } else {
            None
        };

        // Add token to tokens list if provided (before moving token_key)
        if let Some(token_value) = proxy_token {
            // token_key is guaranteed to be Some here since proxy_token.is_some()
            let key = token_key.clone().unwrap();
            options.tokens.push((key, token_value));
        }

        // Determine auth type
        let auth_type = proxy_auth_type
            .map(|a| a.as_str().to_string())
            .unwrap_or_else(|| {
                if token_key.is_some() {
                    AuthType::Bearer.as_str().to_string()
                } else {
                    AuthType::None.as_str().to_string()
                }
            });

        options.proxy = Some(ProxyMcpServerConfig {
            name,
            url,
            auth_type,
            token_key,
            tool_prefix: None,
            transport,
            routing: None,
        });
    }

    // Add remote config settings if provided
    if let Some(url) = remote_config_url {
        let token_key = if let Some(token_value) = remote_config_token {
            let key = "remote_config.token".to_string();
            options.tokens.push((key.clone(), token_value));
            Some(key)
        } else {
            None
        };
        options.remote_config = Some(devboy_core::RemoteConfigSettings {
            url: Some(url),
            token_key,
        });
    }

    // Build configuration
    let config = build_config(&options);
    let mut toml_content =
        toml::to_string_pretty(&config).context("Failed to serialize configuration")?;

    // When `build_config` produced an empty TOML (no providers detected
    // and the user did not opt into any proxy / remote-config settings),
    // serialize_with-skip leaves us with a zero-byte file. Seed it with a
    // short header comment so `cat .devboy.toml` is not blank and so that
    // `doctor` / `config list` have something to key off of. The content
    // is valid TOML (comments only) and does not change behaviour.
    if toml_content.trim().is_empty() {
        toml_content = minimal_devboy_toml_template();
    }

    // Dry-run mode: just print what would be done
    if dry_run {
        println!("[dry-run] Would create: {}", config_path.display());
        println!();
        println!("{}", toml_content);

        if !options.tokens.is_empty() {
            println!();
            println!(
                "[dry-run] Would store {} token(s) in keychain:",
                options.tokens.len()
            );
            for (key, _) in &options.tokens {
                println!("  - {}", key);
            }
        }

        if claude {
            println!();
            println!("[dry-run] Would register devboy MCP server in Claude Code");
        }

        if kimi {
            println!();
            println!("[dry-run] Would register devboy MCP server in Kimi CLI");
        }

        if codex_cli {
            println!();
            println!("[dry-run] Would register devboy MCP server in Codex CLI");
        }

        if copilot {
            println!();
            println!("[dry-run] Would register devboy MCP server in Copilot CLI");
        }

        if gemini {
            println!();
            println!("[dry-run] Would register devboy MCP server in Gemini CLI");
        }

        if opencode {
            println!();
            println!("[dry-run] Would register devboy MCP server in OpenCode");
        }

        if forge {
            println!();
            println!("[dry-run] Would register devboy MCP server in ForgeCode");
        }

        return Ok(());
    }

    // Create backup if forcing overwrite
    if config_path.exists() && force {
        let backup_path = create_backup(&config_path)?;
        println!("Created backup: {}", backup_path.display());
    }

    // Write configuration file
    std::fs::write(&config_path, &toml_content).context("Failed to write configuration file")?;
    println!("Created: {}", config_path.display());

    // Store tokens in keychain
    if !options.tokens.is_empty() {
        let store = get_credential_store_for_init();
        for (key, value) in &options.tokens {
            store
                .store(key, value)
                .with_context(|| format!("Failed to store {} in keychain", key))?;
            println!("Stored {} in keychain", key);
        }
    }

    // Register with Claude Code
    if claude {
        // Use --proxy-name if explicitly provided by user, otherwise default to "devboy"
        // Note: We use claude_server_name (saved before proxy_name was consumed) because
        // options.proxy.name defaults to "proxy" when --proxy is used without --proxy-name
        let server_name = claude_server_name.unwrap_or_else(|| "devboy".to_string());
        register_claude_mcp(&server_name).await?;
    }

    // Register with Kimi CLI
    if kimi {
        let server_name = kimi_server_name.unwrap_or_else(|| "devboy".to_string());
        register_kimi_mcp(&server_name)?;
    }

    // Register with Codex CLI
    if codex_cli {
        let server_name = codex_server_name.unwrap_or_else(|| "devboy".to_string());
        register_codex_mcp(&server_name)?;
    }

    // Register with Copilot CLI
    if copilot {
        let server_name = copilot_server_name.unwrap_or_else(|| "devboy".to_string());
        register_copilot_mcp(&server_name)?;
    }

    // Register with Gemini CLI
    if gemini {
        let server_name = gemini_server_name.unwrap_or_else(|| "devboy".to_string());
        register_gemini_mcp(&server_name)?;
    }

    // Register with OpenCode
    if opencode {
        let server_name = opencode_server_name.unwrap_or_else(|| "devboy".to_string());
        register_opencode_mcp(&server_name)?;
    }

    // Register with ForgeCode
    if forge {
        let server_name = forge_server_name.unwrap_or_else(|| "devboy".to_string());
        register_forge_mcp(&server_name)?;
    }

    println!();
    println!("Initialization complete!");
    println!();
    println!("Next steps:");
    println!("  1. Review the configuration: cat {}", INIT_CONFIG_FILE);
    println!("  2. Test connection: devboy test <provider>");
    println!("  3. Start using: devboy mcp");

    Ok(())
}

/// Collect options automatically from git remote.
fn collect_options_auto(context_name: Option<String>) -> Result<InitOptions> {
    // Derive context name from directory if not provided
    let ctx_name = context_name.unwrap_or_else(|| {
        std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
            .unwrap_or_else(|| "default".to_string())
    });

    let mut options = InitOptions {
        context_name: Some(ctx_name),
        ..Default::default()
    };

    // Try to detect GitHub/GitLab from git remote
    if let Some((provider, owner, repo)) = detect_git_remote() {
        match provider.as_str() {
            "github" => {
                options.github = Some(GitHubConfig {
                    owner,
                    repo,
                    base_url: None,
                });
                println!(
                    "Detected GitHub repository: {}/{}",
                    options.github.as_ref().unwrap().owner,
                    options.github.as_ref().unwrap().repo
                );
            }
            "gitlab" => {
                let project_id = format!("{}/{}", owner, repo);
                options.gitlab = Some(GitLabConfig {
                    url: "https://gitlab.com".to_string(),
                    project_id,
                });
                println!(
                    "Detected GitLab repository: {}",
                    options.gitlab.as_ref().unwrap().project_id
                );
            }
            _ => {}
        }
    } else {
        println!("No git remote detected. Creating minimal config.");
    }

    Ok(options)
}

/// Collect options interactively from user.
fn collect_options_interactive(context_name: Option<String>) -> Result<InitOptions> {
    let mut options = InitOptions::default();

    println!("DevBoy tools configuration setup");
    println!("================================");
    println!();

    // Context name
    let default_context = context_name.unwrap_or_else(|| {
        // Try to derive from directory name
        std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_string()))
            .unwrap_or_else(|| "default".to_string())
    });

    let ctx_name: String = Input::new()
        .with_prompt("Context name")
        .default(default_context)
        .interact_text()
        .context("Failed to read context name")?;

    options.context_name = Some(ctx_name.clone());

    // Provider selection
    let providers = vec!["GitHub", "GitLab", "ClickUp", "Jira"];
    let defaults = detect_provider_defaults();

    let selections = MultiSelect::new()
        .with_prompt("Select providers to configure (Space to toggle, Enter to confirm)")
        .items(&providers)
        .defaults(&defaults)
        .interact()
        .context("Provider selection cancelled")?;

    println!();

    // Configure selected providers
    for idx in selections {
        match providers[idx] {
            "GitHub" => {
                options.github = Some(configure_github_interactive(&ctx_name)?);
                if let Some(token) = prompt_token("GitHub", "github.token")? {
                    options.tokens.push(("github.token".to_string(), token));
                }
            }
            "GitLab" => {
                options.gitlab = Some(configure_gitlab_interactive(&ctx_name)?);
                if let Some(token) = prompt_token("GitLab", "gitlab.token")? {
                    options.tokens.push(("gitlab.token".to_string(), token));
                }
            }
            "ClickUp" => {
                options.clickup = Some(configure_clickup_interactive()?);
                if let Some(token) = prompt_token("ClickUp", "clickup.token")? {
                    options.tokens.push(("clickup.token".to_string(), token));
                }
            }
            "Jira" => {
                options.jira = Some(configure_jira_interactive()?);
                if let Some(token) = prompt_token("Jira API", "jira.token")? {
                    options.tokens.push(("jira.token".to_string(), token));
                }
            }
            _ => {}
        }
        println!();
    }

    Ok(options)
}

fn configure_github_interactive(_context_name: &str) -> Result<GitHubConfig> {
    println!("GitHub Configuration");
    println!("--------------------");

    // Try to detect from git remote
    let (default_owner, default_repo) = detect_git_remote()
        .filter(|(p, _, _)| p == "github")
        .map(|(_, o, r)| (o, r))
        .unwrap_or_default();

    let owner: String = Input::new()
        .with_prompt("Repository owner")
        .default(default_owner)
        .interact_text()
        .context("Failed to read owner")?;

    let repo: String = Input::new()
        .with_prompt("Repository name")
        .default(default_repo.clone())
        .interact_text()
        .context("Failed to read repo")?;

    let use_enterprise = Confirm::new()
        .with_prompt("Use GitHub Enterprise?")
        .default(false)
        .interact()
        .context("Failed to read enterprise choice")?;

    let base_url = if use_enterprise {
        let url: String = Input::new()
            .with_prompt("GitHub Enterprise API URL")
            .interact_text()
            .context("Failed to read enterprise URL")?;
        Some(url)
    } else {
        None
    };

    Ok(GitHubConfig {
        owner,
        repo,
        base_url,
    })
}

fn configure_gitlab_interactive(_context_name: &str) -> Result<GitLabConfig> {
    println!("GitLab Configuration");
    println!("--------------------");

    // Try to detect from git remote
    let default_project = detect_git_remote()
        .filter(|(p, _, _)| p == "gitlab")
        .map(|(_, o, r)| format!("{}/{}", o, r))
        .unwrap_or_default();

    let url: String = Input::new()
        .with_prompt("GitLab URL")
        .default("https://gitlab.com".to_string())
        .interact_text()
        .context("Failed to read GitLab URL")?;

    let project_id: String = Input::new()
        .with_prompt("Project ID (numeric or path like 'owner/repo')")
        .default(default_project)
        .interact_text()
        .context("Failed to read project ID")?;

    Ok(GitLabConfig { url, project_id })
}

fn configure_clickup_interactive() -> Result<ClickUpConfig> {
    println!("ClickUp Configuration");
    println!("---------------------");

    let list_id: String = Input::new()
        .with_prompt("List ID")
        .interact_text()
        .context("Failed to read list ID")?;

    let has_team_id = Confirm::new()
        .with_prompt("Configure Team ID? (required for custom task IDs like DEV-123)")
        .default(true)
        .interact()
        .context("Failed to read team ID choice")?;

    let team_id = if has_team_id {
        let id: String = Input::new()
            .with_prompt("Team ID")
            .interact_text()
            .context("Failed to read team ID")?;
        Some(id)
    } else {
        None
    };

    Ok(ClickUpConfig { list_id, team_id })
}

fn configure_jira_interactive() -> Result<JiraConfig> {
    println!("Jira Configuration");
    println!("------------------");

    let url: String = Input::new()
        .with_prompt("Jira URL (e.g., https://company.atlassian.net)")
        .interact_text()
        .context("Failed to read Jira URL")?;

    let project_key: String = Input::new()
        .with_prompt("Project key (e.g., PROJ)")
        .interact_text()
        .context("Failed to read project key")?;

    let email: String = Input::new()
        .with_prompt("Your email (for authentication)")
        .interact_text()
        .context("Failed to read email")?;

    Ok(JiraConfig {
        url,
        project_key,
        email,
    })
}

fn prompt_token(provider_name: &str, key_name: &str) -> Result<Option<String>> {
    // Check if token already exists
    let store = get_credential_store_for_init();
    if store.exists(key_name) {
        let overwrite = Confirm::new()
            .with_prompt(format!(
                "{} token already exists in keychain. Overwrite?",
                provider_name
            ))
            .default(false)
            .interact()
            .context("Failed to read overwrite choice")?;

        if !overwrite {
            return Ok(None);
        }
    }

    let store_token = Confirm::new()
        .with_prompt(format!("Store {} token in keychain?", provider_name))
        .default(true)
        .interact()
        .context("Failed to read token choice")?;

    if !store_token {
        return Ok(None);
    }

    let token: String = Password::new()
        .with_prompt(format!("{} token", provider_name))
        .interact()
        .context("Failed to read token")?;

    if token.is_empty() {
        return Ok(None);
    }

    Ok(Some(token))
}

/// Detect git remote and return (provider, owner, repo).
fn detect_git_remote() -> Option<(String, String, String)> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_git_url(&url)
}

/// Parse git URL to extract provider, owner, and repo.
fn parse_git_url(url: &str) -> Option<(String, String, String)> {
    // Handle SSH format: git@github.com:owner/repo.git
    if let Some(rest) = url.strip_prefix("git@") {
        let parts: Vec<&str> = rest.splitn(2, ':').collect();
        if parts.len() == 2 {
            let host = parts[0];
            let path = parts[1].trim_end_matches(".git");
            let path_parts: Vec<&str> = path.splitn(2, '/').collect();
            if path_parts.len() == 2 {
                let provider = if host.contains("github") {
                    "github"
                } else if host.contains("gitlab") {
                    "gitlab"
                } else {
                    return None;
                };
                return Some((
                    provider.to_string(),
                    path_parts[0].to_string(),
                    path_parts[1].to_string(),
                ));
            }
        }
    }

    // Handle HTTPS format: https://github.com/owner/repo.git
    if url.starts_with("https://") || url.starts_with("http://") {
        let without_proto = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))?;

        let parts: Vec<&str> = without_proto.splitn(2, '/').collect();
        if parts.len() == 2 {
            let host = parts[0];
            let path = parts[1].trim_end_matches(".git");
            let path_parts: Vec<&str> = path.splitn(2, '/').collect();
            if path_parts.len() == 2 {
                let provider = if host.contains("github") {
                    "github"
                } else if host.contains("gitlab") {
                    "gitlab"
                } else {
                    return None;
                };
                return Some((
                    provider.to_string(),
                    path_parts[0].to_string(),
                    path_parts[1].to_string(),
                ));
            }
        }
    }

    None
}

/// Detect which providers should be pre-selected based on git remote.
fn detect_provider_defaults() -> Vec<bool> {
    let detected = detect_git_remote();
    let is_github = detected
        .as_ref()
        .map(|(p, _, _)| p == "github")
        .unwrap_or(false);
    let is_gitlab = detected
        .as_ref()
        .map(|(p, _, _)| p == "gitlab")
        .unwrap_or(false);

    // [GitHub, GitLab, ClickUp, Jira]
    vec![is_github, is_gitlab, false, false]
}

/// Decide whether `devboy init` should skip local git remote auto-detection and
/// fall through to a minimal-config branch instead of calling `collect_options_auto`
/// (for `--yes`) or `collect_options_interactive` (for the interactive path).
///
/// Git detection is skipped when any of the following holds:
/// - `--proxy-only` was passed (explicit opt-out, pre-existing behaviour);
/// - `--remote-config-url` is present and `--detect-git` is not (remote config is
///   the source of truth for integrations, so a local auto-detected provider would
///   almost always be stale or conflict).
fn should_skip_git_detect(
    proxy_only: bool,
    remote_config_url: Option<&str>,
    detect_git: bool,
) -> bool {
    if proxy_only {
        return true;
    }
    remote_config_url.is_some() && !detect_git
}

/// Placeholder `.devboy.toml` content used when `init --yes` is run in
/// a directory without a detectable git remote and without any
/// agent / proxy / remote-config flag. The file is valid TOML (only
/// comments), so subsequent `config set` writes drop real keys into it
/// without any cleanup needed.
fn minimal_devboy_toml_template() -> String {
    "# DevBoy tools configuration\n\
     #\n\
     # No providers were detected from git or passed on the command line.\n\
     # Add one with:\n\
     #   devboy config set github.owner <owner>\n\
     #   devboy config set github.repo <repo>\n\
     #   devboy config set-secret github.token <token>\n\
     #\n\
     # Or, register an agent MCP server via:\n\
     #   devboy init --claude | --kimi | --codex-cli | --copilot | --gemini | --opencode | --forge\n\
     #\n\
     # See `devboy --help` for the full command surface.\n"
        .to_string()
}

/// Build Config from collected options.
fn build_config(options: &InitOptions) -> Config {
    let mut config = Config::default();

    let context_name = options
        .context_name
        .clone()
        .unwrap_or_else(|| "default".to_string());

    // If we have any providers, add them to a context
    if options.github.is_some()
        || options.gitlab.is_some()
        || options.clickup.is_some()
        || options.jira.is_some()
        || options.slack.is_some()
    {
        let context = ContextConfig {
            github: options.github.clone(),
            gitlab: options.gitlab.clone(),
            clickup: options.clickup.clone(),
            jira: options.jira.clone(),
            fireflies: None,
            slack: options.slack.clone(),
        };
        config.contexts.insert(context_name, context);
    }

    // Add proxy configuration if provided
    if let Some(proxy) = &options.proxy {
        config.proxy_mcp_servers.push(proxy.clone());
    }

    // Add remote config settings if provided
    if let Some(remote_config) = &options.remote_config {
        config.remote_config = Some(remote_config.clone());
    }

    config
}

/// Create a timestamped backup of existing config.
fn create_backup(path: &PathBuf) -> Result<PathBuf> {
    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let backup_name = format!(".devboy.toml.backup.{}", timestamp);
    let backup_path = path.with_file_name(backup_name);

    std::fs::copy(path, &backup_path).context("Failed to create backup")?;

    Ok(backup_path)
}

/// Register MCP server in Claude Code with specified name.
///
/// The server will run `devboy mcp` command but can be registered under any name.
///
/// # Arguments
/// * `server_name` - Name to register the MCP server as (e.g., "devboy", "my-custom-server")
async fn register_claude_mcp(server_name: &str) -> Result<()> {
    println!("Registering '{}' MCP server in Claude Code...", server_name);

    // Try using claude CLI first
    let claude_result = Command::new("claude")
        .args(["mcp", "add", server_name, "--", "devboy", "mcp"])
        .status();

    match claude_result {
        Ok(status) if status.success() => {
            println!("Successfully registered via Claude CLI");
            return Ok(());
        }
        _ => {
            // Fall back to editing ~/.claude.json directly
            register_claude_mcp_direct(server_name)?;
        }
    }

    Ok(())
}

/// Register MCP server by editing ~/.claude.json directly.
///
/// Fallback method when `claude` CLI is not available.
/// The server will run `devboy mcp` command but can be registered under any name.
///
/// # Arguments
/// * `server_name` - Name to register the MCP server as (e.g., "devboy", "my-custom-server")
fn register_claude_mcp_direct(server_name: &str) -> Result<()> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let claude_config_path = home.join(".claude.json");

    register_claude_mcp_to_path(server_name, &claude_config_path)?;

    println!("Successfully registered in ~/.claude.json");
    Ok(())
}

/// Internal helper to register MCP server to a JSON config file.
/// Used by production code and tests for all JSON-based MCP configs.
///
/// # Arguments
/// * `server_name` - Name to register the MCP server as
/// * `config_path` - Path to the JSON config file
/// * `container_key` - Top-level key that holds servers (e.g., "mcpServers")
/// * `server_entry` - JSON value to insert for this server
/// * `config_label` - Human-readable config name for error messages
fn register_json_mcp_config(
    server_name: &str,
    config_path: &std::path::Path,
    container_key: &str,
    server_entry: serde_json::Value,
    config_label: &str,
) -> Result<()> {
    let mut config: serde_json::Value = if config_path.exists() {
        let content = std::fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read {} config", config_label))?;
        let parsed: serde_json::Value = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {} config", config_label))?;

        if !parsed.is_object() {
            anyhow::bail!(
                "{} config exists but is not a JSON object. \
                 Please fix it manually or delete it.",
                config_label
            );
        }
        parsed
    } else {
        serde_json::json!({})
    };

    match config.get(container_key) {
        Some(servers) if !servers.is_object() => {
            anyhow::bail!(
                "{} config has '{}' but it's not an object. \
                 Please fix it manually.",
                config_label,
                container_key
            );
        }
        None => {
            config[container_key] = serde_json::json!({});
        }
        _ => {}
    }

    config[container_key][server_name] = server_entry;

    let content = serde_json::to_string_pretty(&config)
        .with_context(|| format!("Failed to serialize {} config", config_label))?;
    std::fs::write(config_path, content)
        .with_context(|| format!("Failed to write {} config", config_label))?;

    Ok(())
}

/// Internal helper to register MCP server to a specific Claude config path.
/// Used by both production code and tests.
fn register_claude_mcp_to_path(server_name: &str, config_path: &std::path::Path) -> Result<()> {
    register_json_mcp_config(
        server_name,
        config_path,
        "mcpServers",
        serde_json::json!({ "command": "devboy", "args": ["mcp"] }),
        "claude",
    )
}

/// Register MCP server in Kimi CLI by editing .kimi/mcp.json in the current directory.
///
/// Kimi CLI supports project-level MCP configuration via `.kimi/mcp.json`.
/// The server will run `devboy mcp` command but can be registered under any name.
///
/// # Arguments
/// * `server_name` - Name to register the MCP server as (e.g., "devboy", "my-custom-server")
fn register_kimi_mcp(server_name: &str) -> Result<()> {
    println!("Registering '{}' MCP server in Kimi CLI...", server_name);

    let kimi_config_path = std::env::current_dir()
        .context("Could not determine current directory")?
        .join(".kimi")
        .join("mcp.json");

    // Ensure .kimi directory exists
    if let Some(parent) = kimi_config_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create .kimi directory")?;
    }

    register_kimi_mcp_to_path(server_name, &kimi_config_path)?;

    println!("Successfully registered in .kimi/mcp.json");
    Ok(())
}

/// Internal helper to register MCP server to a specific Kimi config path.
/// Used by both production code and tests.
fn register_kimi_mcp_to_path(server_name: &str, config_path: &std::path::Path) -> Result<()> {
    register_json_mcp_config(
        server_name,
        config_path,
        "mcpServers",
        serde_json::json!({ "command": "devboy", "args": ["mcp"] }),
        "kimi",
    )
}

/// Register MCP server in Codex CLI.
///
/// Tries `codex mcp add` first, then falls back to editing `~/.codex/config.toml`.
fn register_codex_mcp(server_name: &str) -> Result<()> {
    println!("Registering '{}' MCP server in Codex CLI...", server_name);

    // Allow tests to force the fallback path for determinism
    if std::env::var("DEVBOY_NO_NATIVE_MCP").is_ok() {
        register_codex_mcp_direct(server_name)?;
        return Ok(());
    }

    let codex_result = Command::new("codex")
        .args(["mcp", "add", server_name, "--", "devboy", "mcp"])
        .stdin(std::process::Stdio::null())
        .status();

    match codex_result {
        Ok(status) if status.success() => {
            println!("Successfully registered via Codex CLI");
            return Ok(());
        }
        _ => {
            register_codex_mcp_direct(server_name)?;
        }
    }

    Ok(())
}

/// Fallback method: edit `~/.codex/config.toml` directly.
fn register_codex_mcp_direct(server_name: &str) -> Result<()> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let config_path = home.join(".codex").join("config.toml");
    register_codex_mcp_to_path(server_name, &config_path)?;
    println!("Successfully registered in {}", config_path.display());
    Ok(())
}

/// Internal helper to register Codex MCP to a specific config path.
/// Used by both production code and tests.
fn register_codex_mcp_to_path(server_name: &str, config_path: &std::path::Path) -> Result<()> {
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create .codex directory")?;
    }

    let mut config: toml::Value = if config_path.exists() {
        let content =
            std::fs::read_to_string(config_path).context("Failed to read codex config")?;
        content.parse().context("Failed to parse codex config")?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };

    let table = config
        .as_table_mut()
        .context("Codex config is not a TOML table")?;

    let mcp_servers = table
        .entry("mcp_servers")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let mcp_table = mcp_servers
        .as_table_mut()
        .context("Codex config 'mcp_servers' is not a table")?;

    let mut server_table = toml::map::Map::new();
    server_table.insert(
        "command".to_string(),
        toml::Value::String("devboy".to_string()),
    );
    let args = vec![toml::Value::String("mcp".to_string())];
    server_table.insert("args".to_string(), toml::Value::Array(args));

    mcp_table.insert(server_name.to_string(), toml::Value::Table(server_table));

    let content = toml::to_string_pretty(&config).context("Failed to serialize codex config")?;
    std::fs::write(config_path, content).context("Failed to write codex config")?;

    Ok(())
}

/// Register MCP server in Copilot CLI by editing `~/.copilot/mcp-config.json`.
fn register_copilot_mcp(server_name: &str) -> Result<()> {
    println!("Registering '{}' MCP server in Copilot CLI...", server_name);

    let home = dirs::home_dir().context("Could not determine home directory")?;
    let config_path = home.join(".copilot").join("mcp-config.json");
    register_copilot_mcp_to_path(server_name, &config_path)?;

    println!("Successfully registered in {}", config_path.display());
    Ok(())
}

/// Internal helper to register Copilot MCP to a specific config path.
/// Used by both production code and tests.
fn register_copilot_mcp_to_path(server_name: &str, config_path: &std::path::Path) -> Result<()> {
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create .copilot directory")?;
    }

    register_json_mcp_config(
        server_name,
        config_path,
        "mcpServers",
        serde_json::json!({
            "type": "local",
            "command": "devboy",
            "args": ["mcp"],
            "tools": ["*"]
        }),
        "copilot",
    )
}

/// Register MCP server in Gemini CLI.
///
/// Tries `gemini mcp add --trust` first, then falls back to `.gemini/settings.json`.
fn register_gemini_mcp(server_name: &str) -> Result<()> {
    println!("Registering '{}' MCP server in Gemini CLI...", server_name);

    // Allow tests to force the fallback path for determinism
    if std::env::var("DEVBOY_NO_NATIVE_MCP").is_ok() {
        register_gemini_mcp_fallback(server_name)?;
        return Ok(());
    }

    let gemini_result = Command::new("gemini")
        .args(["mcp", "add", "--trust", server_name, "devboy", "mcp"])
        .stdin(std::process::Stdio::null())
        .status();

    match gemini_result {
        Ok(status) if status.success() => {
            println!("Successfully registered via Gemini CLI");
            return Ok(());
        }
        _ => {
            register_gemini_mcp_fallback(server_name)?;
        }
    }

    Ok(())
}

fn register_gemini_mcp_fallback(server_name: &str) -> Result<()> {
    let config_path = std::env::current_dir()
        .context("Could not determine current directory")?
        .join(".gemini")
        .join("settings.json");

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create .gemini directory")?;
    }

    register_json_mcp_config(
        server_name,
        &config_path,
        "mcpServers",
        serde_json::json!({
            "command": "devboy",
            "args": ["mcp"],
            "trust": true
        }),
        "gemini",
    )?;

    println!("Successfully registered in .gemini/settings.json");

    Ok(())
}

/// Register MCP server in OpenCode by editing `opencode.json` in the current directory.
fn register_opencode_mcp(server_name: &str) -> Result<()> {
    println!("Registering '{}' MCP server in OpenCode...", server_name);

    let config_path = std::env::current_dir()
        .context("Could not determine current directory")?
        .join("opencode.json");

    register_opencode_mcp_to_path(server_name, &config_path)?;

    println!("Successfully registered in opencode.json");
    Ok(())
}

/// Internal helper to register OpenCode MCP to a specific config path.
/// Used by both production code and tests.
fn register_opencode_mcp_to_path(server_name: &str, config_path: &std::path::Path) -> Result<()> {
    let mut config: serde_json::Value = if config_path.exists() {
        let content =
            std::fs::read_to_string(config_path).context("Failed to read opencode config")?;
        let parsed: serde_json::Value =
            serde_json::from_str(&content).context("Failed to parse opencode config")?;

        if !parsed.is_object() {
            anyhow::bail!(
                "OpenCode config exists but is not a JSON object. \
                 Please fix it manually or delete it."
            );
        }
        parsed
    } else {
        serde_json::json!({})
    };

    match config.get("mcp") {
        Some(mcp) if !mcp.is_object() => {
            anyhow::bail!(
                "OpenCode config has 'mcp' but it's not an object. \
                 Please fix it manually."
            );
        }
        None => {
            config["mcp"] = serde_json::json!({});
        }
        _ => {}
    }

    config["mcp"][server_name] = serde_json::json!({
        "type": "local",
        "command": "devboy",
        "args": ["mcp"]
    });

    let content =
        serde_json::to_string_pretty(&config).context("Failed to serialize opencode config")?;
    std::fs::write(config_path, content).context("Failed to write opencode config")?;

    Ok(())
}

/// Register MCP server in ForgeCode by editing `.mcp.json` in the current directory.
fn register_forge_mcp(server_name: &str) -> Result<()> {
    println!("Registering '{}' MCP server in ForgeCode...", server_name);

    let config_path = std::env::current_dir()
        .context("Could not determine current directory")?
        .join(".mcp.json");

    register_forge_mcp_to_path(server_name, &config_path)?;

    println!("Successfully registered in .mcp.json");
    Ok(())
}

/// Internal helper to register ForgeCode MCP to a specific config path.
/// Used by both production code and tests.
fn register_forge_mcp_to_path(server_name: &str, config_path: &std::path::Path) -> Result<()> {
    register_json_mcp_config(
        server_name,
        config_path,
        "mcpServers",
        serde_json::json!({ "command": "devboy", "args": ["mcp"] }),
        "forge",
    )
}

// =============================================================================
// Config Commands
// =============================================================================

fn handle_config_command(command: ConfigCommands) -> Result<()> {
    match command {
        ConfigCommands::Set { key, value } => {
            let mut config = Config::load().context("Failed to load config")?;
            config
                .set(&key, &value)
                .context("Failed to set config value")?;
            config.save().context("Failed to save config")?;
            println!("Set {} = {}", key, value);
        }

        ConfigCommands::SetSecret { key, value } => {
            let store = get_credential_store();
            store
                .store(&key, &value)
                .context("Failed to store secret")?;
            println!("Secret {} stored in keychain", key);
        }

        ConfigCommands::Get { key } => {
            // `config get` supports two key shapes:
            //   1. Structured config paths — `github.owner`, `proxy.routing.strategy`, …
            //      For these `Config::get` knows the schema: a known field returns
            //      `Ok(Some|None)`, a typo returns `Err("Unknown …")`. We want the user
            //      to notice the typo, so we surface that `Err` instead of a silent
            //      keychain fallback.
            //   2. Arbitrary keychain keys (`proxy.cloud.token`, `github.token`, …).
            //      For these `Config::get` returns `Err("Invalid config key …")` and we
            //      honestly fall through to the keychain — that's where secrets live.
            let config = Config::load().context("Failed to load config")?;
            let is_structured_path = is_structured_config_path(&key);
            match config.get(&key) {
                Ok(Some(value)) => {
                    println!("{}", value);
                    return Ok(());
                }
                Ok(None) => {
                    // Known field, just not populated — fall through to keychain lookup
                    // for backward compat with secret keys that share the `provider.field`
                    // shape (e.g., `github.token` is stored in keychain, not config).
                }
                Err(e) if is_structured_path => {
                    // Typo in a known structured path (e.g., `proxy.routing.nonexistent`) —
                    // keychain fallback would silently mask the mistake. Surface it.
                    return Err(anyhow::Error::msg(e.to_string()));
                }
                Err(_) => {
                    // Unknown key shape — legitimate keychain lookup (arbitrary 3-part
                    // token paths that aren't part of any config schema).
                }
            }

            // Then try keychain (supports arbitrary key formats like "proxy.name.field")
            let store = get_credential_store();
            if let Some(value) = store.get(&key).ok().flatten() {
                println!("{} (from keychain)", mask_secret(&value));
                return Ok(());
            }

            println!("(not set)");
        }

        ConfigCommands::List => {
            // Match `tools call` / `test` / `context list` — prefer a
            // repo-local `.devboy.toml` over the global config so
            // `config list` reports the config that will actually be
            // used by the rest of the CLI in this cwd.
            let (config, source_path) = load_runtime_config().context("Failed to load config")?;
            println!("Configuration (source: {}):", source_path.display());
            println!();
            let store = get_credential_store();

            // GitHub
            if let Some(gh) = &config.github {
                println!("[github]");
                println!("  owner = {}", gh.owner);
                println!("  repo = {}", gh.repo);
                if let Some(url) = &gh.base_url {
                    println!("  base_url = {}", url);
                }
                if store.exists("github.token") {
                    println!("  token = ******* (in keychain)");
                } else {
                    println!("  token = (not set)");
                }
                println!();
            }

            // GitLab
            if let Some(gl) = &config.gitlab {
                println!("[gitlab]");
                println!("  url = {}", gl.url);
                println!("  project_id = {}", gl.project_id);
                if store.exists("gitlab.token") {
                    println!("  token = ******* (in keychain)");
                } else {
                    println!("  token = (not set)");
                }
                println!();
            }

            // ClickUp
            if let Some(cu) = &config.clickup {
                println!("[clickup]");
                println!("  list_id = {}", cu.list_id);
                if let Some(team_id) = &cu.team_id {
                    println!("  team_id = {}", team_id);
                } else {
                    println!("  team_id = (not set, recommended for custom task IDs)");
                }
                if store.exists("clickup.token") {
                    println!("  token = ******* (in keychain)");
                } else {
                    println!("  token = (not set)");
                }
                println!();
            }

            // Jira
            if let Some(jira) = &config.jira {
                println!("[jira]");
                println!("  url = {}", jira.url);
                println!("  project_key = {}", jira.project_key);
                println!("  email = {}", jira.email);
                if store.exists("jira.token") {
                    println!("  token = ******* (in keychain)");
                } else {
                    println!("  token = (not set)");
                }
                println!();
            }

            if let Some(slack) = &config.slack {
                println!("[slack]");
                if let Some(team_id) = &slack.team_id {
                    println!("  team_id = {}", team_id);
                }
                if let Some(workspace) = &slack.workspace {
                    println!("  workspace = {}", workspace);
                }
                if let Some(base_url) = &slack.base_url {
                    println!("  base_url = {}", base_url);
                }
                if let Some(client_id) = &slack.client_id {
                    println!("  client_id = {}", client_id);
                }
                if let Some(redirect_uri) = &slack.redirect_uri {
                    println!("  redirect_uri = {}", redirect_uri);
                }
                println!("  required_scopes = {}", slack.required_scopes.join(", "));
                if store.exists("slack.token") {
                    println!("  token = ******* (in keychain)");
                } else {
                    println!("  token = (not set)");
                }
                println!();
            }

            // Transparent proxy configuration. Shown whenever any proxy-related value
            // deviates from defaults so users can audit their setup with a single
            // command. Identical ordering with the TOML file for quick cross-reference.
            if !config.proxy.is_default() {
                let routing = &config.proxy.routing;
                let secrets = &config.proxy.secrets;
                let telemetry = &config.proxy.telemetry;

                if !routing.is_default() {
                    println!("[proxy.routing]");
                    println!("  strategy = {}", routing_strategy_slug(routing.strategy));
                    println!("  fallback_on_error = {}", routing.fallback_on_error);
                    for rule in &routing.tool_overrides {
                        println!(
                            "  override: {} → {}",
                            rule.pattern,
                            routing_strategy_slug(rule.strategy)
                        );
                    }
                    println!();
                }

                if !secrets.is_default() {
                    println!("[proxy.secrets]");
                    println!("  cache_ttl_secs = {}", secrets.cache_ttl_secs);
                    println!();
                }

                if !telemetry.is_default() {
                    println!("[proxy.telemetry]");
                    println!("  enabled = {}", telemetry.enabled);
                    if let Some(ep) = &telemetry.endpoint {
                        println!("  endpoint = {}", ep);
                    }
                    if let Some(tk) = &telemetry.token_key {
                        println!("  token_key = {}", tk);
                    }
                    println!("  batch_size = {}", telemetry.batch_size);
                    println!("  batch_interval_secs = {}", telemetry.batch_interval_secs);
                    println!("  offline_queue_max = {}", telemetry.offline_queue_max);
                    println!();
                }
            }

            if !config.has_any_provider() && config.proxy.is_default() {
                println!("No providers configured.");
                println!();
                println!("To configure GitHub:");
                println!("  devboy config set github.owner <owner>");
                println!("  devboy config set github.repo <repo>");
                println!("  devboy config set-secret github.token <token>");
                println!("To configure Slack:");
                println!("  devboy config set slack.workspace <workspace>");
                println!("  devboy config set-secret slack.token <xoxb-token>");
            }
        }

        ConfigCommands::Path => {
            // Delegate to the same resolver `tools call` / `test` /
            // `context list` / `config list` all use. That way
            // `config path` cannot drift from runtime behaviour — if
            // the resolution policy ever changes (walk-up rule, new
            // env var, …), every surface picks it up automatically.
            match load_runtime_config() {
                Ok((_config, source_path)) => println!("{}", source_path.display()),
                Err(e) => println!("Error: {}", e),
            }
        }
    }

    Ok(())
}

/// Check whether `key` is a **structured config path** (known schema) rather than an
/// arbitrary keychain key. For structured paths, a `Config::get` error means a user
/// typo and we want it surfaced; for arbitrary keys, the same error is a valid signal
/// to fall back to the keychain.
///
/// Currently covers only `proxy.{routing|secrets|telemetry}.*` — this schema was
/// introduced together with the transparent proxy, so we can apply strict rules to it.
/// Provider paths (`github.*`, `gitlab.*`, …) keep the historical silent-keychain
/// fallback to avoid breaking existing users.
fn is_structured_config_path(key: &str) -> bool {
    let parts: Vec<&str> = key.split('.').collect();
    matches!(
        parts.as_slice(),
        ["proxy", "routing" | "secrets" | "telemetry", _]
    )
}

fn mask_secret(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 8 {
        "*".repeat(chars.len())
    } else {
        let prefix: String = chars[..4].iter().collect();
        let suffix: String = chars[chars.len() - 4..].iter().collect();
        format!("{prefix}...{suffix}")
    }
}

fn load_runtime_config() -> Result<(Config, PathBuf)> {
    let local_path = PathBuf::from(".devboy.toml");
    if local_path.exists() {
        let config = Config::load_from(&local_path).context("Failed to load .devboy.toml")?;
        return Ok((config, local_path));
    }

    let path = Config::config_path().context("Failed to determine config path")?;
    let config = Config::load().context("Failed to load config")?;
    Ok((config, path))
}

fn handle_context_command(command: ContextCommands) -> Result<()> {
    match command {
        ContextCommands::List => {
            let (config, source_path) = load_runtime_config()?;
            let active = config.resolve_active_context_name();
            let names = config.context_names();

            if names.is_empty() {
                println!("No contexts configured.");
                println!("Config source: {}", source_path.display());
                return Ok(());
            }

            println!("Contexts (source: {}):", source_path.display());
            for name in names {
                if active.as_deref() == Some(name.as_str()) {
                    println!("* {} (active)", name);
                } else {
                    println!("* {}", name);
                }
            }
        }
        ContextCommands::Use { name } => {
            let (mut config, source_path) = load_runtime_config()?;
            config
                .set_active_context(&name)
                .context("Failed to switch context")?;
            config
                .save_to(&source_path)
                .context("Failed to save context selection")?;
            println!(
                "Active context set to '{}' ({})",
                name,
                source_path.display()
            );
        }
    }

    Ok(())
}

// =============================================================================
// Issues Command
// =============================================================================

async fn handle_issues_command(state: &str, limit: u32) -> Result<()> {
    let (config, _) = load_runtime_config()?;
    let store = get_credential_store();

    if let Some(gh) = &config.github {
        let token = store
            .get("github.token")
            .context("Failed to get token")?
            .context("GitHub token not set. Run: devboy config set-secret github.token <token>")?;

        let client = GitHubClient::new(&gh.owner, &gh.repo, token);

        let filter = IssueFilter {
            state: Some(state.to_string()),
            limit: Some(limit),
            ..Default::default()
        };

        let issues = client
            .get_issues(filter)
            .await
            .context("Failed to fetch issues")?
            .items;

        if issues.is_empty() {
            println!("No issues found with state: {}", state);
            return Ok(());
        }

        println!("Issues ({}):", issues.len());
        println!();
        for issue in &issues {
            let labels = if issue.labels.is_empty() {
                String::new()
            } else {
                format!(" [{}]", issue.labels.join(", "))
            };
            println!("  {} - {}{}", issue.key, issue.title, labels);
        }
    } else {
        println!("No provider configured. Run: devboy config set github.owner <owner>");
    }

    Ok(())
}

// =============================================================================
// MRs Command
// =============================================================================

async fn handle_mrs_command(state: &str, limit: u32) -> Result<()> {
    let (config, _) = load_runtime_config()?;
    let store = get_credential_store();

    if let Some(gh) = &config.github {
        let token = store
            .get("github.token")
            .context("Failed to get token")?
            .context("GitHub token not set. Run: devboy config set-secret github.token <token>")?;

        let client = GitHubClient::new(&gh.owner, &gh.repo, token);

        let filter = MrFilter {
            state: Some(state.to_string()),
            limit: Some(limit),
            ..Default::default()
        };

        let prs = client
            .get_merge_requests(filter)
            .await
            .context("Failed to fetch PRs")?
            .items;

        if prs.is_empty() {
            println!("No pull requests found with state: {}", state);
            return Ok(());
        }

        println!("Pull Requests ({}):", prs.len());
        println!();
        for pr in &prs {
            let state_icon = match pr.state.as_str() {
                "opened" => "O",
                "merged" => "M",
                "closed" => "C",
                "draft" => "D",
                _ => "?",
            };
            println!(
                "  [{}] {} - {} ({} -> {})",
                state_icon, pr.key, pr.title, pr.source_branch, pr.target_branch
            );
        }
    } else {
        println!("No provider configured. Run: devboy config set github.owner <owner>");
    }

    Ok(())
}

// =============================================================================
// Test Command
// =============================================================================

async fn handle_test_command(provider: &str) -> Result<()> {
    let (config, _) = load_runtime_config()?;
    let store = get_credential_store();

    match provider {
        "github" => {
            let gh = config
                .github
                .as_ref()
                .context("GitHub not configured. Run: devboy config set github.owner <owner>")?;

            let token = store
                .get("github.token")
                .context("Failed to get token")?
                .context(
                    "GitHub token not set. Run: devboy config set-secret github.token <token>",
                )?;

            println!("Testing GitHub connection...");
            println!("  Repository: {}/{}", gh.owner, gh.repo);

            let client = GitHubClient::new(&gh.owner, &gh.repo, token);

            // Test by getting current user
            match client.get_current_user().await {
                Ok(user) => {
                    println!(
                        "  Authenticated as: {} ({})",
                        user.username,
                        user.name.unwrap_or_default()
                    );
                    println!();
                    println!("GitHub connection successful!");
                }
                Err(e) => {
                    println!("  Error: {}", e);
                    println!();
                    println!("GitHub connection failed!");
                    return Err(e.into());
                }
            }
        }

        "gitlab" => {
            let gl = config
                .gitlab
                .as_ref()
                .context("GitLab not configured. Run: devboy config set gitlab.url <url>")?;

            let token = store
                .get("gitlab.token")
                .context("Failed to get token")?
                .context(
                    "GitLab token not set. Run: devboy config set-secret gitlab.token <token>",
                )?;

            println!("Testing GitLab connection...");
            println!("  URL: {}", gl.url);
            println!("  Project: {}", gl.project_id);

            let client = GitLabClient::with_base_url(&gl.url, &gl.project_id, token);

            match client.get_current_user().await {
                Ok(user) => {
                    println!(
                        "  Authenticated as: {} ({})",
                        user.username,
                        user.name.unwrap_or_default()
                    );
                    println!();
                    println!("GitLab connection successful!");
                }
                Err(e) => {
                    println!("  Error: {}", e);
                    println!();
                    println!("GitLab connection failed!");
                    return Err(e.into());
                }
            }
        }

        "clickup" => {
            let cu = config.clickup.as_ref().context(
                "ClickUp not configured. Run: devboy config set clickup.list_id <list_id>",
            )?;

            let token = store
                .get("clickup.token")
                .context("Failed to get token")?
                .context(
                    "ClickUp token not set. Run: devboy config set-secret clickup.token <token>",
                )?;

            println!("Testing ClickUp connection...");
            println!("  List ID: {}", cu.list_id);
            if let Some(team_id) = &cu.team_id {
                println!("  Team ID: {}", team_id);
            } else {
                println!("  Team ID: (not set)");
                println!(
                    "  Hint: Set team_id for custom task IDs (e.g., DEV-42) and better integration:"
                );
                println!("    devboy config set clickup.team_id <team_id>");
            }

            let mut client = ClickUpClient::new(&cu.list_id, token);
            if let Some(team_id) = &cu.team_id {
                client = client.with_team_id(team_id);
            }

            match client.get_current_user().await {
                Ok(user) => {
                    println!(
                        "  Authenticated as: {} ({})",
                        user.username,
                        user.name.unwrap_or_default()
                    );
                    println!();
                    println!("ClickUp connection successful!");
                }
                Err(e) => {
                    println!("  Error: {}", e);
                    println!();
                    println!("ClickUp connection failed!");
                    return Err(e.into());
                }
            }
        }

        "jira" => {
            let jira = config
                .jira
                .as_ref()
                .context("Jira not configured. Run: devboy config set jira.url <url>")?;

            let token = store
                .get("jira.token")
                .context("Failed to get token")?
                .context("Jira token not set. Run: devboy config set-secret jira.token <token>")?;

            println!("Testing Jira connection...");
            println!("  URL: {}", jira.url);
            println!("  Project: {}", jira.project_key);
            println!("  Email: {}", jira.email);

            let client = JiraClient::new(&jira.url, &jira.project_key, &jira.email, token);

            match client.get_current_user().await {
                Ok(user) => {
                    println!(
                        "  Authenticated as: {} ({})",
                        user.username,
                        user.name.unwrap_or_default()
                    );
                    println!();
                    println!("Jira connection successful!");
                }
                Err(e) => {
                    println!("  Error: {}", e);
                    println!();
                    println!("Jira connection failed!");
                    return Err(e.into());
                }
            }
        }

        "slack" => {
            let slack = config
                .slack
                .as_ref()
                .context(
                    "Slack not configured. Run: devboy config set slack.team_id <team-id> or devboy config set slack.workspace <name>",
                )?;

            let token = store
                .get("slack.token")
                .context("Failed to get token")?
                .context(
                    "Slack bot token not set. Run: devboy config set-secret slack.token <xoxb-token>",
                )?;

            println!("Testing Slack connection...");
            if let Some(workspace) = &slack.workspace {
                println!("  Workspace: {}", workspace);
            }
            if let Some(team_id) = &slack.team_id {
                println!("  Team ID: {}", team_id);
            }

            let mut client =
                SlackClient::new(token).with_required_scopes(slack.required_scopes.clone());
            if let Some(base_url) = &slack.base_url {
                client = client.with_base_url(base_url);
            }

            match client.auth_info().await {
                Ok(info) => {
                    println!("  Team: {} ({})", info.team_name, info.team_id);
                    if let Some(user_name) = info.user_name.as_deref() {
                        println!("  Authenticated as: {} ({})", info.user_id, user_name);
                    } else {
                        println!("  Authenticated as: {}", info.user_id);
                    }
                    if let Some(bot_id) = info.bot_id.as_deref() {
                        println!("  Bot ID: {}", bot_id);
                    }
                    println!("  Scopes: {}", info.scopes.join(", "));
                    if !info.missing_scopes.is_empty() {
                        println!("  Missing scopes: {}", info.missing_scopes.join(", "));
                        println!();
                        println!("Slack connection failed health check!");
                        anyhow::bail!(
                            "Slack token is missing required scopes: {}",
                            info.missing_scopes.join(", ")
                        );
                    }
                    println!();
                    println!("Slack connection successful!");
                }
                Err(e) => {
                    println!("  Error: {}", e);
                    println!();
                    println!("Slack connection failed!");
                    return Err(e.into());
                }
            }
        }

        _ => {
            println!("Unknown provider: {}", provider);
            println!("Supported providers: github, gitlab, clickup, jira, slack");
        }
    }

    Ok(())
}

// =============================================================================
// MCP Command
// =============================================================================

async fn handle_mcp_command(no_config: bool) -> Result<()> {
    let mut server = McpServer::new();

    // Load config unless --no-config flag or DEVBOY_NO_CONFIG=1 is set
    let skip_config = no_config || is_no_config_enabled();
    let config = if skip_config {
        tracing::info!("Running in env-only mode, skipping config file");
        Config::default()
    } else {
        let (cfg, config_path) = load_runtime_config()?;
        tracing::debug!("Config loaded from: {}", config_path.display());
        cfg
    };

    // Build the credential store. When `[proxy.secrets].cache_ttl_secs > 0`, wrap the
    // default chain in a TTL cache so keychain hits (and UI prompts on macOS) are
    // amortized across routing decisions and telemetry flushes.
    let store = build_mcp_store(config.proxy.secrets.cache_ttl_secs);

    // Set up local providers from config (no network calls — only credential lookups).
    let mut any_provider_added = false;
    if !skip_config {
        for (context_name, context) in &config.contexts {
            server.ensure_context(context_name);
            any_provider_added |=
                add_context_providers(&mut server, store.as_ref(), context_name, context);
        }

        if !config.contexts.contains_key(Config::DEFAULT_CONTEXT_NAME)
            && let Some(default_context) = config.legacy_default_context()
        {
            any_provider_added |= add_context_providers(
                &mut server,
                store.as_ref(),
                Config::DEFAULT_CONTEXT_NAME,
                &default_context,
            );
        }

        if let Some(active) = config.resolve_active_context_name() {
            if let Err(e) = server.set_active_context(&active) {
                tracing::warn!("Could not set active context '{}': {}", active, e);
            } else {
                tracing::info!("Active context: {}", active);
            }
        }
    }
    any_provider_added |= add_env_only_contexts(&mut server, &config, store.as_ref());

    // Apply built-in tools filtering from local config (remote config may override later).
    if !config.builtin_tools.is_empty() {
        config.builtin_tools.warn_unknown_tools(KNOWN_BUILTIN_TOOLS);
        server
            .set_builtin_tools_config(config.builtin_tools.clone())
            .context("Invalid builtin_tools configuration")?;
    }

    // Spawn background task for remote config fetch + proxy connection.
    // This is the slow path (~1-2s of HTTP calls) that we don't want to block stdin.
    // Proxy tools become available on first tools/list or tools/call.
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let env_snapshot: Vec<(String, String)> = std::env::vars().collect();
        let bg_config = config.clone();
        let bg_store = get_credential_store();
        let bg_token = config
            .remote_config
            .as_ref()
            .and_then(|rc| rc.token_key.as_ref())
            .and_then(|key| store.get(key).ok())
            .flatten();

        tokio::spawn(async move {
            // 1. Fetch and merge remote config
            let merged_config =
                devboy_core::remote_config::fetch_and_merge(bg_config, bg_token.as_deref()).await;

            // 2. Re-initialize Sentry if remote config provided a DSN
            #[cfg(feature = "sentry")]
            {
                let remote_dsn = merged_config
                    .sentry
                    .as_ref()
                    .and_then(|s| s.dsn.as_ref())
                    .filter(|s| !s.is_empty());
                let current_enabled = sentry::Hub::current()
                    .client()
                    .is_some_and(|c| c.is_enabled());

                if remote_dsn.is_some() && !current_enabled {
                    let new_guard = devboy_core::sentry_integration::init_sentry(
                        merged_config.sentry.as_ref(),
                        &sentry_release(),
                    );
                    if new_guard.as_ref().is_some_and(|g| g.is_enabled()) {
                        tracing::info!("Sentry re-initialized with DSN from remote config");
                        if let Some(guard) = new_guard {
                            std::mem::forget(guard);
                        }
                    }
                }
            }

            // 3. Build proxy manager and fetch tools
            let mut proxy_manager = build_proxy_manager(&merged_config, bg_store.as_ref()).await;
            add_env_only_proxies_from_snapshot(
                &mut proxy_manager,
                &merged_config,
                bg_store.as_ref(),
                &env_snapshot,
            )
            .await;

            if !proxy_manager.is_empty()
                && let Err(e) = proxy_manager.fetch_all_tools().await
            {
                tracing::warn!("Failed to fetch proxy tools: {}", e);
            }

            // 4. Send back remote builtin_tools override (if any)
            let builtin_tools_config = if !merged_config.builtin_tools.is_empty() {
                Some(merged_config.builtin_tools)
            } else {
                None
            };

            // 5. Build the transparent-routing engine off the finalised upstream
            //    catalogue + local tool registry. Always installed: the engine handles
            //    local-only dispatch and telemetry emission even without upstreams.
            let local_catalogue = local_tool_catalogue();
            let upstream_catalogue = proxy_manager.raw_upstream_catalogue();
            let match_report = build_report(ToolCatalogue {
                local: &local_catalogue,
                upstream: upstream_catalogue
                    .iter()
                    .map(|(p, t)| (p.clone(), &t[..]))
                    .collect(),
            });
            let routing_engine = Arc::new(RoutingEngine::new(
                merged_config.proxy.routing.clone(),
                match_report,
            ));
            tracing::info!(
                strategy = ?routing_engine.config().strategy,
                total_tools = routing_engine.report().len(),
                "routing engine initialised"
            );

            let _ = tx.send(devboy_mcp::DeferredInit {
                proxy_manager,
                builtin_tools_config,
                routing_engine: Some(routing_engine),
            });
        });
        server.set_deferred_init(rx);
    }

    // Start the telemetry pipeline synchronously so its background flush task stays
    // alive for the process lifetime; buffer is a cheap clone attached to the server.
    let mut telemetry_pipeline = start_telemetry_pipeline(&config, store.as_ref());
    if let Some(pipeline) = &telemetry_pipeline {
        server.set_telemetry(pipeline.buffer());
    }

    if !any_provider_added && !skip_config {
        tracing::warn!("No providers configured. MCP server will have limited functionality.");
        tracing::info!("Configure GitHub: devboy config set github.owner <owner>");
    }

    // Run the MCP server (reads from stdin, writes to stdout).
    // initialize is handled immediately; proxy tools + routing engine are resolved
    // lazily on first tools/list or tools/call via the DeferredInit receiver.
    let run_result = server.run().await.context("MCP server error");

    // Best-effort telemetry flush on shutdown.
    if let Some(mut pipeline) = telemetry_pipeline.take() {
        pipeline.shutdown().await;
    }

    run_result?;

    Ok(())
}

/// Credential store factory for the MCP command — wraps the default chain in a TTL
/// cache when the user enabled it.
fn build_mcp_store(cache_ttl_secs: u64) -> Box<dyn CredentialStore> {
    if is_skip_keychain_enabled() {
        tracing::info!("Using CI credential chain (env vars only, keychain disabled)");
        return wrap_with_cache(ChainStore::ci_chain(), cache_ttl_secs);
    }
    tracing::debug!(
        cache_ttl_secs,
        "Using default credential chain (env vars -> keychain) with TTL cache"
    );
    wrap_with_cache(ChainStore::default_chain(), cache_ttl_secs)
}

/// Configure and launch the telemetry pipeline. Returns `None` when telemetry is
/// disabled or when the auth token cannot be resolved.
fn start_telemetry_pipeline(
    config: &Config,
    store: &dyn CredentialStore,
) -> Option<TelemetryPipeline> {
    if !config.proxy.telemetry.enabled {
        tracing::debug!("proxy.telemetry.enabled=false; skipping pipeline");
        return None;
    }
    if config.proxy.telemetry.endpoint.is_none() {
        tracing::debug!("proxy.telemetry.endpoint unset; pipeline buffers locally only");
    }

    // Resolve auth token: prefer explicit telemetry.token_key, otherwise fall back to
    // the first upstream server's token. Missing token is fine for unauthenticated
    // endpoints — the uploader will just omit the header.
    let token_key = config.proxy.telemetry.token_key.clone().or_else(|| {
        config
            .proxy_mcp_servers
            .first()
            .and_then(|s| s.token_key.clone())
    });
    let bearer_token = token_key
        .as_deref()
        .and_then(|k| store.get(k).ok().flatten());

    let mut pipeline = TelemetryPipeline::new(config.proxy.telemetry.clone());
    if let Err(e) = pipeline.start(TelemetryAuth { bearer_token }) {
        tracing::warn!(error = %e, "telemetry pipeline failed to start; continuing without it");
        return None;
    }
    Some(pipeline)
}

// =============================================================================
// Proxy Command
// =============================================================================

async fn handle_proxy_command(command: ProxyCommands) -> Result<()> {
    // Handle Add and Remove commands first (they don't require existing config)
    match &command {
        ProxyCommands::Add {
            name,
            url,
            transport,
            token_key,
            token,
            auth_type,
            force,
        } => {
            return handle_proxy_add(
                name,
                url,
                *transport,
                token_key.clone(),
                token.clone(),
                *auth_type,
                *force,
            );
        }
        ProxyCommands::Remove { name } => {
            return handle_proxy_remove(name);
        }
        _ => {}
    }

    let (config, _) = load_runtime_config()?;
    let store = get_credential_store();

    // `Status` is the only read-only command that is still useful without upstream:
    // it reports the local tool catalogue plus the active routing policy, which lets
    // users validate their config before wiring a cloud server.
    let is_status_cmd = matches!(command, ProxyCommands::Status { .. });

    if config.proxy_mcp_servers.is_empty() && !is_status_cmd {
        println!("No proxy MCP servers configured.");
        println!("Add using: devboy proxy add <name> --url <url>");
        println!();
        println!("Or add to .devboy.toml:");
        println!();
        println!("  [[proxy_mcp_servers]]");
        println!("  name = \"my-server\"");
        println!("  url = \"https://example.com/api/mcp\"");
        println!("  transport = \"streamable-http\"");
        return Ok(());
    }

    let mut proxy_manager = build_proxy_manager(&config, store.as_ref()).await;

    if proxy_manager.is_empty() && !is_status_cmd {
        eprintln!("Could not connect to any upstream MCP server.");
        return Ok(());
    }

    match command {
        ProxyCommands::Add { .. } | ProxyCommands::Remove { .. } => unreachable!(),
        ProxyCommands::Tools { descriptions } => {
            proxy_manager
                .fetch_all_tools()
                .await
                .context("Failed to fetch tools from upstream servers")?;
            let tools = proxy_manager.all_tools();
            if tools.is_empty() {
                println!("No tools available from upstream servers.");
            } else {
                println!("Available proxy tools ({}):", tools.len());
                println!();
                for tool in &tools {
                    if descriptions {
                        let desc = tool.description.lines().next().unwrap_or("");
                        println!("  {} - {}", tool.name, desc);
                    } else {
                        println!("  {}", tool.name);
                    }
                }
            }
        }
        ProxyCommands::Call { tool, args } => {
            let arguments: Option<serde_json::Value> = match serde_json::from_str(&args) {
                Ok(v) => Some(v),
                Err(e) => {
                    eprintln!("Invalid JSON arguments: {}", e);
                    return Ok(());
                }
            };

            match proxy_manager.try_call(&tool, arguments).await {
                Some(result) => {
                    let json = serde_json::to_string_pretty(&result)
                        .unwrap_or_else(|_| format!("{:?}", result));
                    println!("{}", json);
                }
                None => {
                    eprintln!("Tool '{}' not found in any upstream server.", tool);
                    eprintln!("Run 'devboy proxy tools' to see available tools.");
                }
            }
        }
        ProxyCommands::Status { json } => {
            // Fetch upstream tool catalogues only when there are connected clients.
            // Local-only mode (no upstream) still produces a useful status snapshot:
            // every tool is classified as `local_only`, matching runtime behaviour.
            if !proxy_manager.is_empty() {
                proxy_manager
                    .fetch_all_tools()
                    .await
                    .context("Failed to fetch tools from upstream servers")?;
            }
            let status =
                build_proxy_status(&config, &proxy_manager).context("failed to build status")?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&status.to_json()).unwrap_or_default()
                );
            } else {
                if proxy_manager.is_empty() {
                    println!("(No upstream MCP servers connected — showing local-only status)");
                    println!();
                }
                print!("{}", status.to_text_report());
            }
        }
    }

    Ok(())
}

/// Build a [`ProxyStatus`] snapshot from the current config and a connected proxy
/// manager. Combines the local tool catalogue with upstream catalogues into a
/// [`RoutingEngine`] purely for reporting (nothing is dispatched).
fn build_proxy_status(config: &Config, proxy_manager: &ProxyManager) -> Result<ProxyStatus> {
    let local_tools = local_tool_catalogue();
    let upstream = proxy_manager.raw_upstream_catalogue();
    let catalogue = ToolCatalogue {
        local: &local_tools,
        upstream: upstream.iter().map(|(p, t)| (p.clone(), &t[..])).collect(),
    };
    let report = build_report(catalogue);
    let engine = RoutingEngine::new(config.proxy.routing.clone(), report);
    Ok(ProxyStatus::from_engine(&engine))
}

/// Enumerate every built-in tool available on this process. The list is used both for
/// signature matching and for the `proxy status` command.
fn local_tool_catalogue() -> Vec<ToolDefinition> {
    // Source of truth lives in the executor crate — reuse the same definitions the
    // MCP server advertises via `tools/list`. No providers attached: we only need the
    // names and schemas for matching, not runtime dispatch.
    let base = devboy_executor::tools::base_tool_definitions();
    let mut tools: Vec<ToolDefinition> = base
        .into_iter()
        .map(|t| {
            let mut schema = serde_json::to_value(&t.input_schema).unwrap_or_default();
            if let Some(obj) = schema.as_object_mut() {
                obj.entry("type").or_insert_with(|| "object".into());
            }
            ToolDefinition {
                name: t.name,
                description: t.description,
                input_schema: schema,
                category: Some(t.category),
            }
        })
        .collect();

    // Context-management tools that the server injects unconditionally.
    tools.push(ToolDefinition {
        name: "list_contexts".to_string(),
        description: "List configured contexts and indicate the active context.".to_string(),
        input_schema: serde_json::json!({ "type": "object", "properties": {} }),
        category: None,
    });
    tools.push(ToolDefinition {
        name: "use_context".to_string(),
        description: "Switch active context at runtime.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": { "type": "string" }
            }
        }),
        category: None,
    });
    tools.push(ToolDefinition {
        name: "get_current_context".to_string(),
        description: "Get current active context name.".to_string(),
        input_schema: serde_json::json!({ "type": "object", "properties": {} }),
        category: None,
    });

    tools
}

/// Add a new proxy server configuration.
fn handle_proxy_add(
    name: &str,
    url: &str,
    transport: TransportType,
    token_key: Option<String>,
    token: Option<String>,
    auth_type: Option<AuthType>,
    force: bool,
) -> Result<()> {
    let (mut config, config_path) = load_runtime_config()?;

    // Check if proxy with same name exists
    let existing_idx = config.proxy_mcp_servers.iter().position(|p| p.name == name);

    if let Some(idx) = existing_idx {
        if !force {
            anyhow::bail!("Proxy '{}' already exists. Use --force to overwrite.", name);
        }
        // Remove existing proxy
        config.proxy_mcp_servers.remove(idx);
        println!("Overwriting existing proxy '{}'", name);
    }

    // Determine token key: use provided or generate default if token provided
    let final_token_key = if token.is_some() || token_key.is_some() {
        Some(token_key.unwrap_or_else(|| format!("proxy.{}.token", name)))
    } else {
        None
    };

    // Determine auth type
    let final_auth_type = auth_type
        .map(|a| a.as_str().to_string())
        .unwrap_or_else(|| {
            if final_token_key.is_some() {
                AuthType::Bearer.as_str().to_string()
            } else {
                AuthType::None.as_str().to_string()
            }
        });

    let transport_str = transport.as_str();

    // Add new proxy
    let proxy = ProxyMcpServerConfig {
        name: name.to_string(),
        url: url.to_string(),
        auth_type: final_auth_type.clone(),
        token_key: final_token_key.clone(),
        tool_prefix: None,
        transport: transport_str.to_string(),
        routing: None,
    };

    config.proxy_mcp_servers.push(proxy);
    config
        .save_to(&config_path)
        .context("Failed to save config")?;

    // Store token in keychain if provided
    if let Some(token_value) = token {
        let key = final_token_key.as_ref().unwrap();
        let store = get_credential_store_for_init();
        store
            .store(key, &token_value)
            .with_context(|| format!("Failed to store token in keychain as '{}'", key))?;
        println!("Stored token in keychain as '{}'", key);
    }

    println!("Added proxy '{}' -> {}", name, url);
    println!("  transport: {}", transport_str);
    println!("  auth_type: {}", final_auth_type);
    if let Some(key) = &final_token_key {
        println!("  token_key: {}", key);
    }
    println!();
    println!("Config saved to: {}", config_path.display());

    Ok(())
}

/// Remove a proxy server configuration.
fn handle_proxy_remove(name: &str) -> Result<()> {
    let (mut config, config_path) = load_runtime_config()?;

    let original_len = config.proxy_mcp_servers.len();
    config.proxy_mcp_servers.retain(|p| p.name != name);

    if config.proxy_mcp_servers.len() == original_len {
        anyhow::bail!("Proxy '{}' not found in configuration.", name);
    }

    config
        .save_to(&config_path)
        .context("Failed to save config")?;

    println!("Removed proxy '{}'", name);
    println!("Config saved to: {}", config_path.display());

    Ok(())
}

async fn build_proxy_manager(config: &Config, store: &dyn CredentialStore) -> ProxyManager {
    let mut proxy_manager = ProxyManager::new();
    for proxy_cfg in &config.proxy_mcp_servers {
        let token = proxy_cfg
            .token_key
            .as_deref()
            .and_then(|key| store.get(key).ok().flatten());

        // Check for URL override from environment variable
        // Pattern: DEVBOY_{NAME}_URL (e.g., DEVBOY_DEVBOY_CLOUD_URL)
        let url = get_proxy_url_from_env(&proxy_cfg.name).unwrap_or_else(|| proxy_cfg.url.clone());

        let transport = ProxyTransport::parse(&proxy_cfg.transport);

        match McpProxyClient::connect(
            &proxy_cfg.name,
            &url,
            proxy_cfg.tool_prefix.as_deref(),
            token.as_deref(),
            &proxy_cfg.auth_type,
            transport,
        )
        .await
        {
            Ok(client) => {
                tracing::info!(
                    "Connected to upstream MCP server '{}' at {}",
                    proxy_cfg.name,
                    url
                );
                proxy_manager.add_client(client);
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to connect to upstream MCP server '{}': {}",
                    proxy_cfg.name,
                    e
                );
            }
        }
    }
    proxy_manager
}

/// Get proxy URL from environment variable.
///
/// Checks for `DEVBOY_{NAME}_URL` where name is uppercased with
/// dots, slashes, and dashes replaced by underscores.
///
/// Example: proxy name "devboy-cloud" -> `DEVBOY_DEVBOY_CLOUD_URL`
fn get_proxy_url_from_env(name: &str) -> Option<String> {
    let env_name = format!(
        "DEVBOY_{}_URL",
        name.to_uppercase().replace(['.', '/', '-'], "_")
    );
    std::env::var(&env_name).ok().filter(|s| !s.is_empty())
}

/// Scan environment for context definitions not in config.
///
/// Looks for `DEVBOY_CONTEXTS_{NAME}_{PROVIDER}_{FIELD}` patterns and creates
/// contexts with providers for them if they don't already exist in the config.
///
/// Supported patterns:
/// - `DEVBOY_CONTEXTS_{NAME}_GITHUB_OWNER` + `_REPO` -> GitHub provider
/// - `DEVBOY_CONTEXTS_{NAME}_GITLAB_URL` + `_PROJECT_ID` -> GitLab provider
/// - `DEVBOY_CONTEXTS_{NAME}_CLICKUP_LIST_ID` -> ClickUp provider
/// - `DEVBOY_CONTEXTS_{NAME}_JIRA_URL` + `_PROJECT_KEY` + `_EMAIL` -> Jira provider
/// - `DEVBOY_CONTEXTS_{NAME}_SLACK_WORKSPACE` or `_TEAM_ID` -> Slack provider
///
/// Tokens are resolved via the credential store (which checks env vars first).
///
/// Example:
/// ```bash
/// DEVBOY_CONTEXTS_PROD_GITHUB_OWNER=company
/// DEVBOY_CONTEXTS_PROD_GITHUB_REPO=app
/// DEVBOY_CONTEXTS_PROD_GITHUB_TOKEN=ghp_xxx
/// ```
fn add_env_only_contexts(
    server: &mut McpServer,
    config: &Config,
    store: &dyn CredentialStore,
) -> bool {
    let mut any_added = false;

    // Collect existing context names from config
    let existing_contexts: std::collections::HashSet<_> =
        config.contexts.keys().map(|s| s.to_lowercase()).collect();

    // Also check for legacy default context
    let has_legacy_default = config.legacy_default_context().is_some();

    // Scan environment for context patterns
    // We need to group env vars by context name first
    let mut env_contexts: std::collections::HashMap<String, EnvContextBuilder> =
        std::collections::HashMap::new();

    for (key, value) in std::env::vars() {
        if let Some(rest) = key.strip_prefix("DEVBOY_CONTEXTS_") {
            // Parse pattern: {CONTEXT}_{PROVIDER}_{FIELD}
            // e.g., PROD_GITHUB_OWNER, MY_PROJECT_GITLAB_URL
            if let Some((context_name, provider, field)) = parse_context_env_key(rest) {
                let builder = env_contexts
                    .entry(context_name.clone())
                    .or_insert_with(|| EnvContextBuilder::new(context_name.clone()));

                builder.set_field(&provider, &field, value);
            }
        }
    }

    // Now create contexts from collected env vars
    for (context_name, builder) in env_contexts {
        let normalized = context_name.to_lowercase();

        // Skip if already in config
        if existing_contexts.contains(&normalized) {
            tracing::debug!(
                "Context '{}' already in config, env vars ignored",
                context_name
            );
            continue;
        }

        // Skip default if legacy config exists
        if normalized == "default" && has_legacy_default {
            tracing::debug!("Context 'default' has legacy config, env vars ignored");
            continue;
        }

        // Build and add context
        if let Some(context) = builder.build() {
            let display_name = context_name.to_lowercase().replace('_', "-");
            server.ensure_context(&display_name);

            let added =
                add_context_providers_from_env(server, store, &display_name, &context, &builder);
            if added {
                tracing::info!(
                    "Created env-only context '{}' from environment",
                    display_name
                );
                any_added = true;
            }
        }
    }

    any_added
}

/// Parse context env key into (context_name, provider, field).
///
/// Examples:
/// - "PROD_GITHUB_OWNER" -> ("PROD", "GITHUB", "OWNER")
/// - "MY_PROJECT_GITLAB_URL" -> ("MY_PROJECT", "GITLAB", "URL")
fn parse_context_env_key(key: &str) -> Option<(String, String, String)> {
    // Known provider prefixes (in order of specificity)
    let providers = ["GITHUB", "GITLAB", "CLICKUP", "JIRA", "SLACK"];

    for provider in providers {
        // Look for _PROVIDER_ in the key
        let provider_marker = format!("_{}_", provider);
        if let Some(pos) = key.find(&provider_marker) {
            let context_name = &key[..pos];
            let field = &key[pos + provider_marker.len()..];
            if !context_name.is_empty() && !field.is_empty() {
                return Some((
                    context_name.to_string(),
                    provider.to_string(),
                    field.to_string(),
                ));
            }
        }
    }
    None
}

/// Builder for collecting context configuration from env vars.
#[derive(Default)]
struct EnvContextBuilder {
    name: String,
    // GitHub
    github_owner: Option<String>,
    github_repo: Option<String>,
    github_base_url: Option<String>,
    // GitLab
    gitlab_url: Option<String>,
    gitlab_project_id: Option<String>,
    // ClickUp
    clickup_list_id: Option<String>,
    clickup_team_id: Option<String>,
    // Jira
    jira_url: Option<String>,
    jira_project_key: Option<String>,
    jira_email: Option<String>,
    // Slack
    slack_team_id: Option<String>,
    slack_workspace: Option<String>,
    slack_base_url: Option<String>,
    slack_client_id: Option<String>,
    slack_redirect_uri: Option<String>,
}

impl EnvContextBuilder {
    fn new(name: String) -> Self {
        Self {
            name,
            ..Default::default()
        }
    }

    fn set_field(&mut self, provider: &str, field: &str, value: String) {
        match (provider, field) {
            // GitHub
            ("GITHUB", "OWNER") => self.github_owner = Some(value),
            ("GITHUB", "REPO") => self.github_repo = Some(value),
            ("GITHUB", "BASE_URL") | ("GITHUB", "URL") => self.github_base_url = Some(value),
            // GitLab
            ("GITLAB", "URL") => self.gitlab_url = Some(value),
            ("GITLAB", "PROJECT_ID") | ("GITLAB", "PROJECT") => {
                self.gitlab_project_id = Some(value)
            }
            // ClickUp
            ("CLICKUP", "LIST_ID") | ("CLICKUP", "LIST") => self.clickup_list_id = Some(value),
            ("CLICKUP", "TEAM_ID") | ("CLICKUP", "TEAM") => self.clickup_team_id = Some(value),
            // Jira
            ("JIRA", "URL") => self.jira_url = Some(value),
            ("JIRA", "PROJECT_KEY") | ("JIRA", "PROJECT") => self.jira_project_key = Some(value),
            ("JIRA", "EMAIL") => self.jira_email = Some(value),
            // Slack
            ("SLACK", "TEAM_ID") | ("SLACK", "TEAM") => self.slack_team_id = Some(value),
            ("SLACK", "WORKSPACE") => self.slack_workspace = Some(value),
            ("SLACK", "BASE_URL") | ("SLACK", "URL") => self.slack_base_url = Some(value),
            ("SLACK", "CLIENT_ID") => self.slack_client_id = Some(value),
            ("SLACK", "REDIRECT_URI") => self.slack_redirect_uri = Some(value),
            // Unknown fields are silently ignored
            _ => {
                tracing::debug!(
                    "Unknown env field for context '{}': {}_{}_{}",
                    self.name,
                    self.name,
                    provider,
                    field
                );
            }
        }
    }

    fn build(&self) -> Option<ContextConfig> {
        let github = if self.github_owner.is_some() && self.github_repo.is_some() {
            Some(GitHubConfig {
                owner: self.github_owner.clone().unwrap(),
                repo: self.github_repo.clone().unwrap(),
                base_url: self.github_base_url.clone(),
            })
        } else {
            None
        };

        let gitlab = if self.gitlab_project_id.is_some() {
            Some(GitLabConfig {
                url: self
                    .gitlab_url
                    .clone()
                    .unwrap_or_else(|| "https://gitlab.com".to_string()),
                project_id: self.gitlab_project_id.clone().unwrap(),
            })
        } else {
            None
        };

        let clickup = if self.clickup_list_id.is_some() {
            Some(ClickUpConfig {
                list_id: self.clickup_list_id.clone().unwrap(),
                team_id: self.clickup_team_id.clone(),
            })
        } else {
            None
        };

        let jira = if self.jira_url.is_some()
            && self.jira_project_key.is_some()
            && self.jira_email.is_some()
        {
            Some(JiraConfig {
                url: self.jira_url.clone().unwrap(),
                project_key: self.jira_project_key.clone().unwrap(),
                email: self.jira_email.clone().unwrap(),
            })
        } else {
            None
        };

        let context = ContextConfig {
            github,
            gitlab,
            clickup,
            jira,
            fireflies: None,
            slack: if self.slack_team_id.is_some()
                || self.slack_workspace.is_some()
                || self.slack_base_url.is_some()
                || self.slack_client_id.is_some()
                || self.slack_redirect_uri.is_some()
            {
                Some(SlackConfig {
                    team_id: self.slack_team_id.clone(),
                    workspace: self.slack_workspace.clone(),
                    base_url: self.slack_base_url.clone(),
                    client_id: self.slack_client_id.clone(),
                    redirect_uri: self.slack_redirect_uri.clone(),
                    required_scopes: devboy_core::default_slack_required_scopes(),
                })
            } else {
                None
            },
        };

        if context.has_any_provider() {
            Some(context)
        } else {
            None
        }
    }
}

/// Add providers to context from env-only configuration.
///
/// Similar to `add_context_providers` but logs different messages.
fn add_context_providers_from_env(
    server: &mut McpServer,
    store: &dyn CredentialStore,
    context_name: &str,
    context: &ContextConfig,
    _builder: &EnvContextBuilder,
) -> bool {
    let mut added = false;

    if let Some(gh) = &context.github {
        if let Some(token) = get_token_for_context(store, context_name, "github") {
            let client = GitHubClient::new(&gh.owner, &gh.repo, token);
            server.add_provider_to_context(context_name, Arc::new(client));
            tracing::info!(
                "Added GitHub provider to env-only context '{}': {}/{}",
                context_name,
                gh.owner,
                gh.repo
            );
            added = true;
        } else {
            tracing::warn!(
                "GitHub configured via env for context '{}' but no token found",
                context_name
            );
        }
    }

    if let Some(gl) = &context.gitlab {
        if let Some(token) = get_token_for_context(store, context_name, "gitlab") {
            let client = GitLabClient::with_base_url(&gl.url, &gl.project_id, token);
            server.add_provider_to_context(context_name, Arc::new(client));
            tracing::info!(
                "Added GitLab provider to env-only context '{}': {} (project {})",
                context_name,
                gl.url,
                gl.project_id
            );
            added = true;
        } else {
            tracing::warn!(
                "GitLab configured via env for context '{}' but no token found",
                context_name
            );
        }
    }

    if let Some(cu) = &context.clickup {
        if let Some(token) = get_token_for_context(store, context_name, "clickup") {
            let mut client = ClickUpClient::new(&cu.list_id, token);
            if let Some(team_id) = &cu.team_id {
                client = client.with_team_id(team_id);
            }
            server.add_provider_to_context(context_name, Arc::new(client));
            tracing::info!(
                "Added ClickUp provider to env-only context '{}': list {}",
                context_name,
                cu.list_id
            );
            added = true;
        } else {
            tracing::warn!(
                "ClickUp configured via env for context '{}' but no token found",
                context_name
            );
        }
    }

    if let Some(jira) = &context.jira {
        if let Some(token) = get_token_for_context(store, context_name, "jira") {
            let client = JiraClient::new(&jira.url, &jira.project_key, &jira.email, token);
            server.add_provider_to_context(context_name, Arc::new(client));
            tracing::info!(
                "Added Jira provider to env-only context '{}': {} (project {})",
                context_name,
                jira.url,
                jira.project_key
            );
            added = true;
        } else {
            tracing::warn!(
                "Jira configured via env for context '{}' but no token found",
                context_name
            );
        }
    }

    if context.fireflies.is_some() {
        if let Some(token) = get_token_for_context(store, context_name, "fireflies") {
            let client = devboy_fireflies::FirefliesClient::new(&token);
            server.add_meeting_provider(Arc::new(client));
            tracing::info!("Added Fireflies provider to context '{}'", context_name);
            added = true;
        } else {
            tracing::warn!(
                "Fireflies configured for context '{}' but no API key found",
                context_name
            );
        }
    }

    if let Some(slack) = &context.slack {
        if let Some(token) = get_token_for_context(store, context_name, "slack") {
            let mut client =
                SlackClient::new(token).with_required_scopes(slack.required_scopes.clone());
            if let Some(base_url) = &slack.base_url {
                client = client.with_base_url(base_url);
            }
            server.add_messenger_provider_to_context(context_name, Arc::new(client));
            tracing::info!("Added Slack provider to context '{}'", context_name);
            added = true;
        } else {
            tracing::warn!(
                "Slack configured for context '{}' but no bot token found",
                context_name
            );
        }
    }

    added
}

/// Scan environment for proxy definitions not in config.
///
/// Looks for `DEVBOY_*_URL` variables and creates proxies for them
/// if they don't already exist in the config.
/// Uses a pre-collected env var snapshot so this function is Send-safe.
///
/// Example:
///
/// - `DEVBOY_DEVBOY_CLOUD_URL=https://...` creates proxy named "devboy-cloud"
/// - `DEVBOY_DEVBOY_CLOUD_TOKEN=xxx` provides the token
async fn add_env_only_proxies_from_snapshot(
    proxy_manager: &mut ProxyManager,
    config: &Config,
    store: &dyn CredentialStore,
    env_vars: &[(String, String)],
) {
    let existing_names: std::collections::HashSet<_> = config
        .proxy_mcp_servers
        .iter()
        .map(|p| p.name.to_lowercase().replace(['.', '/', '-'], "_"))
        .collect();

    for (key, url) in env_vars {
        if let Some(name) = key
            .strip_prefix("DEVBOY_")
            .and_then(|s| s.strip_suffix("_URL"))
        {
            if name.starts_with("CONTEXTS_") {
                continue;
            }
            let proxy_name = name.to_lowercase().replace('_', "-");
            let normalized = name.to_lowercase();
            if existing_names.contains(&normalized) {
                continue;
            }
            let token_key = format!("{}.token", proxy_name);
            let token = store.get(&token_key).ok().flatten();
            tracing::info!(
                "Found env-only proxy '{}' from {} (token: {})",
                proxy_name,
                key,
                if token.is_some() { "found" } else { "none" }
            );
            match McpProxyClient::connect(
                &proxy_name,
                url,
                None,
                token.as_deref(),
                "bearer",
                ProxyTransport::StreamableHttp,
            )
            .await
            {
                Ok(client) => {
                    tracing::info!("Connected to env-only proxy '{}' at {}", proxy_name, url);
                    proxy_manager.add_client(client);
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to connect to env-only proxy '{}': {}",
                        proxy_name,
                        e
                    );
                }
            }
        }
    }
}

fn get_token_for_context(
    store: &dyn CredentialStore,
    context_name: &str,
    provider: &str,
) -> Option<String> {
    let scoped_key = format!("contexts.{}.{}.token", context_name, provider);
    store
        .get(&scoped_key)
        .ok()
        .flatten()
        .or_else(|| store.get(&format!("{}.token", provider)).ok().flatten())
}

fn add_context_providers(
    server: &mut McpServer,
    store: &dyn CredentialStore,
    context_name: &str,
    context: &ContextConfig,
) -> bool {
    let mut added = false;

    if let Some(gh) = &context.github {
        if let Some(token) = get_token_for_context(store, context_name, "github") {
            let client = GitHubClient::new(&gh.owner, &gh.repo, token);
            server.add_provider_to_context(context_name, Arc::new(client));
            tracing::info!(
                "Added GitHub provider to context '{}': {}/{}",
                context_name,
                gh.owner,
                gh.repo
            );
            added = true;
        } else {
            tracing::warn!(
                "GitHub configured in context '{}' but no token found (tried contexts.{}.github.token then github.token)",
                context_name,
                context_name
            );
        }
    }

    if let Some(gl) = &context.gitlab {
        if let Some(token) = get_token_for_context(store, context_name, "gitlab") {
            let client = GitLabClient::with_base_url(&gl.url, &gl.project_id, token);
            server.add_provider_to_context(context_name, Arc::new(client));
            tracing::info!(
                "Added GitLab provider to context '{}': {} (project {})",
                context_name,
                gl.url,
                gl.project_id
            );
            added = true;
        } else {
            tracing::warn!(
                "GitLab configured in context '{}' but no token found (tried contexts.{}.gitlab.token then gitlab.token)",
                context_name,
                context_name
            );
        }
    }

    if let Some(cu) = &context.clickup {
        if let Some(token) = get_token_for_context(store, context_name, "clickup") {
            let mut client = ClickUpClient::new(&cu.list_id, token);
            if let Some(team_id) = &cu.team_id {
                client = client.with_team_id(team_id);
            }
            server.add_provider_to_context(context_name, Arc::new(client));
            tracing::info!(
                "Added ClickUp provider to context '{}': list {}",
                context_name,
                cu.list_id
            );
            added = true;
        } else {
            tracing::warn!(
                "ClickUp configured in context '{}' but no token found (tried contexts.{}.clickup.token then clickup.token)",
                context_name,
                context_name
            );
        }
    }

    if let Some(jira) = &context.jira {
        if let Some(token) = get_token_for_context(store, context_name, "jira") {
            let client = JiraClient::new(&jira.url, &jira.project_key, &jira.email, token);
            server.add_provider_to_context(context_name, Arc::new(client));
            tracing::info!(
                "Added Jira provider to context '{}': {} (project {})",
                context_name,
                jira.url,
                jira.project_key
            );
            added = true;
        } else {
            tracing::warn!(
                "Jira configured in context '{}' but no token found (tried contexts.{}.jira.token then jira.token)",
                context_name,
                context_name
            );
        }
    }

    if context.fireflies.is_some() {
        if let Some(token) = get_token_for_context(store, context_name, "fireflies") {
            let client = devboy_fireflies::FirefliesClient::new(&token);
            server.add_meeting_provider(Arc::new(client));
            tracing::info!("Added Fireflies provider to context '{}'", context_name);
            added = true;
        } else {
            tracing::warn!(
                "Fireflies configured for context '{}' but no API key found",
                context_name
            );
        }
    }

    if let Some(slack) = &context.slack {
        if let Some(token) = get_token_for_context(store, context_name, "slack") {
            let mut client =
                SlackClient::new(token).with_required_scopes(slack.required_scopes.clone());
            if let Some(base_url) = &slack.base_url {
                client = client.with_base_url(base_url);
            }
            server.add_messenger_provider_to_context(context_name, Arc::new(client));
            tracing::info!("Added Slack provider to context '{}'", context_name);
            added = true;
        } else {
            tracing::warn!(
                "Slack configured in context '{}' but no token found (tried contexts.{}.slack.token then slack.token)",
                context_name,
                context_name
            );
        }
    }

    added
}

// =============================================================================
// Tools Command
// =============================================================================

async fn handle_tools_command(command: Option<ToolsCommands>) -> Result<()> {
    match command {
        None => handle_tools_interactive()?,
        Some(ToolsCommands::List) => handle_tools_list()?,
        Some(ToolsCommands::Disable { names }) => handle_tools_disable(names)?,
        Some(ToolsCommands::Enable { names }) => handle_tools_enable(names)?,
        Some(ToolsCommands::Reset) => handle_tools_reset()?,
        Some(ToolsCommands::Call { name, args }) => handle_tools_call(&name, &args).await?,
        Some(ToolsCommands::Docs {
            format,
            output,
            check,
        }) => handle_tools_docs(format.into(), output.as_deref(), check)?,
    }
    Ok(())
}

fn handle_tools_docs(
    format: devboy_executor::tool_docs::DocsFormat,
    output: Option<&std::path::Path>,
    check: bool,
) -> Result<()> {
    let rendered = devboy_executor::tool_docs::render(format);

    let Some(path) = output else {
        // Stdout — print as-is.
        print!("{}", rendered);
        if !rendered.ends_with('\n') {
            println!();
        }
        return Ok(());
    };

    if check {
        let existing = std::fs::read_to_string(path).with_context(|| {
            format!(
                "--check: reference file `{}` is missing — run without --check to generate it",
                path.display()
            )
        })?;
        // Normalise CRLF → LF before equality so Windows checkouts with
        // `core.autocrlf=true` do not trip the gate. `.gitattributes`
        // also forces LF on this file in the index.
        if existing.replace("\r\n", "\n") != rendered.replace("\r\n", "\n") {
            anyhow::bail!(
                "{} is out of date — re-run `devboy tools docs --output {}`",
                path.display(),
                path.display()
            );
        }
        println!("{} is up to date", path.display());
        return Ok(());
    }

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create parent directory `{}`", parent.display()))?;
    }
    std::fs::write(path, &rendered)
        .with_context(|| format!("failed to write `{}`", path.display()))?;
    println!("Wrote {}", path.display());
    Ok(())
}

// =============================================================================
// Docs Command (CLI auto-reference)
// =============================================================================

fn handle_docs_command(command: DocsCommands) -> Result<()> {
    match command {
        DocsCommands::Cli {
            format: _,
            output,
            check,
        } => handle_docs_cli(output.as_deref(), check),
    }
}

fn render_cli_markdown() -> String {
    use clap::CommandFactory;
    // `clap-markdown` walks the entire `Command` tree and emits one
    // table per subcommand. The output is deterministic so we can
    // commit it as a snapshot and gate CI on byte-equality.
    let body = clap_markdown::help_markdown_command(&Cli::command());

    // Inject a "do not edit" preamble after the H1 so reviewers
    // landing on the file from a search result understand the
    // refresh path. Mirrors the convention used in tools.md.
    let preamble = "> Auto-generated by `devboy docs cli` from the live \
        `clap` definition. Do not edit by hand — re-run the command \
        (or `cargo test --workspace` will fail with a drift error) to \
        refresh.\n\n";

    if let Some((heading, rest)) = body.split_once("\n\n") {
        format!("{heading}\n\n{preamble}{rest}")
    } else {
        format!("{preamble}{body}")
    }
}

fn handle_docs_cli(output: Option<&std::path::Path>, check: bool) -> Result<()> {
    let rendered = render_cli_markdown();

    let Some(path) = output else {
        print!("{}", rendered);
        if !rendered.ends_with('\n') {
            println!();
        }
        return Ok(());
    };

    if check {
        let existing = std::fs::read_to_string(path).with_context(|| {
            format!(
                "--check: reference file `{}` is missing — run without --check to generate it",
                path.display()
            )
        })?;
        // Normalise CRLF → LF before the equality check. `.gitattributes`
        // forces LF on this file in the index, but Windows checkouts with
        // `core.autocrlf=true` rewrite to CRLF locally and would trip
        // the gate spuriously.
        if existing.replace("\r\n", "\n") != rendered.replace("\r\n", "\n") {
            anyhow::bail!(
                "{} is out of date — re-run `devboy docs cli --output {}`",
                path.display(),
                path.display()
            );
        }
        println!("{} is up to date", path.display());
        return Ok(());
    }

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create parent directory `{}`", parent.display()))?;
    }
    std::fs::write(path, &rendered)
        .with_context(|| format!("failed to write `{}`", path.display()))?;
    println!("Wrote {}", path.display());
    Ok(())
}

fn handle_tools_interactive() -> Result<()> {
    let (mut config, config_path) = load_runtime_config()?;

    let tools: Vec<&str> = KNOWN_BUILTIN_TOOLS.to_vec();

    // Determine which tools are currently enabled
    let defaults: Vec<bool> = tools
        .iter()
        .map(|name| config.builtin_tools.is_tool_allowed(name))
        .collect();

    let selections = MultiSelect::new()
        .with_prompt("Select enabled built-in tools (Space to toggle, Enter to confirm)")
        .items(&tools)
        .defaults(&defaults)
        .interact()
        .context("Interactive selection cancelled")?;

    // Collect disabled tools list
    let disabled: Vec<String> = tools
        .iter()
        .enumerate()
        .filter(|(i, _)| !selections.contains(i))
        .map(|(_, name)| name.to_string())
        .collect();

    config.builtin_tools = if disabled.is_empty() {
        BuiltinToolsConfig::default()
    } else {
        BuiltinToolsConfig {
            disabled,
            enabled: Vec::new(),
        }
    };

    config
        .save_to(&config_path)
        .context("Failed to save config")?;

    // Print summary
    let enabled_count = selections.len();
    let disabled_count = tools.len() - enabled_count;
    println!(
        "Saved: {} tools enabled, {} disabled ({})",
        enabled_count,
        disabled_count,
        config_path.display()
    );

    Ok(())
}

fn handle_tools_list() -> Result<()> {
    let (config, _) = load_runtime_config()?;
    print_tools_list(&config);
    Ok(())
}

fn print_tools_list(config: &Config) {
    println!("Built-in tools:");
    println!();
    for name in KNOWN_BUILTIN_TOOLS {
        let status = if config.builtin_tools.is_tool_allowed(name) {
            "enabled"
        } else {
            "disabled"
        };
        println!("  {} [{}]", name, status);
    }

    if !config.builtin_tools.disabled.is_empty() {
        println!();
        println!(
            "Mode: blacklist (disabled: {})",
            config.builtin_tools.disabled.join(", ")
        );
    } else if !config.builtin_tools.enabled.is_empty() {
        println!();
        println!(
            "Mode: whitelist (enabled: {})",
            config.builtin_tools.enabled.join(", ")
        );
    }
}

fn handle_tools_disable(names: Vec<String>) -> Result<()> {
    let (mut config, config_path) = load_runtime_config()?;
    apply_tools_disable(&mut config, &names)?;
    config
        .save_to(&config_path)
        .context("Failed to save config")?;
    println!("Disabled tools: {}", names.join(", "));
    Ok(())
}

fn apply_tools_disable(config: &mut Config, names: &[String]) -> Result<()> {
    for name in names {
        if !KNOWN_BUILTIN_TOOLS.contains(&name.as_str()) {
            eprintln!("Warning: unknown tool '{}'", name);
        }
    }

    if !config.builtin_tools.enabled.is_empty() {
        anyhow::bail!(
            "Cannot use 'disable' when whitelist (enabled) mode is active. Use 'reset' first."
        );
    }

    for name in names {
        if !config.builtin_tools.disabled.contains(name) {
            config.builtin_tools.disabled.push(name.clone());
        }
    }

    Ok(())
}

fn handle_tools_enable(names: Vec<String>) -> Result<()> {
    let (mut config, config_path) = load_runtime_config()?;
    apply_tools_enable(&mut config, &names)?;
    config
        .save_to(&config_path)
        .context("Failed to save config")?;
    println!("Enabled tools: {}", names.join(", "));
    Ok(())
}

fn apply_tools_enable(config: &mut Config, names: &[String]) -> Result<()> {
    for name in names {
        if !KNOWN_BUILTIN_TOOLS.contains(&name.as_str()) {
            eprintln!("Warning: unknown tool '{}'", name);
        }
    }

    if !config.builtin_tools.enabled.is_empty() {
        anyhow::bail!(
            "Cannot use 'enable' when whitelist (enabled) mode is active. Use 'reset' first."
        );
    }

    config.builtin_tools.disabled.retain(|n| !names.contains(n));

    Ok(())
}

fn handle_tools_reset() -> Result<()> {
    let (mut config, config_path) = load_runtime_config()?;
    apply_tools_reset(&mut config);
    config
        .save_to(&config_path)
        .context("Failed to save config")?;
    println!("All built-in tools filtering has been reset (all tools enabled).");
    Ok(())
}

fn apply_tools_reset(config: &mut Config) {
    config.builtin_tools = BuiltinToolsConfig::default();
}

async fn handle_tools_call(name: &str, args: &str) -> Result<()> {
    let arguments: serde_json::Value =
        serde_json::from_str(args).context("Invalid JSON arguments")?;

    let (config, _) = load_runtime_config()?;
    let store = get_credential_store();

    let mut server = McpServer::new();

    // Apply built-in tools filtering
    if !config.builtin_tools.is_empty() {
        server
            .set_builtin_tools_config(config.builtin_tools.clone())
            .context("Invalid builtin_tools configuration")?;
    }

    // Add providers (same as handle_mcp_command)
    for (context_name, context) in &config.contexts {
        server.ensure_context(context_name);
        add_context_providers(&mut server, store.as_ref(), context_name, context);
    }
    if !config.contexts.contains_key(Config::DEFAULT_CONTEXT_NAME)
        && let Some(default_context) = config.legacy_default_context()
    {
        add_context_providers(
            &mut server,
            store.as_ref(),
            Config::DEFAULT_CONTEXT_NAME,
            &default_context,
        );
    }
    if let Some(active) = config.resolve_active_context_name() {
        let _ = server.set_active_context(&active);
    }

    let req = JsonRpcRequest {
        jsonrpc: JSONRPC_VERSION.to_string(),
        id: RequestId::Number(1),
        method: "tools/call".to_string(),
        params: Some(serde_json::json!({
            "name": name,
            "arguments": arguments,
        })),
    };

    let resp = server.handle_request(req).await;

    if let Some(error) = resp.error {
        eprintln!("Error: {}", error.message);
        std::process::exit(1);
    }

    // MCP tools/call represents "the tool ran but returned an
    // application-level error" via `result.isError: true`. That's not
    // a JSON-RPC error, so the CLI used to print the body and exit 0
    // — making shell scripting impossible. Mirror the MCP convention:
    // print the body, but exit 1 so `$?` tells callers the tool
    // rejected the call.
    if let Some(result) = resp.result {
        let is_error = result.get("isError").and_then(|v| v.as_bool()) == Some(true);
        let json = serde_json::to_string_pretty(&result)?;
        println!("{}", json);
        if is_error {
            std::process::exit(1);
        }
    }

    Ok(())
}

// =============================================================================
// Benchmark Command
// =============================================================================

async fn run_benchmark(
    owner: &str,
    repo: &str,
    budget: usize,
    limit: u32,
    token: Option<&str>,
) -> Result<()> {
    println!("Format Pipeline Benchmark");
    println!("{}", "=".repeat(65));
    println!("Source:   github.com/{}/{}", owner, repo);
    println!("Budget:   {} tokens", budget);
    println!();

    // Use provided token, env var, or empty (for public repos)
    let gh_token = token
        .map(|t| t.to_string())
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .unwrap_or_default();

    if gh_token.is_empty() {
        println!("Note: No GITHUB_TOKEN set. Set it for higher rate limits.\n");
    }

    let client = devboy_github::GitHubClient::new(owner, repo, &gh_token);

    // Fetch issues
    println!("Fetching issues...");
    let filter = IssueFilter {
        state: Some("all".into()),
        limit: Some(limit),
        ..Default::default()
    };
    let issues = client.get_issues(filter).await?.items;
    if !issues.is_empty() {
        print_comparison("Issues", &issues, budget)?;
    } else {
        println!("  No issues found.");
    }

    // Fetch MRs/PRs
    println!("\nFetching pull requests...");
    let mr_filter = devboy_core::MrFilter {
        state: Some("all".into()),
        limit: Some(limit),
        ..Default::default()
    };
    let mrs = client.get_merge_requests(mr_filter).await?.items;
    if !mrs.is_empty() {
        print_comparison("Pull Requests", &mrs, budget)?;
    } else {
        println!("  No pull requests found.");
    }

    // Fetch diffs from first open PR
    let open_mrs: Vec<_> = mrs.iter().filter(|m| m.state == "open").collect();
    if let Some(mr) = open_mrs.first() {
        println!("\nFetching diffs for {}...", mr.key);
        match client.get_diffs(&mr.key).await {
            Ok(result) if !result.items.is_empty() => {
                print_comparison("Diffs", &result.items, budget)?;
            }
            Ok(_) => println!("  No diffs in this PR."),
            Err(e) => println!("  Failed to fetch diffs: {}", e),
        }
    }

    println!("\n{}", "=".repeat(65));
    Ok(())
}

fn print_comparison<T: serde::Serialize>(label: &str, items: &Vec<T>, budget: usize) -> Result<()> {
    use devboy_format_pipeline::token_counter::estimate_tokens;
    use std::time::Instant;

    // Warm up and measure JSON encoding (average of N runs)
    const RUNS: u32 = 100;
    let start = Instant::now();
    let mut json = String::new();
    for _ in 0..RUNS {
        json = serde_json::to_string_pretty(&items)?;
    }
    let json_us = start.elapsed().as_micros() / RUNS as u128;

    // Measure TOON encoding
    let start = Instant::now();
    let mut toon_out = String::new();
    for _ in 0..RUNS {
        toon_out = devboy_format_pipeline::toon::encode_value(&items)?;
    }
    let toon_us = start.elapsed().as_micros() / RUNS as u128;

    let json_tokens = estimate_tokens(&json);
    let toon_tokens = estimate_tokens(&toon_out);
    let savings = calc_savings(json_tokens, toon_tokens);

    let json_pages = json_tokens.div_ceil(budget);
    let toon_pages = toon_tokens.div_ceil(budget);

    println!("  {} ({} items):", label, items.len());
    println!(
        "    {:<15} {:>8} tokens {:>8} chars {:>3} pages  {:>6}us",
        "JSON",
        json_tokens,
        json.len(),
        json_pages,
        json_us
    );
    println!(
        "    {:<15} {:>8} tokens {:>8} chars {:>3} pages  {:>6}us  ({:.0}% savings)",
        "TOON",
        toon_tokens,
        toon_out.len(),
        toon_pages,
        toon_us,
        savings
    );

    if toon_pages < json_pages {
        println!(
            "    -> TOON saves {} pages ({} vs {})",
            json_pages - toon_pages,
            toon_pages,
            json_pages
        );
    }

    let cpu_overhead = if json_us > 0 {
        ((toon_us as f64 / json_us as f64) - 1.0) * 100.0
    } else {
        0.0
    };

    // Estimate memory: JSON output size + TOON output size (heap strings)
    let json_mem = json.capacity();
    let toon_mem = toon_out.capacity();

    println!(
        "    -> CPU: JSON {}us vs TOON {}us ({:+.0}%), Memory: JSON {} vs TOON {} bytes",
        json_us, toon_us, cpu_overhead, json_mem, toon_mem
    );

    Ok(())
}

fn calc_savings(json_tokens: usize, toon_tokens: usize) -> f64 {
    if json_tokens == 0 {
        return 0.0;
    }
    (1.0 - toon_tokens as f64 / json_tokens as f64) * 100.0
}

// =============================================================================
// Format Pipeline (pipe mode)
// =============================================================================

fn run_format_pipeline(
    data_type: Option<&str>,
    budget: usize,
    strategy: Option<&str>,
    level: &str,
    format: &str,
    stats: bool,
) -> Result<()> {
    use devboy_format_pipeline::budget::{self, BudgetConfig};
    use devboy_format_pipeline::strategy::TrimStrategyKind;
    use devboy_format_pipeline::token_counter::estimate_tokens;
    use devboy_format_pipeline::toon::{self, TrimLevel};
    use std::io;

    if budget == 0 {
        anyhow::bail!("Budget must be at least 1 token.");
    }

    // Parse JSON from stdin (streaming, no full buffering)
    let stdin = io::stdin();
    let stdin_lock = stdin.lock();
    let json_value: serde_json::Value = serde_json::from_reader(stdin_lock).map_err(|err| {
        if err.is_eof() {
            anyhow::anyhow!("Empty input. Pipe JSON data through stdin.")
        } else {
            anyhow::anyhow!("Invalid JSON input: {err}")
        }
    })?;

    // Serialize for token counting (needed for stats comparison)
    let json_string = serde_json::to_string(&json_value)?;
    let json_tokens = estimate_tokens(&json_string);

    let detected_type = data_type.unwrap_or_else(|| detect_data_type(&json_value));

    // Validated by clap value_parser
    let trim_level = match level {
        "standard" => TrimLevel::Standard,
        "minimal" => TrimLevel::Minimal,
        _ => TrimLevel::Full,
    };

    // Validated by clap value_parser, or auto-detect from type
    let strategy_kind = strategy
        .and_then(TrimStrategyKind::parse)
        .unwrap_or(match detected_type {
            "issues" => TrimStrategyKind::ElementCount,
            "merge_requests" => TrimStrategyKind::ElementCount,
            "diffs" => TrimStrategyKind::SizeProportional,
            "comments" => TrimStrategyKind::Cascading,
            "discussions" => TrimStrategyKind::ThreadLevel,
            _ => TrimStrategyKind::Default,
        });

    let budget_config = BudgetConfig {
        budget_tokens: budget,
        ..Default::default()
    };
    let use_toon = format == "toon";

    // Budget trimming applied for ALL types, not just issues.
    // For issues: full budget pipeline with tree knapsack.
    // For other types: proportional trimming to fit budget.
    let output = match detected_type {
        "issues" => {
            let issues: Vec<devboy_core::Issue> = serde_json::from_value(json_value)?;
            let result = budget::process_issues(&issues, strategy_kind, &budget_config)?;
            if use_toon {
                result.content
            } else {
                let included: Vec<_> = issues.into_iter().take(result.included_items).collect();
                serde_json::to_string_pretty(&included)?
            }
        }
        "merge_requests" => {
            let mrs: Vec<devboy_core::MergeRequest> = serde_json::from_value(json_value)?;
            let trimmed = trim_to_budget(&mrs, budget, |items| {
                toon::encode_merge_requests(items, trim_level)
            })?;
            if use_toon {
                toon::encode_merge_requests(&trimmed, trim_level)?
            } else {
                serde_json::to_string_pretty(&trimmed)?
            }
        }
        "diffs" => {
            let diffs: Vec<devboy_core::FileDiff> = serde_json::from_value(json_value)?;
            let trimmed = trim_to_budget(&diffs, budget, toon::encode_diffs)?;
            if use_toon {
                toon::encode_diffs(&trimmed)?
            } else {
                serde_json::to_string_pretty(&trimmed)?
            }
        }
        "comments" => {
            let comments: Vec<devboy_core::Comment> = serde_json::from_value(json_value)?;
            let trimmed = trim_to_budget(&comments, budget, toon::encode_comments)?;
            if use_toon {
                toon::encode_comments(&trimmed)?
            } else {
                serde_json::to_string_pretty(&trimmed)?
            }
        }
        "discussions" => {
            let discussions: Vec<devboy_core::Discussion> = serde_json::from_value(json_value)?;
            let trimmed = trim_to_budget(&discussions, budget, |items| {
                toon::encode_discussions(items)
            })?;
            if use_toon {
                toon::encode_discussions(&trimmed)?
            } else {
                serde_json::to_string_pretty(&trimmed)?
            }
        }
        _ => {
            if use_toon {
                toon::encode_value(&json_value)?
            } else {
                serde_json::to_string_pretty(&json_value)?
            }
        }
    };

    let output_tokens = estimate_tokens(&output);
    println!("{output}");

    if stats {
        let savings = calc_savings(json_tokens, output_tokens);
        eprintln!("--- Format Pipeline Stats ---");
        eprintln!("Type:     {detected_type}");
        eprintln!("Strategy: {}", strategy_kind.as_str());
        eprintln!("Level:    {level}");
        eprintln!("Budget:   {budget} tokens");
        eprintln!(
            "Input:    {json_tokens} tokens ({} chars)",
            json_string.len()
        );
        eprintln!("Output:   {output_tokens} tokens ({} chars)", output.len());
        eprintln!("Savings:  {savings:.1}%");
        eprintln!(
            "Pages:    {} (JSON would need {})",
            output_tokens.div_ceil(budget),
            json_tokens.div_ceil(budget)
        );
    }

    Ok(())
}

/// Trim a slice of items to fit within a token budget.
/// Uses the provided encoder to estimate tokens, then keeps as many items as fit.
fn trim_to_budget<T: Clone>(
    items: &[T],
    budget: usize,
    encode: impl Fn(&[T]) -> devboy_core::Result<String>,
) -> Result<Vec<T>> {
    let encoded = encode(items)?;
    let tokens = devboy_format_pipeline::token_counter::estimate_tokens(&encoded);
    if tokens <= budget {
        return Ok(items.to_vec());
    }
    // Proportional trimming
    let ratio = budget as f64 / tokens as f64;
    let keep = ((items.len() as f64 * ratio) as usize).max(1);
    Ok(items[..keep].to_vec())
}

/// Auto-detect data type from JSON structure.
fn detect_data_type(value: &serde_json::Value) -> &'static str {
    if let Some(arr) = value.as_array()
        && let Some(first) = arr.first()
        && let Some(obj) = first.as_object()
    {
        if obj.contains_key("diff") && obj.contains_key("file_path") {
            return "diffs";
        }
        if obj.contains_key("resolved") && obj.contains_key("comments") {
            return "discussions";
        }
        if obj.contains_key("source_branch") && obj.contains_key("target_branch") {
            return "merge_requests";
        }
        if obj.contains_key("key") && obj.contains_key("title") {
            return "issues";
        }
        if obj.contains_key("body") && obj.contains_key("author") {
            return "comments";
        }
    }
    "unknown"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn config_with_disabled(names: &[&str]) -> Config {
        let mut config = Config::default();
        config.builtin_tools.disabled = names.iter().map(|s| s.to_string()).collect();
        config
    }

    fn config_with_enabled(names: &[&str]) -> Config {
        let mut config = Config::default();
        config.builtin_tools.enabled = names.iter().map(|s| s.to_string()).collect();
        config
    }

    // -- disable tests --

    #[test]
    fn test_disable_adds_tools_to_disabled_list() {
        let mut config = Config::default();
        let names = vec!["get_issues".to_string(), "create_issue".to_string()];

        apply_tools_disable(&mut config, &names).unwrap();

        assert_eq!(config.builtin_tools.disabled, names);
        assert!(config.builtin_tools.enabled.is_empty());
    }

    #[test]
    fn test_disable_does_not_duplicate() {
        let mut config = config_with_disabled(&["get_issues"]);
        let names = vec!["get_issues".to_string(), "create_issue".to_string()];

        apply_tools_disable(&mut config, &names).unwrap();

        assert_eq!(
            config.builtin_tools.disabled,
            vec!["get_issues".to_string(), "create_issue".to_string()]
        );
    }

    #[test]
    fn test_disable_fails_when_whitelist_active() {
        let mut config = config_with_enabled(&["get_issues"]);
        let names = vec!["create_issue".to_string()];

        let result = apply_tools_disable(&mut config, &names);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("whitelist (enabled) mode is active")
        );
    }

    // -- enable tests --

    #[test]
    fn test_enable_removes_from_disabled_list() {
        let mut config = config_with_disabled(&["get_issues", "create_issue", "update_issue"]);
        let names = vec!["get_issues".to_string(), "update_issue".to_string()];

        apply_tools_enable(&mut config, &names).unwrap();

        assert_eq!(
            config.builtin_tools.disabled,
            vec!["create_issue".to_string()]
        );
    }

    #[test]
    fn test_enable_noop_if_not_disabled() {
        let mut config = config_with_disabled(&["create_issue"]);
        let names = vec!["get_issues".to_string()];

        apply_tools_enable(&mut config, &names).unwrap();

        // create_issue stays disabled, get_issues was not in the list
        assert_eq!(
            config.builtin_tools.disabled,
            vec!["create_issue".to_string()]
        );
    }

    #[test]
    fn test_enable_fails_when_whitelist_active() {
        let mut config = config_with_enabled(&["get_issues"]);
        let names = vec!["create_issue".to_string()];

        let result = apply_tools_enable(&mut config, &names);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("whitelist (enabled) mode is active")
        );
    }

    // -- reset tests --

    #[test]
    fn test_reset_clears_disabled() {
        let mut config = config_with_disabled(&["get_issues", "create_issue"]);

        apply_tools_reset(&mut config);

        assert!(config.builtin_tools.disabled.is_empty());
        assert!(config.builtin_tools.enabled.is_empty());
    }

    #[test]
    fn test_reset_clears_enabled() {
        let mut config = config_with_enabled(&["get_issues"]);

        apply_tools_reset(&mut config);

        assert!(config.builtin_tools.disabled.is_empty());
        assert!(config.builtin_tools.enabled.is_empty());
    }

    // -- persistence round-trip --

    #[test]
    fn test_disable_persists_to_file() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp).unwrap();
        let path = tmp.path().to_path_buf();

        let mut config = Config::default();
        apply_tools_disable(
            &mut config,
            &["get_issues".to_string(), "create_issue".to_string()],
        )
        .unwrap();
        config.save_to(&path).unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(
            loaded.builtin_tools.disabled,
            vec!["get_issues".to_string(), "create_issue".to_string()]
        );
        assert!(!loaded.builtin_tools.is_tool_allowed("get_issues"));
        assert!(loaded.builtin_tools.is_tool_allowed("update_issue"));
    }

    #[test]
    fn test_reset_persists_to_file() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp).unwrap();
        let path = tmp.path().to_path_buf();

        let config = config_with_disabled(&["get_issues"]);
        config.save_to(&path).unwrap();

        let mut config = Config::load_from(&path).unwrap();
        apply_tools_reset(&mut config);
        config.save_to(&path).unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert!(loaded.builtin_tools.is_empty());
    }

    // -- sequential workflow --

    #[test]
    fn test_disable_then_enable_workflow() {
        let mut config = Config::default();

        // Disable 3 tools
        apply_tools_disable(
            &mut config,
            &[
                "get_issues".to_string(),
                "create_issue".to_string(),
                "update_issue".to_string(),
            ],
        )
        .unwrap();
        assert_eq!(config.builtin_tools.disabled.len(), 3);
        assert!(!config.builtin_tools.is_tool_allowed("get_issues"));

        // Re-enable one tool
        apply_tools_enable(&mut config, &["get_issues".to_string()]).unwrap();
        assert_eq!(config.builtin_tools.disabled.len(), 2);
        assert!(config.builtin_tools.is_tool_allowed("get_issues"));
        assert!(!config.builtin_tools.is_tool_allowed("create_issue"));

        // Reset everything
        apply_tools_reset(&mut config);
        assert!(config.builtin_tools.is_empty());
        assert!(config.builtin_tools.is_tool_allowed("create_issue"));
    }

    // ==========================================================================
    // Init command tests
    // ==========================================================================

    #[test]
    fn test_parse_git_url_github_ssh() {
        let result = parse_git_url("git@github.com:meteora-pro/devboy-tools.git");
        assert_eq!(
            result,
            Some((
                "github".to_string(),
                "meteora-pro".to_string(),
                "devboy-tools".to_string()
            ))
        );
    }

    #[test]
    fn test_parse_git_url_github_ssh_no_git_suffix() {
        let result = parse_git_url("git@github.com:owner/repo");
        assert_eq!(
            result,
            Some((
                "github".to_string(),
                "owner".to_string(),
                "repo".to_string()
            ))
        );
    }

    #[test]
    fn test_parse_git_url_github_https() {
        let result = parse_git_url("https://github.com/meteora-pro/devboy-tools.git");
        assert_eq!(
            result,
            Some((
                "github".to_string(),
                "meteora-pro".to_string(),
                "devboy-tools".to_string()
            ))
        );
    }

    #[test]
    fn test_parse_git_url_github_https_no_git_suffix() {
        let result = parse_git_url("https://github.com/owner/repo");
        assert_eq!(
            result,
            Some((
                "github".to_string(),
                "owner".to_string(),
                "repo".to_string()
            ))
        );
    }

    #[test]
    fn test_parse_git_url_gitlab_ssh() {
        let result = parse_git_url("git@gitlab.com:company/project.git");
        assert_eq!(
            result,
            Some((
                "gitlab".to_string(),
                "company".to_string(),
                "project".to_string()
            ))
        );
    }

    #[test]
    fn test_parse_git_url_gitlab_https() {
        let result = parse_git_url("https://gitlab.com/company/project.git");
        assert_eq!(
            result,
            Some((
                "gitlab".to_string(),
                "company".to_string(),
                "project".to_string()
            ))
        );
    }

    #[test]
    fn test_parse_git_url_unknown_host() {
        let result = parse_git_url("git@bitbucket.org:owner/repo.git");
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_git_url_invalid() {
        assert_eq!(parse_git_url("not-a-url"), None);
        assert_eq!(parse_git_url(""), None);
    }

    #[test]
    fn test_build_config_with_github() {
        let options = InitOptions {
            context_name: Some("my-project".to_string()),
            github: Some(GitHubConfig {
                owner: "owner".to_string(),
                repo: "repo".to_string(),
                base_url: None,
            }),
            ..Default::default()
        };

        let config = build_config(&options);

        assert!(config.contexts.contains_key("my-project"));
        let ctx = config.contexts.get("my-project").unwrap();
        assert!(ctx.github.is_some());
        assert_eq!(ctx.github.as_ref().unwrap().owner, "owner");
        assert_eq!(ctx.github.as_ref().unwrap().repo, "repo");
    }

    #[test]
    fn test_build_config_empty_options() {
        let options = InitOptions::default();
        let config = build_config(&options);
        assert!(config.contexts.is_empty());
    }

    // ==========================================================================
    // should_skip_git_detect tests
    // ==========================================================================

    #[test]
    fn test_skip_git_detect_default_behaviour() {
        // Plain `devboy init --yes` — no proxy, no remote config, no override.
        // Must preserve pre-existing auto-detect behaviour.
        assert!(!should_skip_git_detect(false, None, false));
    }

    #[test]
    fn test_skip_git_detect_proxy_only() {
        // --proxy --proxy-only keeps its original skip behaviour.
        assert!(should_skip_git_detect(true, None, false));
    }

    #[test]
    fn test_skip_git_detect_remote_config_implies_skip() {
        // --remote-config-url alone now suppresses git auto-detection.
        assert!(should_skip_git_detect(
            false,
            Some("https://example.com/config"),
            false
        ));
    }

    #[test]
    fn test_skip_git_detect_remote_config_with_detect_git_override() {
        // Users who want both (remote config + local git-detected provider) opt in
        // explicitly via --detect-git.
        assert!(!should_skip_git_detect(
            false,
            Some("https://example.com/config"),
            true
        ));
    }

    #[test]
    fn test_skip_git_detect_proxy_only_beats_detect_git() {
        // --proxy-only is an explicit skip and wins even if --detect-git is also set.
        assert!(should_skip_git_detect(
            true,
            Some("https://example.com/config"),
            true
        ));
    }

    #[test]
    fn test_skip_git_detect_detect_git_without_remote_config_is_noop() {
        // --detect-git with no remote config URL doesn't spuriously flip behaviour.
        assert!(!should_skip_git_detect(false, None, true));
    }

    #[test]
    fn test_build_config_multiple_providers() {
        let options = InitOptions {
            context_name: Some("test".to_string()),
            github: Some(GitHubConfig {
                owner: "gh-owner".to_string(),
                repo: "gh-repo".to_string(),
                base_url: None,
            }),
            gitlab: Some(GitLabConfig {
                url: "https://gitlab.com".to_string(),
                project_id: "gl/project".to_string(),
            }),
            ..Default::default()
        };

        let config = build_config(&options);
        let ctx = config.contexts.get("test").unwrap();

        assert!(ctx.github.is_some());
        assert!(ctx.gitlab.is_some());
        assert!(ctx.clickup.is_none());
        assert!(ctx.jira.is_none());
    }

    // ==========================================================================
    // Proxy configuration tests
    // ==========================================================================

    #[test]
    fn test_build_config_with_proxy() {
        let options = InitOptions {
            context_name: Some("test".to_string()),
            proxy: Some(ProxyMcpServerConfig {
                name: "my-proxy".to_string(),
                url: "https://example.com/mcp".to_string(),
                auth_type: "bearer".to_string(),
                token_key: Some("proxy.token".to_string()),
                tool_prefix: None,
                transport: "streamable-http".to_string(),
                routing: None,
            }),
            ..Default::default()
        };

        let config = build_config(&options);

        assert_eq!(config.proxy_mcp_servers.len(), 1);
        let proxy = &config.proxy_mcp_servers[0];
        assert_eq!(proxy.name, "my-proxy");
        assert_eq!(proxy.url, "https://example.com/mcp");
        assert_eq!(proxy.auth_type, "bearer");
        assert_eq!(proxy.token_key, Some("proxy.token".to_string()));
        assert_eq!(proxy.transport, "streamable-http");
    }

    #[test]
    fn test_build_config_with_proxy_no_token() {
        let options = InitOptions {
            proxy: Some(ProxyMcpServerConfig {
                name: "proxy".to_string(),
                url: "https://example.com/mcp".to_string(),
                auth_type: "none".to_string(),
                token_key: None,
                tool_prefix: None,
                transport: "sse".to_string(),
                routing: None,
            }),
            ..Default::default()
        };

        let config = build_config(&options);

        assert_eq!(config.proxy_mcp_servers.len(), 1);
        let proxy = &config.proxy_mcp_servers[0];
        assert_eq!(proxy.auth_type, "none");
        assert!(proxy.token_key.is_none());
        assert_eq!(proxy.transport, "sse");
    }

    #[test]
    fn test_build_config_with_github_and_proxy() {
        let options = InitOptions {
            context_name: Some("combined".to_string()),
            github: Some(GitHubConfig {
                owner: "owner".to_string(),
                repo: "repo".to_string(),
                base_url: None,
            }),
            proxy: Some(ProxyMcpServerConfig {
                name: "devboy-cloud".to_string(),
                url: "https://app.devboy.pro/api/mcp".to_string(),
                auth_type: "bearer".to_string(),
                token_key: Some("devboy.token".to_string()),
                tool_prefix: None,
                transport: "streamable-http".to_string(),
                routing: None,
            }),
            ..Default::default()
        };

        let config = build_config(&options);

        // Check context with GitHub
        assert!(config.contexts.contains_key("combined"));
        let ctx = config.contexts.get("combined").unwrap();
        assert!(ctx.github.is_some());

        // Check proxy
        assert_eq!(config.proxy_mcp_servers.len(), 1);
        assert_eq!(config.proxy_mcp_servers[0].name, "devboy-cloud");
    }

    // ==========================================================================
    // Claude MCP registration tests
    // ==========================================================================

    /// Helper to create a test claude.json config in a temp directory
    fn create_test_claude_config(content: &str) -> tempfile::TempDir {
        let tmp_dir = tempfile::tempdir().unwrap();
        let claude_json = tmp_dir.path().join(".claude.json");
        std::fs::write(&claude_json, content).unwrap();
        tmp_dir
    }

    /// Helper to call production code with a custom config path for testing
    fn register_claude_mcp_to_test_path(server_name: &str, home: &std::path::Path) -> Result<()> {
        let config_path = home.join(".claude.json");
        register_claude_mcp_to_path(server_name, &config_path)
    }

    #[test]
    fn test_register_claude_mcp_default_name() {
        let tmp_dir = tempfile::tempdir().unwrap();

        register_claude_mcp_to_test_path("devboy", tmp_dir.path()).unwrap();

        let content = std::fs::read_to_string(tmp_dir.path().join(".claude.json")).unwrap();
        let config: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert!(config["mcpServers"]["devboy"].is_object());
        assert_eq!(config["mcpServers"]["devboy"]["command"], "devboy");
        assert_eq!(config["mcpServers"]["devboy"]["args"][0], "mcp");
    }

    #[test]
    fn test_register_claude_mcp_custom_name() {
        let tmp_dir = tempfile::tempdir().unwrap();

        register_claude_mcp_to_test_path("my-custom-server", tmp_dir.path()).unwrap();

        let content = std::fs::read_to_string(tmp_dir.path().join(".claude.json")).unwrap();
        let config: serde_json::Value = serde_json::from_str(&content).unwrap();

        // Should be registered under custom name, not "devboy"
        assert!(config["mcpServers"]["devboy"].is_null());
        assert!(config["mcpServers"]["my-custom-server"].is_object());
        assert_eq!(
            config["mcpServers"]["my-custom-server"]["command"],
            "devboy"
        );
        assert_eq!(config["mcpServers"]["my-custom-server"]["args"][0], "mcp");
    }

    #[test]
    fn test_register_claude_mcp_preserves_existing_servers() {
        let tmp_dir = create_test_claude_config(
            r#"{
            "mcpServers": {
                "existing-server": {
                    "command": "some-cmd",
                    "args": ["arg1"]
                }
            }
        }"#,
        );

        register_claude_mcp_to_test_path("my-proxy", tmp_dir.path()).unwrap();

        let content = std::fs::read_to_string(tmp_dir.path().join(".claude.json")).unwrap();
        let config: serde_json::Value = serde_json::from_str(&content).unwrap();

        // Existing server should be preserved
        assert!(config["mcpServers"]["existing-server"].is_object());
        assert_eq!(
            config["mcpServers"]["existing-server"]["command"],
            "some-cmd"
        );

        // New server should be added
        assert!(config["mcpServers"]["my-proxy"].is_object());
        assert_eq!(config["mcpServers"]["my-proxy"]["command"], "devboy");
    }

    #[test]
    fn test_register_claude_mcp_creates_mcp_servers_section() {
        let tmp_dir = create_test_claude_config(r#"{"someOtherKey": "value"}"#);

        register_claude_mcp_to_test_path("devboy", tmp_dir.path()).unwrap();

        let content = std::fs::read_to_string(tmp_dir.path().join(".claude.json")).unwrap();
        let config: serde_json::Value = serde_json::from_str(&content).unwrap();

        // Original key should be preserved
        assert_eq!(config["someOtherKey"], "value");

        // mcpServers should be created
        assert!(config["mcpServers"]["devboy"].is_object());
    }

    #[test]
    fn test_register_claude_mcp_creates_file_if_not_exists() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let claude_json = tmp_dir.path().join(".claude.json");

        // File should not exist initially
        assert!(!claude_json.exists());

        register_claude_mcp_to_test_path("devboy", tmp_dir.path()).unwrap();

        // File should now exist
        assert!(claude_json.exists());

        let content = std::fs::read_to_string(&claude_json).unwrap();
        let config: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(config["mcpServers"]["devboy"].is_object());
    }

    #[test]
    fn test_register_claude_mcp_fails_on_non_object_root() {
        let tmp_dir = create_test_claude_config(r#"[]"#);

        let result = register_claude_mcp_to_test_path("devboy", tmp_dir.path());

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not a JSON object")
        );
    }

    #[test]
    fn test_register_claude_mcp_fails_on_non_object_mcp_servers() {
        let tmp_dir = create_test_claude_config(r#"{"mcpServers": "invalid"}"#);

        let result = register_claude_mcp_to_test_path("devboy", tmp_dir.path());

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not an object"));
    }

    // ==========================================================================
    // Kimi MCP registration tests
    // ==========================================================================

    /// Helper to create a test kimi mcp.json config in a temp directory
    fn create_test_kimi_config(content: &str) -> tempfile::TempDir {
        let tmp_dir = tempfile::tempdir().unwrap();
        let kimi_dir = tmp_dir.path().join(".kimi");
        std::fs::create_dir_all(&kimi_dir).unwrap();
        let kimi_json = kimi_dir.join("mcp.json");
        std::fs::write(&kimi_json, content).unwrap();
        tmp_dir
    }

    /// Helper to call production code with a custom config path for testing
    fn register_kimi_mcp_to_test_path(
        server_name: &str,
        base_path: &std::path::Path,
    ) -> Result<()> {
        let config_path = base_path.join(".kimi").join("mcp.json");
        // Ensure .kimi directory exists
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        register_kimi_mcp_to_path(server_name, &config_path)
    }

    #[test]
    fn test_register_kimi_mcp_default_name() {
        let tmp_dir = tempfile::tempdir().unwrap();

        register_kimi_mcp_to_test_path("devboy", tmp_dir.path()).unwrap();

        let content =
            std::fs::read_to_string(tmp_dir.path().join(".kimi").join("mcp.json")).unwrap();
        let config: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert!(config["mcpServers"]["devboy"].is_object());
        assert_eq!(config["mcpServers"]["devboy"]["command"], "devboy");
        assert_eq!(config["mcpServers"]["devboy"]["args"][0], "mcp");
    }

    #[test]
    fn test_register_kimi_mcp_custom_name() {
        let tmp_dir = tempfile::tempdir().unwrap();

        register_kimi_mcp_to_test_path("my-custom-server", tmp_dir.path()).unwrap();

        let content =
            std::fs::read_to_string(tmp_dir.path().join(".kimi").join("mcp.json")).unwrap();
        let config: serde_json::Value = serde_json::from_str(&content).unwrap();

        // Should be registered under custom name, not "devboy"
        assert!(config["mcpServers"]["devboy"].is_null());
        assert!(config["mcpServers"]["my-custom-server"].is_object());
        assert_eq!(
            config["mcpServers"]["my-custom-server"]["command"],
            "devboy"
        );
        assert_eq!(config["mcpServers"]["my-custom-server"]["args"][0], "mcp");
    }

    #[test]
    fn test_register_kimi_mcp_preserves_existing_servers() {
        let tmp_dir = create_test_kimi_config(
            r#"{
            "mcpServers": {
                "existing-server": {
                    "command": "some-cmd",
                    "args": ["arg1"]
                }
            }
        }"#,
        );

        register_kimi_mcp_to_test_path("my-proxy", tmp_dir.path()).unwrap();

        let content =
            std::fs::read_to_string(tmp_dir.path().join(".kimi").join("mcp.json")).unwrap();
        let config: serde_json::Value = serde_json::from_str(&content).unwrap();

        // Existing server should be preserved
        assert!(config["mcpServers"]["existing-server"].is_object());
        assert_eq!(
            config["mcpServers"]["existing-server"]["command"],
            "some-cmd"
        );

        // New server should be added
        assert!(config["mcpServers"]["my-proxy"].is_object());
        assert_eq!(config["mcpServers"]["my-proxy"]["command"], "devboy");
    }

    #[test]
    fn test_register_kimi_mcp_creates_mcp_servers_section() {
        let tmp_dir = create_test_kimi_config(r#"{"someOtherKey": "value"}"#);

        register_kimi_mcp_to_test_path("devboy", tmp_dir.path()).unwrap();

        let content =
            std::fs::read_to_string(tmp_dir.path().join(".kimi").join("mcp.json")).unwrap();
        let config: serde_json::Value = serde_json::from_str(&content).unwrap();

        // Original key should be preserved
        assert_eq!(config["someOtherKey"], "value");

        // mcpServers should be created
        assert!(config["mcpServers"]["devboy"].is_object());
    }

    #[test]
    fn test_register_kimi_mcp_creates_file_if_not_exists() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let kimi_json = tmp_dir.path().join(".kimi").join("mcp.json");

        // File should not exist initially
        assert!(!kimi_json.exists());

        register_kimi_mcp_to_test_path("devboy", tmp_dir.path()).unwrap();

        // File should now exist
        assert!(kimi_json.exists());

        let content = std::fs::read_to_string(&kimi_json).unwrap();
        let config: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(config["mcpServers"]["devboy"].is_object());
    }

    #[test]
    fn test_register_kimi_mcp_creates_kimi_directory() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let kimi_dir = tmp_dir.path().join(".kimi");
        let kimi_json = kimi_dir.join("mcp.json");

        // .kimi directory should not exist initially
        assert!(!kimi_dir.exists());

        register_kimi_mcp_to_test_path("devboy", tmp_dir.path()).unwrap();

        // .kimi directory and mcp.json should now exist
        assert!(kimi_dir.exists());
        assert!(kimi_json.exists());
    }

    #[test]
    fn test_register_kimi_mcp_fails_on_non_object_root() {
        let tmp_dir = create_test_kimi_config(r#"[]"#);

        let result = register_kimi_mcp_to_test_path("devboy", tmp_dir.path());

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not a JSON object")
        );
    }

    #[test]
    fn test_register_kimi_mcp_fails_on_non_object_mcp_servers() {
        let tmp_dir = create_test_kimi_config(r#"{"mcpServers": "invalid"}"#);

        let result = register_kimi_mcp_to_test_path("devboy", tmp_dir.path());

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not an object"));
    }

    // ==========================================================================
    // Codex MCP registration tests (fallback TOML path)
    // ==========================================================================

    #[test]
    fn test_register_codex_mcp_to_path_default_name() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let config_path = tmp_dir.path().join(".codex").join("config.toml");

        register_codex_mcp_to_path("devboy", &config_path).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config: toml::Value = content.parse().unwrap();

        assert!(config.get("mcp_servers").unwrap().get("devboy").is_some());
        assert_eq!(
            config["mcp_servers"]["devboy"]["command"].as_str().unwrap(),
            "devboy"
        );
        let args = config["mcp_servers"]["devboy"]["args"].as_array().unwrap();
        assert_eq!(args[0].as_str().unwrap(), "mcp");
    }

    #[test]
    fn test_register_codex_mcp_to_path_preserves_existing() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let config_path = tmp_dir.path().join(".codex").join("config.toml");

        let existing = r#"
[model]
provider = "openai"

[mcp_servers.existing]
command = "npx"
args = ["old"]
"#;
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, existing).unwrap();

        register_codex_mcp_to_path("devboy", &config_path).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config: toml::Value = content.parse().unwrap();

        assert!(config.get("mcp_servers").unwrap().get("existing").is_some());
        assert!(config.get("mcp_servers").unwrap().get("devboy").is_some());
        assert_eq!(config["model"]["provider"].as_str().unwrap(), "openai");
    }

    // ==========================================================================
    // Copilot MCP registration tests
    // ==========================================================================

    #[test]
    fn test_register_copilot_mcp_to_path_default_name() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let config_path = tmp_dir.path().join(".copilot").join("mcp-config.json");

        register_copilot_mcp_to_path("devboy", &config_path).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert!(config["mcpServers"]["devboy"].is_object());
        assert_eq!(config["mcpServers"]["devboy"]["type"], "local");
        assert_eq!(config["mcpServers"]["devboy"]["command"], "devboy");
        assert_eq!(config["mcpServers"]["devboy"]["args"][0], "mcp");
        assert_eq!(config["mcpServers"]["devboy"]["tools"][0], "*");
    }

    // ==========================================================================
    // Gemini MCP registration tests (fallback JSON path)
    // ==========================================================================

    fn register_gemini_mcp_to_test_path(server_name: &str, base: &std::path::Path) -> Result<()> {
        let config_path = base.join(".gemini").join("settings.json");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        register_json_mcp_config(
            server_name,
            &config_path,
            "mcpServers",
            serde_json::json!({ "command": "devboy", "args": ["mcp"], "trust": true }),
            "gemini",
        )
    }

    #[test]
    fn test_register_gemini_mcp_default_name() {
        let tmp_dir = tempfile::tempdir().unwrap();

        register_gemini_mcp_to_test_path("devboy", tmp_dir.path()).unwrap();

        let content =
            std::fs::read_to_string(tmp_dir.path().join(".gemini").join("settings.json")).unwrap();
        let config: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert!(config["mcpServers"]["devboy"].is_object());
        assert_eq!(config["mcpServers"]["devboy"]["command"], "devboy");
        assert_eq!(config["mcpServers"]["devboy"]["args"][0], "mcp");
        assert_eq!(config["mcpServers"]["devboy"]["trust"], true);
    }

    // ==========================================================================
    // OpenCode MCP registration tests
    // ==========================================================================

    #[test]
    fn test_register_opencode_mcp_default_name() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let config_path = tmp_dir.path().join("opencode.json");

        register_opencode_mcp_to_path("devboy", &config_path).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert!(config["mcp"]["devboy"].is_object());
        assert_eq!(config["mcp"]["devboy"]["type"], "local");
        assert_eq!(config["mcp"]["devboy"]["command"], "devboy");
        assert_eq!(config["mcp"]["devboy"]["args"][0], "mcp");
    }

    #[test]
    fn test_register_opencode_mcp_preserves_existing() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let config_path = tmp_dir.path().join("opencode.json");
        std::fs::write(&config_path, r#"{"existingKey": "value"}"#).unwrap();

        register_opencode_mcp_to_path("devboy", &config_path).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert_eq!(config["existingKey"], "value");
        assert!(config["mcp"]["devboy"].is_object());
    }

    // ==========================================================================
    // ForgeCode MCP registration tests
    // ==========================================================================

    #[test]
    fn test_register_forge_mcp_default_name() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let config_path = tmp_dir.path().join(".mcp.json");

        register_forge_mcp_to_path("devboy", &config_path).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert!(config["mcpServers"]["devboy"].is_object());
        assert_eq!(config["mcpServers"]["devboy"]["command"], "devboy");
        assert_eq!(config["mcpServers"]["devboy"]["args"][0], "mcp");
    }

    #[test]
    fn test_register_forge_mcp_preserves_existing() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let config_path = tmp_dir.path().join(".mcp.json");
        std::fs::write(&config_path, r#"{"existingKey": "value"}"#).unwrap();

        register_forge_mcp_to_path("devboy", &config_path).unwrap();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let config: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert_eq!(config["existingKey"], "value");
        assert!(config["mcpServers"]["devboy"].is_object());
    }

    // ==========================================================================
    // Env-only context tests
    // ==========================================================================

    #[test]
    fn test_parse_context_env_key_github() {
        let result = parse_context_env_key("PROD_GITHUB_OWNER");
        assert_eq!(
            result,
            Some((
                "PROD".to_string(),
                "GITHUB".to_string(),
                "OWNER".to_string()
            ))
        );
    }

    #[test]
    fn test_parse_context_env_key_gitlab() {
        let result = parse_context_env_key("MY_PROJECT_GITLAB_PROJECT_ID");
        assert_eq!(
            result,
            Some((
                "MY_PROJECT".to_string(),
                "GITLAB".to_string(),
                "PROJECT_ID".to_string()
            ))
        );
    }

    #[test]
    fn test_parse_context_env_key_clickup() {
        let result = parse_context_env_key("DEV_CLICKUP_LIST_ID");
        assert_eq!(
            result,
            Some((
                "DEV".to_string(),
                "CLICKUP".to_string(),
                "LIST_ID".to_string()
            ))
        );
    }

    #[test]
    fn test_parse_context_env_key_jira() {
        let result = parse_context_env_key("STAGING_JIRA_URL");
        assert_eq!(
            result,
            Some(("STAGING".to_string(), "JIRA".to_string(), "URL".to_string()))
        );
    }

    #[test]
    fn test_parse_context_env_key_invalid() {
        assert_eq!(parse_context_env_key("INVALID_KEY"), None);
        assert_eq!(parse_context_env_key("GITHUB_OWNER"), None); // Missing context name
        assert_eq!(parse_context_env_key("PROD_GITHUB_"), None); // Missing field
        assert_eq!(parse_context_env_key("_GITHUB_OWNER"), None); // Missing context name
    }

    #[test]
    fn test_env_context_builder_github() {
        let mut builder = EnvContextBuilder::new("TEST".to_string());
        builder.set_field("GITHUB", "OWNER", "my-org".to_string());
        builder.set_field("GITHUB", "REPO", "my-repo".to_string());

        let context = builder.build().unwrap();
        let gh = context.github.unwrap();
        assert_eq!(gh.owner, "my-org");
        assert_eq!(gh.repo, "my-repo");
        assert!(gh.base_url.is_none());
    }

    #[test]
    fn test_env_context_builder_github_with_base_url() {
        let mut builder = EnvContextBuilder::new("TEST".to_string());
        builder.set_field("GITHUB", "OWNER", "my-org".to_string());
        builder.set_field("GITHUB", "REPO", "my-repo".to_string());
        builder.set_field(
            "GITHUB",
            "BASE_URL",
            "https://github.example.com".to_string(),
        );

        let context = builder.build().unwrap();
        let gh = context.github.unwrap();
        assert_eq!(gh.base_url, Some("https://github.example.com".to_string()));
    }

    #[test]
    fn test_env_context_builder_gitlab() {
        let mut builder = EnvContextBuilder::new("TEST".to_string());
        builder.set_field("GITLAB", "PROJECT_ID", "123".to_string());

        let context = builder.build().unwrap();
        let gl = context.gitlab.unwrap();
        assert_eq!(gl.project_id, "123");
        assert_eq!(gl.url, "https://gitlab.com"); // default
    }

    #[test]
    fn test_env_context_builder_gitlab_with_url() {
        let mut builder = EnvContextBuilder::new("TEST".to_string());
        builder.set_field("GITLAB", "URL", "https://gitlab.example.com".to_string());
        builder.set_field("GITLAB", "PROJECT_ID", "owner/repo".to_string());

        let context = builder.build().unwrap();
        let gl = context.gitlab.unwrap();
        assert_eq!(gl.url, "https://gitlab.example.com");
        assert_eq!(gl.project_id, "owner/repo");
    }

    #[test]
    fn test_env_context_builder_clickup() {
        let mut builder = EnvContextBuilder::new("TEST".to_string());
        builder.set_field("CLICKUP", "LIST_ID", "abc123".to_string());

        let context = builder.build().unwrap();
        let cu = context.clickup.unwrap();
        assert_eq!(cu.list_id, "abc123");
        assert!(cu.team_id.is_none());
    }

    #[test]
    fn test_env_context_builder_clickup_with_team() {
        let mut builder = EnvContextBuilder::new("TEST".to_string());
        builder.set_field("CLICKUP", "LIST_ID", "abc123".to_string());
        builder.set_field("CLICKUP", "TEAM_ID", "team456".to_string());

        let context = builder.build().unwrap();
        let cu = context.clickup.unwrap();
        assert_eq!(cu.list_id, "abc123");
        assert_eq!(cu.team_id, Some("team456".to_string()));
    }

    #[test]
    fn test_env_context_builder_jira() {
        let mut builder = EnvContextBuilder::new("TEST".to_string());
        builder.set_field("JIRA", "URL", "https://jira.example.com".to_string());
        builder.set_field("JIRA", "PROJECT_KEY", "PROJ".to_string());
        builder.set_field("JIRA", "EMAIL", "user@example.com".to_string());

        let context = builder.build().unwrap();
        let jira = context.jira.unwrap();
        assert_eq!(jira.url, "https://jira.example.com");
        assert_eq!(jira.project_key, "PROJ");
        assert_eq!(jira.email, "user@example.com");
    }

    #[test]
    fn test_env_context_builder_jira_incomplete() {
        let mut builder = EnvContextBuilder::new("TEST".to_string());
        builder.set_field("JIRA", "URL", "https://jira.example.com".to_string());
        // Missing project_key and email

        let context = builder.build();
        // Should return None because Jira requires all three fields
        assert!(context.is_none() || context.as_ref().map(|c| c.jira.is_none()).unwrap_or(true));
    }

    #[test]
    fn test_env_context_builder_github_incomplete() {
        let mut builder = EnvContextBuilder::new("TEST".to_string());
        builder.set_field("GITHUB", "OWNER", "my-org".to_string());
        // Missing repo

        let context = builder.build();
        // Should return None because GitHub requires both owner and repo
        assert!(context.is_none() || context.as_ref().map(|c| c.github.is_none()).unwrap_or(true));
    }

    #[test]
    fn test_env_context_builder_empty() {
        let builder = EnvContextBuilder::new("TEST".to_string());
        let context = builder.build();
        assert!(context.is_none());
    }

    #[test]
    fn test_env_context_builder_multiple_providers() {
        let mut builder = EnvContextBuilder::new("TEST".to_string());
        builder.set_field("GITHUB", "OWNER", "my-org".to_string());
        builder.set_field("GITHUB", "REPO", "my-repo".to_string());
        builder.set_field("GITLAB", "PROJECT_ID", "123".to_string());

        let context = builder.build().unwrap();
        assert!(context.github.is_some());
        assert!(context.gitlab.is_some());
        assert!(context.clickup.is_none());
        assert!(context.jira.is_none());
    }

    #[test]
    fn test_env_context_builder_aliases() {
        // Test that PROJECT alias works for GITLAB
        let mut builder = EnvContextBuilder::new("TEST".to_string());
        builder.set_field("GITLAB", "PROJECT", "owner/repo".to_string());

        let context = builder.build().unwrap();
        let gl = context.gitlab.unwrap();
        assert_eq!(gl.project_id, "owner/repo");
    }

    #[test]
    fn test_env_context_builder_list_alias() {
        // Test that LIST alias works for CLICKUP
        let mut builder = EnvContextBuilder::new("TEST".to_string());
        builder.set_field("CLICKUP", "LIST", "abc123".to_string());

        let context = builder.build().unwrap();
        let cu = context.clickup.unwrap();
        assert_eq!(cu.list_id, "abc123");
    }

    // =========================================================================
    // Format Pipeline (pipe mode) tests
    // =========================================================================

    #[test]
    fn test_detect_data_type_issues() {
        let json: serde_json::Value = serde_json::json!([
            {"key": "gh#1", "title": "Bug", "state": "open"}
        ]);
        assert_eq!(detect_data_type(&json), "issues");
    }

    #[test]
    fn test_detect_data_type_merge_requests() {
        let json: serde_json::Value = serde_json::json!([
            {"key": "pr#1", "title": "Fix", "source_branch": "feat", "target_branch": "main"}
        ]);
        assert_eq!(detect_data_type(&json), "merge_requests");
    }

    #[test]
    fn test_detect_data_type_diffs() {
        let json: serde_json::Value = serde_json::json!([
            {"file_path": "src/main.rs", "diff": "+line"}
        ]);
        assert_eq!(detect_data_type(&json), "diffs");
    }

    #[test]
    fn test_detect_data_type_discussions() {
        let json: serde_json::Value = serde_json::json!([
            {"id": "d1", "resolved": false, "comments": []}
        ]);
        assert_eq!(detect_data_type(&json), "discussions");
    }

    #[test]
    fn test_detect_data_type_comments() {
        let json: serde_json::Value = serde_json::json!([
            {"id": "c1", "body": "LGTM", "author": null}
        ]);
        assert_eq!(detect_data_type(&json), "comments");
    }

    #[test]
    fn test_detect_data_type_unknown() {
        let json: serde_json::Value = serde_json::json!({"foo": "bar"});
        assert_eq!(detect_data_type(&json), "unknown");
    }

    #[test]
    fn test_detect_data_type_empty_array() {
        let json: serde_json::Value = serde_json::json!([]);
        assert_eq!(detect_data_type(&json), "unknown");
    }

    #[test]
    fn test_trim_to_budget_fits() {
        let items = vec![1, 2, 3];
        let result = trim_to_budget(&items, 100000, |items| Ok(format!("{:?}", items))).unwrap();
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_trim_to_budget_trims() {
        let items: Vec<String> = (0..100).map(|i| format!("item_{i}")).collect();
        let result = trim_to_budget(&items, 10, |items| {
            // Each item ~7 chars → ~2 tokens. 100 items → 200 tokens
            Ok(items.join(","))
        })
        .unwrap();
        assert!(result.len() < 100);
        assert!(!result.is_empty());
    }

    #[test]
    fn test_trim_to_budget_keeps_at_least_one() {
        let items = vec!["a very long string that exceeds budget".to_string()];
        let result = trim_to_budget(&items, 1, |items| Ok(items.join(","))).unwrap();
        assert_eq!(result.len(), 1);
    }

    // -- CLI auto-docs --------------------------------------------------------

    #[test]
    fn cli_clap_definition_parses() {
        // The clap-markdown renderer chokes on an invalid Command tree
        // (missing about, unreachable subcommand). This is the cheapest
        // smoke test: just instantiate the tree and require a few
        // landmark subcommands so renames don't go unnoticed.
        use clap::CommandFactory;
        let cmd = Cli::command();
        let names: Vec<&str> = cmd.get_subcommands().map(|c| c.get_name()).collect();
        for required in ["init", "mcp", "tools", "docs", "skills", "doctor"] {
            assert!(
                names.contains(&required),
                "expected top-level subcommand `{required}`, got {names:?}"
            );
        }
    }

    #[test]
    fn cli_markdown_renderer_emits_expected_landmarks() {
        let body = render_cli_markdown();
        // Preamble (so reviewers see "do not edit" before they touch the file).
        assert!(
            body.contains("Auto-generated by `devboy docs cli`"),
            "preamble missing from CLI reference"
        );
        // The `docs cli` subcommand itself must appear *as a rendered
        // section heading*, not just as the substring inside the
        // preamble — otherwise a refactor that drops the command would
        // still pass this assertion. (Copilot review on PR #208.)
        assert!(
            body.contains("## `devboy docs cli`"),
            "`devboy docs cli` section heading missing from CLI reference"
        );
        // A UTF-8-safe preview for the failure message — the rendered
        // markdown contains em-dashes and other multi-byte characters,
        // so byte-slicing at an arbitrary index could panic *inside*
        // the assertion message and obscure the real failure.
        let preview: String = body.chars().take(200).collect();
        // A landmark from each major area, so a refactor that drops
        // a whole section trips this test.
        for required in ["devboy init", "devboy tools docs", "devboy skills"] {
            assert!(
                body.contains(required),
                "expected `{required}` in CLI reference, scan around `{preview}`"
            );
        }
    }

    #[test]
    fn committed_cli_md_matches_renderer() {
        // P-203-style drift gate, but enforced at `cargo test --workspace`
        // time so contributors notice before they push. The CI step
        // `Check CLI reference is up to date` is the secondary safety net.
        //
        // Anchor the path on `CARGO_MANIFEST_DIR` (cargo sets this to
        // the crate root at compile time) so the test does not depend on
        // the working directory — `cargo test --workspace` from the repo
        // root and IDE runners on macOS / Windows both pass it through.
        let crate_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let path = crate_root.join("../../docs/guide/reference/cli.md");
        let committed = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "docs/guide/reference/cli.md missing at {} — run \
                 `cargo run -p devboy-cli -- docs cli --output \
                 docs/guide/reference/cli.md` from the repo root: {e}",
                path.display()
            )
        });
        let rendered = render_cli_markdown();
        // Normalise line endings before comparing — `.gitattributes` already
        // forces LF on this file, but Windows checkouts with
        // `core.autocrlf=true` rewrite to CRLF locally and would trip the
        // gate. Compare in a CRLF-tolerant way so the test is platform-
        // independent.
        let committed_n = committed.replace("\r\n", "\n");
        let rendered_n = rendered.replace("\r\n", "\n");
        if committed_n != rendered_n {
            panic!(
                "docs/guide/reference/cli.md is out of date — re-run \
                 `cargo run -p devboy-cli -- docs cli --output \
                 docs/guide/reference/cli.md` from the repo root and commit \
                 the result"
            );
        }
    }
}
