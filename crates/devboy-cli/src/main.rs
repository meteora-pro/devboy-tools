//! DevBoy CLI - Command-line interface for devboy-tools.

use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use devboy_clickup::ClickUpClient;
use devboy_core::{
    BuiltinToolsConfig, ClickUpConfig, Config, ContextConfig, GitHubConfig, GitLabConfig,
    IssueFilter, IssueProvider, JiraConfig, MergeRequestProvider, MrFilter, Provider,
    ProxyMcpServerConfig,
};
use devboy_github::GitHubClient;
use devboy_gitlab::GitLabClient;
use devboy_jira::JiraClient;
use devboy_mcp::{
    JsonRpcRequest, McpProxyClient, McpServer, ProxyManager, ProxyTransport, RequestId,
    JSONRPC_VERSION, KNOWN_BUILTIN_TOOLS,
};
use devboy_storage::{CredentialStore, KeychainStore};
use dialoguer::{Confirm, Input, MultiSelect, Password};
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

        /// Context name for the configuration
        #[arg(short, long)]
        context: Option<String>,

        /// Add MCP proxy server URL
        #[arg(long)]
        proxy: Option<String>,

        /// Proxy server name (default: "proxy")
        #[arg(long, requires = "proxy")]
        proxy_name: Option<String>,

        /// Proxy transport type: streamable-http or sse (default: streamable-http)
        #[arg(long, requires = "proxy")]
        proxy_transport: Option<String>,

        /// Keychain key for proxy token (e.g., "proxy.token")
        #[arg(long, requires = "proxy")]
        proxy_token_key: Option<String>,

        /// Proxy token value (will be stored in keychain automatically)
        #[arg(long, requires = "proxy")]
        proxy_token: Option<String>,

        /// Proxy auth type: bearer, api_key, or none (default: bearer if token provided, else none)
        #[arg(long, requires = "proxy")]
        proxy_auth_type: Option<String>,
    },

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

    /// Manage built-in tools (enable/disable). Interactive mode when run without subcommand.
    Tools {
        #[command(subcommand)]
        command: Option<ToolsCommands>,
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
    /// Add a new proxy server configuration
    Add {
        /// Proxy server name
        name: String,

        /// Proxy server URL
        #[arg(long)]
        url: String,

        /// Transport type: streamable-http or sse (default: streamable-http)
        #[arg(long, default_value = "streamable-http")]
        transport: String,

        /// Keychain key for proxy token (e.g., "proxy.token")
        #[arg(long)]
        token_key: Option<String>,

        /// Token value (will be stored in keychain automatically)
        #[arg(long)]
        token: Option<String>,

        /// Auth type: bearer, api_key, or none (default: bearer if token provided, else none)
        #[arg(long)]
        auth_type: Option<String>,

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
        Some(Commands::Init {
            yes,
            dry_run,
            force,
            claude,
            context,
            proxy,
            proxy_name,
            proxy_transport,
            proxy_token_key,
            proxy_token,
            proxy_auth_type,
        }) => {
            handle_init_command(
                yes,
                dry_run,
                force,
                claude,
                context,
                proxy,
                proxy_name,
                proxy_transport,
                proxy_token_key,
                proxy_token,
                proxy_auth_type,
            )
            .await?;
        }

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

        Some(Commands::Tools { command }) => {
            handle_tools_command(command).await?;
        }

        None => {
            println!("DevBoy - AI-powered development tools");
            println!("Run with --help for usage information");
        }
    }

    Ok(())
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
    tokens: Vec<(String, String)>, // (key, value) pairs for keychain
    proxy: Option<ProxyMcpServerConfig>,
}

