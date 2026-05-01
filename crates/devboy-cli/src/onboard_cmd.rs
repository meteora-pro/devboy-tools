//! Handler for `devboy onboard`.
//!
//! Detects which AI agent the user actively uses (`devboy_core::agents`),
//! resolves a curated skill bundle for the requested profile
//! (`skills/bundles/<profile>.toml`), and installs the resulting skill
//! set to the agent's canonical directory.
//!
//! Four of the seven detector ids currently map onto a `devboy_skills::Agent`
//! install target (Claude, Codex, Cursor, Kimi). For the other three
//! (Copilot, Gemini, Antigravity) we print the plan and skip — install
//! support lands in follow-up work.

use anyhow::{Context, Result, anyhow};
use clap::Args;
use devboy_core::agents::{AgentSnapshot, InstallStatus, bundles, detect_all, pick_primary};
use devboy_skills::{
    Agent, EmbeddedSkillSource, Environment, HistoricalHashes, InstallOptions, InstallOutcome,
    InstallSpec, SkillSource, install_skills_to_target, resolve_targets,
};
use std::io::{self, IsTerminal, Write};

#[derive(Debug, Args)]
pub struct OnboardArgs {
    /// Override the auto-detected primary agent (e.g. `--agent claude`)
    #[arg(long)]
    pub agent: Option<String>,

    /// Skill bundle profile to install (default: dev)
    #[arg(long, default_value = "dev")]
    pub profile: String,

    /// Skip confirmation — install without asking. Required in non-TTY.
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Show the plan without writing anything.
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn handle(args: OnboardArgs) -> Result<()> {
    let snapshots = detect_all();

    let chosen = pick_agent(&args, &snapshots)?;
    let bundle = bundles::load(&args.profile)?;

    println!();
    println!("Detected agents:");
    print_agent_table(&snapshots, chosen.id);
    println!();
    println!(
        "→ Setting up for {} with profile '{}' ({} skills):",
        chosen.display_name,
        bundle.name,
        bundle.skills.len()
    );
    for s in &bundle.skills {
        println!("    • {s}");
    }
    println!();

    if !confirm(&args)? {
        println!("Cancelled.");
        return Ok(());
    }

    let skills_agent = match chosen.id {
        "claude" => Some(Agent::Claude),
        "codex" => Some(Agent::Codex),
        "cursor" => Some(Agent::Cursor),
        "kimi" => Some(Agent::Kimi),
        _ => None,
    };

    let Some(agent) = skills_agent else {
        println!(
            "⚠ install target for '{}' is not implemented yet.",
            chosen.id
        );
        println!("  Bundle plan recorded above; manual install will land in a follow-up.");
        return Ok(());
    };

    install_bundle(agent, &bundle.skills, args.dry_run).await
}

fn pick_agent<'a>(args: &OnboardArgs, snapshots: &'a [AgentSnapshot]) -> Result<&'a AgentSnapshot> {
    if let Some(name) = &args.agent {
        return snapshots
            .iter()
            .find(|s| s.id == name)
            .ok_or_else(|| anyhow!("no agent named '{name}'. Try `devboy agents list`."));
    }

    if let Some(primary) = pick_primary(snapshots) {
        return Ok(primary);
    }

    // No clear primary. Look at what's installed before deciding what to say.
    let mut installed: Vec<&AgentSnapshot> = snapshots
        .iter()
        .filter(|s| s.status == InstallStatus::Yes)
        .collect();
    installed.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if installed.is_empty() {
        return Err(anyhow!(
            "no supported agents detected on this machine.\n  \
             Install Claude Code / Copilot CLI / Codex / Cursor / Kimi / Gemini and re-run, \
             or pass `--agent <id>` to override detection."
        ));
    }

    let top: Vec<&str> = installed.iter().take(3).map(|s| s.id).collect();
    Err(anyhow!(
        "could not pick a primary agent automatically — top candidates: {}.\n  \
         Re-run with `--agent <id>` to choose one (e.g. `devboy onboard --agent {}`).",
        top.join(", "),
        top.first().copied().unwrap_or("claude"),
    ))
}

fn print_agent_table(snapshots: &[AgentSnapshot], chosen_id: &str) {
    for s in snapshots {
        let glyph = match s.status {
            InstallStatus::Yes => "✓",
            InstallStatus::No => "✗",
            InstallStatus::Unknown => "?",
        };
        let marker = if s.id == chosen_id { " ← chosen" } else { "" };
        let sessions = s
            .sessions
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".into());
        println!(
            "  {} {:<14} {:<22} sessions={:>5}  score={:.3}{}",
            glyph, s.id, s.display_name, sessions, s.score, marker
        );
    }
}

fn confirm(args: &OnboardArgs) -> Result<bool> {
    if args.yes || args.dry_run {
        return Ok(true);
    }
    if !io::stdin().is_terminal() {
        return Err(anyhow!(
            "non-interactive shell — re-run with --yes or --dry-run"
        ));
    }
    print!("Continue? [Y/n] ");
    io::stdout().flush().ok();
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    let answer = buf.trim().to_lowercase();
    Ok(answer.is_empty() || answer == "y" || answer == "yes")
}

async fn install_bundle(agent: Agent, skill_names: &[String], dry_run: bool) -> Result<()> {
    let env = Environment::detect().context("failed to detect environment")?;
    let spec = InstallSpec {
        agents: vec![agent],
        ..Default::default()
    };
    let targets = resolve_targets(&env, &spec).context("failed to resolve install targets")?;

    let source = EmbeddedSkillSource::new();
    let mut skills = Vec::with_capacity(skill_names.len());
    let mut missing = Vec::new();
    for name in skill_names {
        match source.load(name).await {
            Ok(s) => skills.push(s),
            Err(_) => missing.push(name.clone()),
        }
    }
    if !missing.is_empty() {
        println!(
            "⚠ {} skill(s) not in catalogue and will be skipped:",
            missing.len()
        );
        for m in &missing {
            println!("    • {m}");
        }
    }
    if skills.is_empty() {
        println!("Nothing to install.");
        return Ok(());
    }

    let history =
        HistoricalHashes::load_embedded().context("failed to load embedded skill history")?;

    let options = InstallOptions {
        force: false,
        dry_run,
        installed_from: Some(env!("CARGO_PKG_VERSION").to_string()),
    };

    println!();
    for target in &targets {
        println!("Installing {} skills to: {}", skills.len(), target.label);
        let report = install_skills_to_target(target, &skills, &history, &options)
            .with_context(|| format!("install failed for target {}", target.label))?;

        let mut installed = 0;
        let mut unchanged = 0;
        let mut upgraded = 0;
        let mut skipped = 0;
        for outcome in report.outcomes.values() {
            match outcome {
                InstallOutcome::Installed => installed += 1,
                InstallOutcome::Unchanged => unchanged += 1,
                InstallOutcome::Upgraded { .. } => upgraded += 1,
                InstallOutcome::SkippedUserModified | InstallOutcome::SkippedUnknown => {
                    skipped += 1
                }
                InstallOutcome::OverwrittenWithForce => installed += 1,
            }
        }
        println!(
            "  ✓ installed={installed} unchanged={unchanged} upgraded={upgraded} skipped={skipped}{}",
            if dry_run { "  (dry-run)" } else { "" }
        );
    }

    if !dry_run {
        println!();
        println!("Done. Run `devboy skills list` to verify.");
    } else {
        println!();
        println!("Dry-run complete. Re-run without --dry-run to apply.");
    }
    Ok(())
}
