//! Handlers for the `devboy skills {list,show,install,upgrade,remove}`
//! CLI subcommand family. The real logic lives in `devboy_skills`; this
//! module only translates between CLI flags and the crate's API and
//! handles user-facing output.

use anyhow::{Context, Result};
use devboy_skills::{
    Agent, Category, EmbeddedSkillSource, Environment, InstallOptions, InstallOutcome,
    InstallReport, InstallSpec, Manifest, SkillSource, detect_installed_agents,
    install_skills_to_target, remove_skills_from_target, resolve_targets,
};

use crate::SkillsCommands;

/// Dispatch a `devboy skills` subcommand.
pub async fn handle(command: SkillsCommands) -> Result<()> {
    match command {
        SkillsCommands::List { category } => handle_list(category).await,
        SkillsCommands::Show { name } => handle_show(&name).await,
        SkillsCommands::Install {
            names,
            all,
            category,
            global,
            local,
            agents,
            force,
            dry_run,
        } => handle_install(names, all, category, global, local, agents, force, dry_run).await,
        SkillsCommands::Upgrade {
            names,
            global,
            local,
            agents,
            force,
            dry_run,
        } => handle_upgrade(names, global, local, agents, force, dry_run).await,
        SkillsCommands::Remove {
            names,
            global,
            local,
            agents,
            strict,
            dry_run,
        } => handle_remove(names, global, local, agents, strict, dry_run).await,
    }
}

// ---------------------------------------------------------------------------
// list / show
// ---------------------------------------------------------------------------

async fn handle_list(category_filter: Option<String>) -> Result<()> {
    let source = EmbeddedSkillSource::new();
    let summaries = source
        .list()
        .await
        .context("failed to enumerate embedded skills")?;

    let filtered: Vec<_> = match category_filter.as_deref() {
        None => summaries,
        Some(raw) => {
            let cat = Category::parse(raw).with_context(|| {
                format!(
                    "unknown category `{raw}` (expected one of: self-bootstrap, issue-tracking, code-review, self-feedback, meeting-notes, messenger)"
                )
            })?;
            summaries
                .into_iter()
                .filter(|s| s.category == cat)
                .collect()
        }
    };

    if filtered.is_empty() {
        println!("no skills found");
        return Ok(());
    }

    let mut current_category: Option<Category> = None;
    for s in &filtered {
        if Some(s.category) != current_category {
            current_category = Some(s.category);
            println!("\n[{}]", s.category);
        }
        println!("  {:<32} v{:<3} {}", s.name, s.version, s.description);
    }
    println!();
    Ok(())
}

