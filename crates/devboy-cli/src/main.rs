//! DevBoy CLI - Command-line interface for devboy-tools.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use devboy_clickup::ClickUpClient;
use devboy_core::{
    Config, ContextConfig, IssueFilter, IssueProvider, MergeRequestProvider, MrFilter, Provider,
};
use devboy_github::GitHubClient;
use devboy_gitlab::GitLabClient;
use devboy_jira::JiraClient;
use devboy_mcp::{McpProxyClient, McpServer, ProxyManager, ProxyTransport};
use devboy_storage::{CredentialStore, KeychainStore};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "devboy")]
#[command(author, version, about = "DevBoy - AI-powered development tools", long_about = None)]
struct Cli {
    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the MCP server (stdio mode for AI assistants)
    Mcp,

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
        #[arg(short, long, default_value = "open")]
        state: String,

        /// Maximum number of issues to display
        #[arg(short, long, default_value = "20")]
        limit: u32,
    },

    /// Get information about merge requests / pull requests
    Mrs {
        /// Filter by state
        #[arg(short, long, default_value = "open")]
        state: String,

        /// Maximum number of MRs to display
        #[arg(short, long, default_value = "20")]
        limit: u32,
    },

    /// Test provider connection
    Test {
        /// Provider to test (github, gitlab, clickup, jira)
        provider: String,
    },

    /// Interact with upstream MCP proxy servers
    Proxy {
        #[command(subcommand)]
        command: ProxyCommands,
    },
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Set a configuration value
    Set {
        /// Config key (e.g., github.owner, gitlab.url)
        key: String,
        /// Config value
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
}

#[derive(Subcommand)]
enum ContextCommands {
    /// List available contexts and show active one
    List,
    /// Switch active context
    Use { name: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };

    tracing_subscriber::fmt().with_env_filter(filter).init();

    match cli.command {
        Some(Commands::Mcp) => {
            handle_mcp_command().await?;
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

        None => {
            println!("DevBoy - AI-powered development tools");
            println!("Run with --help for usage information");
        }
    }

    Ok(())
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
            let store = KeychainStore::new();
            store
                .store(&key, &value)
                .context("Failed to store secret")?;
            println!("Secret {} stored in keychain", key);
        }

        ConfigCommands::Get { key } => {
            // First try config file
            let config = Config::load().context("Failed to load config")?;
            if let Some(value) = config.get(&key).context("Failed to get config value")? {
                println!("{}", value);
                return Ok(());
            }

            // Then try keychain
            let store = KeychainStore::new();
            if let Some(value) = store.get(&key).ok().flatten() {
                println!("{} (from keychain)", mask_secret(&value));
                return Ok(());
            }

            println!("(not set)");
        }

        ConfigCommands::List => {
            let config = Config::load().context("Failed to load config")?;
            let store = KeychainStore::new();

            println!("Configuration:");
            println!();

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

            if !config.has_any_provider() {
                println!("No providers configured.");
                println!();
                println!("To configure GitHub:");
                println!("  devboy config set github.owner <owner>");
                println!("  devboy config set github.repo <repo>");
                println!("  devboy config set-secret github.token <token>");
            }
        }

        ConfigCommands::Path => match Config::config_path() {
            Ok(path) => println!("{}", path.display()),
            Err(e) => println!("Error: {}", e),
        },
    }

    Ok(())
}

fn mask_secret(value: &str) -> String {
    if value.len() <= 8 {
        "*".repeat(value.len())
    } else {
        format!("{}...{}", &value[..4], &value[value.len() - 4..])
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
    let config = Config::load().context("Failed to load config")?;
    let store = KeychainStore::new();

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
            .context("Failed to fetch issues")?;

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
    let config = Config::load().context("Failed to load config")?;
    let store = KeychainStore::new();

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
            .context("Failed to fetch PRs")?;

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
    let config = Config::load().context("Failed to load config")?;
    let store = KeychainStore::new();

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
                println!("  Hint: Set team_id for custom task IDs (e.g., DEV-42) and better integration:");
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

        _ => {
            println!("Unknown provider: {}", provider);
            println!("Supported providers: github, gitlab, clickup, jira");
        }
    }

    Ok(())
}

// =============================================================================
// MCP Command
// =============================================================================

async fn handle_mcp_command() -> Result<()> {
    let (config, config_path) = load_runtime_config()?;
    let store = KeychainStore::new();

    let mut server = McpServer::new();
    let mut any_provider_added = false;

    // Add configured named contexts.
    for (context_name, context) in &config.contexts {
        server.ensure_context(context_name);
        any_provider_added |= add_context_providers(&mut server, &store, context_name, context);
    }

    // Backward-compatible implicit default context from top-level provider fields.
    // Skip when explicit `contexts.default` exists to match Config::get_context precedence.
    if !config.contexts.contains_key(Config::DEFAULT_CONTEXT_NAME) {
        if let Some(default_context) = config.legacy_default_context() {
            any_provider_added |= add_context_providers(
                &mut server,
                &store,
                Config::DEFAULT_CONTEXT_NAME,
                &default_context,
            );
        }
    }

    // Set active context (if configured and valid).
    if let Some(active) = config.resolve_active_context_name() {
        if let Err(e) = server.set_active_context(&active) {
            tracing::warn!("Could not set active context '{}': {}", active, e);
        } else {
            tracing::info!("Active context: {}", active);
        }
    }

    // Connect to upstream MCP proxy servers (if configured).
    if !config.proxy_mcp_servers.is_empty() {
        let mut proxy_manager = build_proxy_manager(&config, &store).await;
        if !proxy_manager.is_empty() {
            if let Err(e) = proxy_manager.fetch_all_tools().await {
                tracing::warn!("Failed to fetch proxy tools: {}", e);
            }
            server.set_proxy_manager(proxy_manager);
        }
    }

    if !any_provider_added {
        tracing::warn!("No providers configured. MCP server will have limited functionality.");
        tracing::info!("Config source: {}", config_path.display());
        tracing::info!("Configure GitHub: devboy config set github.owner <owner>");
    }

    // Run the MCP server (reads from stdin, writes to stdout)
    server.run().await.context("MCP server error")?;

    Ok(())
}

// =============================================================================
// Proxy Command
// =============================================================================

async fn handle_proxy_command(command: ProxyCommands) -> Result<()> {
    let (config, _) = load_runtime_config()?;
    let store = KeychainStore::new();

    if config.proxy_mcp_servers.is_empty() {
        println!("No proxy MCP servers configured.");
        println!("Add to config.toml:");
        println!();
        println!("  [[proxy_mcp_servers]]");
        println!("  name = \"my-server\"");
        println!("  url = \"https://example.com/api/mcp\"");
        println!("  transport = \"streamable-http\"");
        return Ok(());
    }

    let mut proxy_manager = build_proxy_manager(&config, &store).await;

    if proxy_manager.is_empty() {
        eprintln!("Could not connect to any upstream MCP server.");
        return Ok(());
    }

    match command {
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
    }

    Ok(())
}

async fn build_proxy_manager(config: &Config, store: &KeychainStore) -> ProxyManager {
    let mut proxy_manager = ProxyManager::new();
    for proxy_cfg in &config.proxy_mcp_servers {
        let token = proxy_cfg
            .token_key
            .as_deref()
            .and_then(|key| store.get(key).ok().flatten());

        let transport = ProxyTransport::parse(&proxy_cfg.transport);

        match McpProxyClient::connect(
            &proxy_cfg.name,
            &proxy_cfg.url,
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
                    proxy_cfg.url
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

fn get_token_for_context(
    store: &KeychainStore,
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
    store: &KeychainStore,
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

    added
}
