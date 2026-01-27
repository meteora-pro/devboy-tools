//! DevBoy CLI - Command-line interface for devboy-tools.

use clap::{Parser, Subcommand};
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

    /// Configure providers
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },

    /// Get information about issues
    Issues {
        /// Filter by state
        #[arg(short, long, default_value = "open")]
        state: String,
    },

    /// Get information about merge requests
    Mrs {
        /// Filter by state
        #[arg(short, long, default_value = "open")]
        state: String,
    },
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Configure GitLab provider
    Gitlab {
        /// GitLab base URL
        #[arg(long)]
        url: Option<String>,

        /// GitLab project ID
        #[arg(long)]
        project: Option<String>,

        /// GitLab access token
        #[arg(long)]
        token: Option<String>,
    },

    /// Configure GitHub provider
    Github {
        /// Repository owner
        #[arg(long)]
        owner: Option<String>,

        /// Repository name
        #[arg(long)]
        repo: Option<String>,

        /// GitHub access token
        #[arg(long)]
        token: Option<String>,
    },

    /// Show current configuration
    Show,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
            // TODO: Implement server
        }
        Some(Commands::Config { command }) => match command {
            ConfigCommands::Gitlab {
                url,
                project,
                token,
            } => {
                tracing::info!("Configuring GitLab: url={:?}, project={:?}", url, project);
                // TODO: Store configuration
                if token.is_some() {
                    tracing::info!("Token provided (hidden)");
                }
            }
            ConfigCommands::Github { owner, repo, token } => {
                tracing::info!("Configuring GitHub: owner={:?}, repo={:?}", owner, repo);
                // TODO: Store configuration
                if token.is_some() {
                    tracing::info!("Token provided (hidden)");
                }
            }
            ConfigCommands::Show => {
                tracing::info!("Current configuration:");
                // TODO: Show configuration
            }
        },
        Some(Commands::Issues { state }) => {
            tracing::info!("Fetching issues with state: {}", state);
            // TODO: Fetch and display issues
        }
        Some(Commands::Mrs { state }) => {
            tracing::info!("Fetching merge requests with state: {}", state);
            // TODO: Fetch and display MRs
        }
        None => {
            println!("DevBoy - AI-powered development tools");
            println!("Run with --help for usage information");
        }
    }

    Ok(())
}