async fn handle_show(name: &str) -> Result<()> {
    let source = EmbeddedSkillSource::new();
    let skill = source
        .load(name)
        .await
        .with_context(|| format!("failed to load skill `{name}`"))?;
    let yaml =
        serde_yaml::to_string(&skill.frontmatter).context("failed to serialise frontmatter")?;
    println!("---");
    print!("{yaml}");
    if !yaml.ends_with('\n') {
        println!();
    }
    println!("---");
    print!("{}", skill.body);
    if !skill.body.ends_with('\n') {
        println!();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// install / upgrade
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn handle_install(
    names: Vec<String>,
    all: bool,
    category: Option<String>,
    global: bool,
    local: bool,
    agent_flags: Vec<String>,
    force: bool,
    dry_run: bool,
) -> Result<()> {
    let skills_to_install = select_skills(&names, all, category.as_deref()).await?;
    if skills_to_install.is_empty() {
        println!("no skills matched the selection");
        return Ok(());
    }

    let env = Environment::detect().context("failed to detect environment")?;
    let spec = build_install_spec(global, local, &agent_flags, &env)?;
    let targets = resolve_targets(&env, &spec).map_err(format_target_error)?;

    let history = devboy_skills::HistoricalHashes::load_embedded()
        .context("failed to load embedded historical hashes")?;
    let options = InstallOptions {
        force,
        dry_run,
        installed_from: Some(format!("devboy-tools {}", env!("CARGO_PKG_VERSION"))),
    };

    let mut any_written = false;
    let mut any_failed = false;

    for target in &targets {
        println!("-> {}", target.label);
        match install_skills_to_target(target, &skills_to_install, &history, &options) {
            Ok(report) => {
                print_report(&report);
                if report.outcomes.values().any(|o| {
                    matches!(
                        o,
                        InstallOutcome::Installed
                            | InstallOutcome::Upgraded { .. }
                            | InstallOutcome::OverwrittenWithForce
                    )
                }) {
                    any_written = true;
                }
            }
            Err(e) => {
                eprintln!("   error: {e}");
                any_failed = true;
            }
        }
    }

    if any_failed && !any_written {
        anyhow::bail!("every install target failed; see errors above");
    }
    if dry_run {
        println!("\n(dry-run) no filesystem changes were made");
    }
    Ok(())
}

async fn handle_upgrade(
    names: Vec<String>,
    global: bool,
    local: bool,
    agent_flags: Vec<String>,
    force: bool,
    dry_run: bool,
) -> Result<()> {
    let env = Environment::detect().context("failed to detect environment")?;
    let spec = build_install_spec(global, local, &agent_flags, &env)?;
    let targets = resolve_targets(&env, &spec).map_err(format_target_error)?;

    let source = EmbeddedSkillSource::new();
    let history = devboy_skills::HistoricalHashes::load_embedded()
        .context("failed to load embedded historical hashes")?;
    let options = InstallOptions {
        force,
        dry_run,
        installed_from: Some(format!("devboy-tools {}", env!("CARGO_PKG_VERSION"))),
    };

    for target in &targets {
        println!("-> {}", target.label);
        let manifest_path = target.skills_dir.join(devboy_skills::MANIFEST_FILE);
        let manifest = Manifest::load(&manifest_path).unwrap_or_default();
        let candidates: Vec<String> = if names.is_empty() {
            manifest.skills.keys().cloned().collect()
        } else {
            names.clone()
        };
        if candidates.is_empty() {
            println!("   (no installed skills)");
            continue;
        }
        let mut skills = Vec::new();
        for n in &candidates {
            match source.load(n).await {
                Ok(s) => skills.push(s),
                Err(e) => eprintln!("   {n}: skipped ({e})"),
            }
        }
        match install_skills_to_target(target, &skills, &history, &options) {
            Ok(report) => print_report(&report),
            Err(e) => eprintln!("   error: {e}"),
        }
    }
    if dry_run {
        println!("\n(dry-run) no filesystem changes were made");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// remove
// ---------------------------------------------------------------------------

async fn handle_remove(
    names: Vec<String>,
    global: bool,
    local: bool,
    agent_flags: Vec<String>,
    strict: bool,
    dry_run: bool,
) -> Result<()> {
    let env = Environment::detect().context("failed to detect environment")?;
    let spec = build_install_spec(global, local, &agent_flags, &env)?;
    let targets = resolve_targets(&env, &spec).map_err(format_target_error)?;

    for target in &targets {
        println!("-> {}", target.label);
        match remove_skills_from_target(target, &names, strict, dry_run) {
            Ok(removed) => {
                if removed.is_empty() {
                    println!("   (nothing matched)");
                } else {
                    for n in removed {
                        println!("   removed {n}");
                    }
                }
            }
            Err(e) => eprintln!("   error: {e}"),
        }
    }
    if dry_run {
        println!("\n(dry-run) no filesystem changes were made");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

async fn select_skills(
    names: &[String],
    all: bool,
    category_filter: Option<&str>,
) -> Result<Vec<devboy_skills::Skill>> {
    let source = EmbeddedSkillSource::new();
    if all {
        let summaries = source.list().await?;
        let mut out = Vec::with_capacity(summaries.len());
        for s in summaries {
            out.push(source.load(&s.name).await?);
        }
        return Ok(out);
    }
    if let Some(raw) = category_filter {
        let cat = Category::parse(raw).with_context(|| format!("unknown category `{raw}`"))?;
        let summaries = source.list().await?;
        let mut out = Vec::new();
        for s in summaries.into_iter().filter(|s| s.category == cat) {
            out.push(source.load(&s.name).await?);
        }
        return Ok(out);
    }
    if names.is_empty() {
        anyhow::bail!(
            "no skills specified. Pass one or more names, `--all`, or `--category <id>`."
        );
    }
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        out.push(source.load(name).await?);
    }
    Ok(out)
}

fn build_install_spec(
    global: bool,
    local: bool,
    agent_flags: &[String],
    env: &Environment,
) -> Result<InstallSpec> {
    let mut spec = InstallSpec {
        global,
        local,
        ..Default::default()
    };
    let mut include_vendor_neutral = false;
    for raw in agent_flags {
        if raw == "all" {
            for a in detect_installed_agents(&env.home) {
                if !spec.agents.contains(&a) {
                    spec.agents.push(a);
                }
            }
            include_vendor_neutral = true;
        } else if let Some(a) = Agent::parse(raw) {
            if !spec.agents.contains(&a) {
                spec.agents.push(a);
            }
        } else {
            anyhow::bail!(
                "unknown agent `{raw}` (expected one of: claude, codex, cursor, kimi, all)"
            );
        }
    }
    spec.include_vendor_neutral_global = include_vendor_neutral;
    Ok(spec)
}

fn format_target_error(err: devboy_skills::SkillError) -> anyhow::Error {
    if matches!(err, devboy_skills::SkillError::MissingRequiredField { .. }) {
        anyhow::anyhow!(
            "no git repository / .devboy.toml at the current path\n\n\
             skills are installed repo-locally by default. choose one:\n  \
             devboy skills install <name> --global           # install to ~/.agents/skills/\n  \
             devboy skills install <name> --agent claude     # install to ~/.claude/skills/\n  \
             devboy skills install <name> --agent all        # every detected agent\n  \
             cd <your-project> && devboy skills install <name>"
        )
    } else {
        anyhow::Error::new(err)
    }
}

fn print_report(report: &InstallReport) {
    if report.outcomes.is_empty() {
        println!("   (no skills applied)");
        return;
    }
    let mut installed = 0usize;
    let mut unchanged = 0usize;
    let mut upgraded = 0usize;
    let mut skipped_user = 0usize;
    let mut overwritten = 0usize;
    let mut skipped_unknown = 0usize;
    for (name, outcome) in &report.outcomes {
        match outcome {
            InstallOutcome::Installed => {
                installed += 1;
                println!("   installed {name}");
            }
            InstallOutcome::Unchanged => {
                unchanged += 1;
                println!("   unchanged  {name}");
            }
            InstallOutcome::Upgraded { from_version } => {
                upgraded += 1;
                match from_version {
                    Some(v) => println!("   upgraded  {name} (from v{v})"),
                    None => println!("   upgraded  {name}"),
                }
            }
            InstallOutcome::SkippedUserModified => {
                skipped_user += 1;
                println!("   skipped   {name} (user-modified — pass --force to overwrite)");
            }
            InstallOutcome::OverwrittenWithForce => {
                overwritten += 1;
                println!("   forced    {name} (user-modified, overwritten)");
            }
            InstallOutcome::SkippedUnknown => {
                skipped_unknown += 1;
                println!("   skipped   {name} (unknown history — pass --force to overwrite)");
            }
        }
    }
    let _ = (
        installed,
        unchanged,
        upgraded,
        skipped_user,
        overwritten,
        skipped_unknown,
    );
}