async fn handle_init_command(
    yes: bool,
    dry_run: bool,
    force: bool,
    claude: bool,
    context_name: Option<String>,
    proxy_url: Option<String>,
    proxy_name: Option<String>,
    proxy_transport: Option<String>,
    proxy_token_key: Option<String>,
    proxy_token: Option<String>,
    proxy_auth_type: Option<String>,
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

    // Collect options
    let mut options = if yes {
        collect_options_auto(context_name)?
    } else {
        collect_options_interactive(context_name)?
    };

    // Add proxy configuration if provided
    if let Some(url) = proxy_url {
        let name = proxy_name.unwrap_or_else(|| "proxy".to_string());
        let transport = proxy_transport.unwrap_or_else(|| "streamable-http".to_string());

        // Determine token key: use provided or generate default
        let token_key = if proxy_token.is_some() || proxy_token_key.is_some() {
            Some(
                proxy_token_key.unwrap_or_else(|| format!("proxy.{}.token", name)),
            )
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
        let auth_type = proxy_auth_type.unwrap_or_else(|| {
            if token_key.is_some() {
                "bearer".to_string()
            } else {
                "none".to_string()
            }
        });

        options.proxy = Some(ProxyMcpServerConfig {
            name,
            url,
            auth_type,
            token_key,
            tool_prefix: None,
            transport,
        });
    }

    // Build configuration
    let config = build_config(&options);
    let toml_content =
        toml::to_string_pretty(&config).context("Failed to serialize configuration")?;

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
        let store = KeychainStore::new();
        for (key, value) in &options.tokens {
            store
                .store(key, value)
                .with_context(|| format!("Failed to store {} in keychain", key))?;
            println!("Stored {} in keychain", key);
        }
    }

    // Register with Claude Code
    if claude {
        register_claude_mcp().await?;
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
    let store = KeychainStore::new();
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
    {
        let context = ContextConfig {
            github: options.github.clone(),
            gitlab: options.gitlab.clone(),
            clickup: options.clickup.clone(),
            jira: options.jira.clone(),
        };
        config.contexts.insert(context_name, context);
    }

    // Add proxy configuration if provided
    if let Some(proxy) = &options.proxy {
        config.proxy_mcp_servers.push(proxy.clone());
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

/// Register devboy as MCP server in Claude Code.
async fn register_claude_mcp() -> Result<()> {
    println!("Registering devboy MCP server in Claude Code...");

    // Try using claude CLI first
    let claude_result = Command::new("claude")
        .args(["mcp", "add", "devboy", "--", "devboy", "mcp"])
        .status();

    match claude_result {
        Ok(status) if status.success() => {
            println!("Successfully registered via Claude CLI");
            return Ok(());
        }
        _ => {
            // Fall back to editing ~/.claude.json directly
            register_claude_mcp_direct()?;
        }
    }

    Ok(())
}

/// Register devboy MCP server by editing ~/.claude.json directly.
fn register_claude_mcp_direct() -> Result<()> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let claude_config_path = home.join(".claude.json");

    let mut claude_config: serde_json::Value = if claude_config_path.exists() {
        let content = std::fs::read_to_string(&claude_config_path)
            .context("Failed to read ~/.claude.json")?;
        let parsed: serde_json::Value =
            serde_json::from_str(&content).context("Failed to parse ~/.claude.json")?;

        // Ensure root is an object
        if !parsed.is_object() {
            anyhow::bail!(
                "~/.claude.json exists but is not a JSON object. \
                 Please fix it manually or delete it."
            );
        }
        parsed
    } else {
        serde_json::json!({})
    };

    // Ensure mcpServers is an object (or create it)
    match claude_config.get("mcpServers") {
        Some(servers) if !servers.is_object() => {
            anyhow::bail!(
                "~/.claude.json has 'mcpServers' but it's not an object. \
                 Please fix it manually."
            );
        }
        None => {
            claude_config["mcpServers"] = serde_json::json!({});
        }
        _ => {}
    }

    // Add devboy server
    claude_config["mcpServers"]["devboy"] = serde_json::json!({
        "command": "devboy",
        "args": ["mcp"]
    });

    // Write back
    let content = serde_json::to_string_pretty(&claude_config)
        .context("Failed to serialize claude config")?;
    std::fs::write(&claude_config_path, content).context("Failed to write ~/.claude.json")?;

    println!("Successfully registered in ~/.claude.json");
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

    // Apply built-in tools filtering config.
    if !config.builtin_tools.is_empty() {
        config.builtin_tools.warn_unknown_tools(KNOWN_BUILTIN_TOOLS);
        server
            .set_builtin_tools_config(config.builtin_tools.clone())
            .context("Invalid builtin_tools configuration")?;
    }

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
                transport,
                token_key.clone(),
                token.clone(),
                auth_type.clone(),
                *force,
            );
        }
        ProxyCommands::Remove { name } => {
            return handle_proxy_remove(name);
        }
        _ => {}
    }

    let (config, _) = load_runtime_config()?;
    let store = KeychainStore::new();

    if config.proxy_mcp_servers.is_empty() {
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

    let mut proxy_manager = build_proxy_manager(&config, &store).await;

    if proxy_manager.is_empty() {
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
    }

    Ok(())
}

/// Add a new proxy server configuration.
fn handle_proxy_add(
    name: &str,
    url: &str,
    transport: &str,
    token_key: Option<String>,
    token: Option<String>,
    auth_type: Option<String>,
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
    let final_auth_type = auth_type.unwrap_or_else(|| {
        if final_token_key.is_some() {
            "bearer".to_string()
        } else {
            "none".to_string()
        }
    });

    // Add new proxy
    let proxy = ProxyMcpServerConfig {
        name: name.to_string(),
        url: url.to_string(),
        auth_type: final_auth_type.clone(),
        token_key: final_token_key.clone(),
        tool_prefix: None,
        transport: transport.to_string(),
    };

    config.proxy_mcp_servers.push(proxy);
    config
        .save_to(&config_path)
        .context("Failed to save config")?;

    // Store token in keychain if provided
    if let Some(token_value) = token {
        let key = final_token_key.as_ref().unwrap();
        let store = KeychainStore::new();
        store
            .store(key, &token_value)
            .with_context(|| format!("Failed to store token in keychain as '{}'", key))?;
        println!("Stored token in keychain as '{}'", key);
    }

    println!("Added proxy '{}' -> {}", name, url);
    println!("  transport: {}", transport);
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
    }
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
    let store = KeychainStore::new();

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
        add_context_providers(&mut server, &store, context_name, context);
    }
    if !config.contexts.contains_key(Config::DEFAULT_CONTEXT_NAME) {
        if let Some(default_context) = config.legacy_default_context() {
            add_context_providers(
                &mut server,
                &store,
                Config::DEFAULT_CONTEXT_NAME,
                &default_context,
            );
        }
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

    if let Some(result) = resp.result {
        let json = serde_json::to_string_pretty(&result)?;
        println!("{}", json);
    }

    Ok(())
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
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("whitelist (enabled) mode is active"));
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
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("whitelist (enabled) mode is active"));
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
}
