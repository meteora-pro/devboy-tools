//! DevBoy CLI - Command-line interface for devboy-tools.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use devboy_core::{Config, IssueFilter, IssueProvider, MrFilter, MergeRequestProvider, Provider};
use devboy_github::GitHubClient;
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
    /// Start the MCP server
    Serve {
        /// Port to listen on
        #[arg(short, long, default_value = "3000")]
        port: u16,
    },

    /// Configuration management
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
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
        /// Provider to test (github, gitlab)
        provider: String,
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
        Some(Commands::Serve { port }) => {
            tracing::info!("Starting MCP server on port {}", port);
            // TODO: Implement MCP server
            println!("MCP server not yet implemented");
        }

        Some(Commands::Config { command }) => {
            handle_config_command(command)?;
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
            config.set(&key, &value).context("Failed to set config value")?;
            config.save().context("Failed to save config")?;
            println!("Set {} = {}", key, value);
        }

        ConfigCommands::SetSecret { key, value } => {
            let store = KeychainStore::new();
            store.store(&key, &value).context("Failed to store secret")?;
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

        ConfigCommands::Path => {
            match Config::config_path() {
                Ok(path) => println!("{}", path.display()),
                Err(e) => println!("Error: {}", e),
            }
        }
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

        let issues = client.get_issues(filter).await.context("Failed to fetch issues")?;

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

        let prs = client.get_merge_requests(filter).await.context("Failed to fetch PRs")?;

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
                .context("GitHub token not set. Run: devboy config set-secret github.token <token>")?;

            println!("Testing GitHub connection...");
            println!("  Repository: {}/{}", gh.owner, gh.repo);

            let client = GitHubClient::new(&gh.owner, &gh.repo, token);

            // Test by getting current user
            match client.get_current_user().await {
                Ok(user) => {
                    println!("  Authenticated as: {} ({})", user.username, user.name.unwrap_or_default());
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

            let _token = store
                .get("gitlab.token")
                .context("Failed to get token")?
                .context("GitLab token not set. Run: devboy config set-secret gitlab.token <token>")?;

            println!("Testing GitLab connection...");
            println!("  URL: {}", gl.url);
            println!("  Project: {}", gl.project_id);

            // TODO: Implement GitLab test
            println!();
            println!("GitLab test not yet implemented");
        }

        _ => {
            println!("Unknown provider: {}", provider);
            println!("Supported providers: github, gitlab");
        }
    }

    Ok(())
}
