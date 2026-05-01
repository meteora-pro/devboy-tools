//! Handler for `devboy agents <subcommand>`.
//!
//! Walks `$HOME/` looking for installed AI coding agents and reports a
//! ranked snapshot. The detection logic itself lives in
//! [`devboy_core::agents`]; this module only translates between CLI flags
//! and that API and renders user-facing output.

use anyhow::Result;
use clap::{Subcommand, ValueEnum};
use devboy_core::agents::{detect_all, pick_primary, AgentSnapshot, InstallStatus};

#[derive(Subcommand)]
pub enum AgentsCommands {
    /// List detected AI coding agents with status, session count, and last-used time.
    List {
        /// Output format
        #[arg(long, value_enum, default_value_t = AgentsListFormat::Table)]
        format: AgentsListFormat,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum AgentsListFormat {
    Table,
    Json,
}

pub fn handle(command: AgentsCommands) -> Result<()> {
    match command {
        AgentsCommands::List { format } => {
            let snapshots = detect_all();
            match format {
                AgentsListFormat::Table => render_table(&snapshots),
                AgentsListFormat::Json => render_json(&snapshots)?,
            }
        }
    }
    Ok(())
}

fn render_json(snapshots: &[AgentSnapshot]) -> Result<()> {
    let primary_id = pick_primary(snapshots).map(|s| s.id);
    let payload = serde_json::json!({
        "primary": primary_id,
        "agents": snapshots,
    });
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

fn render_table(snapshots: &[AgentSnapshot]) {
    let primary_id = pick_primary(snapshots).map(|s| s.id);
    println!(
        "{:<14} {:<22} {:<10} {:>10} {:<28} {:>6}  {:<8}",
        "id", "name", "status", "sessions", "last_used", "score", "primary"
    );
    println!("{}", "-".repeat(108));
    for s in snapshots {
        let status_glyph = status_glyph(s.status);
        let sessions = match s.sessions {
            Some(n) => n.to_string(),
            None => "-".to_string(),
        };
        let last_used = match s.last_used {
            Some(t) => format_relative(t),
            None => "-".to_string(),
        };
        let primary = if Some(s.id) == primary_id { "★ primary" } else { "" };
        println!(
            "{:<14} {:<22} {} {:<8} {:>10} {:<28} {:>6.3}  {}",
            s.id,
            s.display_name,
            status_glyph,
            install_status_label(s.status),
            sessions,
            last_used,
            s.score,
            primary,
        );
    }
}

fn status_glyph(status: InstallStatus) -> &'static str {
    match status {
        InstallStatus::Yes => "✓",
        InstallStatus::No => "✗",
        InstallStatus::Unknown => "?",
    }
}

fn install_status_label(status: InstallStatus) -> &'static str {
    match status {
        InstallStatus::Yes => "yes",
        InstallStatus::No => "no",
        InstallStatus::Unknown => "unknown",
    }
}

fn format_relative(t: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let delta = now - t;
    let secs = delta.num_seconds();
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = delta.num_minutes();
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = delta.num_hours();
    if hours < 48 {
        return format!("{hours}h ago");
    }
    let days = delta.num_days();
    if days < 60 {
        return format!("{days}d ago");
    }
    t.format("%Y-%m-%d").to_string()
}
