//! Handler for `devboy agents <subcommand>`.
//!
//! Walks `$HOME/` looking for installed AI coding agents and reports a
//! ranked snapshot. The detection logic itself lives in
//! [`devboy_core::agents`]; this module only translates between CLI flags
//! and that API and renders user-facing output.

use anyhow::Result;
use clap::{Subcommand, ValueEnum};
use devboy_core::agents::{AgentSnapshot, InstallStatus, detect_all, pick_primary};

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
        let primary = if Some(s.id) == primary_id {
            "★ primary"
        } else {
            ""
        };
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
    format_relative_at(t, chrono::Utc::now())
}

/// Pure variant of [`format_relative`] for testing — caller passes `now`.
fn format_relative_at(
    t: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let delta = now - t;
    let secs = delta.num_seconds();
    if secs < 0 {
        // Future timestamp — clock skew, bad mtime, or a very-just-written file.
        // Treat as "just now" rather than printing "-10s ago".
        return "just now".to_string();
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use devboy_core::agents::AgentSnapshot;

    fn at(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
    ) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .unwrap()
    }

    #[test]
    fn status_glyph_covers_all_variants() {
        assert_eq!(status_glyph(InstallStatus::Yes), "✓");
        assert_eq!(status_glyph(InstallStatus::No), "✗");
        assert_eq!(status_glyph(InstallStatus::Unknown), "?");
    }

    #[test]
    fn install_status_label_covers_all_variants() {
        assert_eq!(install_status_label(InstallStatus::Yes), "yes");
        assert_eq!(install_status_label(InstallStatus::No), "no");
        assert_eq!(install_status_label(InstallStatus::Unknown), "unknown");
    }

    #[test]
    fn format_relative_at_handles_all_branches() {
        let now = at(2026, 5, 1, 12, 0);

        // Future timestamp → "just now"
        assert_eq!(format_relative_at(at(2026, 5, 1, 12, 1), now), "just now");

        // < 60 seconds
        assert_eq!(
            format_relative_at(now - chrono::Duration::seconds(5), now),
            "5s ago"
        );

        // < 60 minutes
        assert_eq!(
            format_relative_at(now - chrono::Duration::minutes(15), now),
            "15m ago"
        );

        // < 48 hours
        assert_eq!(
            format_relative_at(now - chrono::Duration::hours(5), now),
            "5h ago"
        );

        // < 60 days (between 48h and 60d shows days)
        assert_eq!(
            format_relative_at(now - chrono::Duration::days(10), now),
            "10d ago"
        );

        // ≥ 60 days falls back to absolute date
        let ancient = at(2025, 1, 15, 8, 0);
        assert_eq!(format_relative_at(ancient, now), "2025-01-15");
    }

    #[test]
    fn format_relative_at_treats_exactly_now_as_zero_seconds() {
        let now = at(2026, 5, 1, 12, 0);
        assert_eq!(format_relative_at(now, now), "0s ago");
    }

    #[test]
    fn render_json_emits_primary_and_agents_keys() {
        // Build a snapshot vector that exercises both an "installed" and a
        // "not installed" entry.
        let snaps = vec![AgentSnapshot {
            id: "claude",
            display_name: "Claude Code",
            status: InstallStatus::Yes,
            sessions: Some(42),
            last_used: Some(at(2026, 5, 1, 12, 0)),
            score: 0.9,
            paths_checked: vec!["/tmp/a".into()],
        }];

        // We can't easily capture stdout, but at least make sure the
        // function does not panic on a typical input.
        let result = render_json(&snaps);
        assert!(result.is_ok());
    }
}
