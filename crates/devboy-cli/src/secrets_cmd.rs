//! Handlers for the `devboy secrets {list,describe}` subcommand
//! family.
//!
//! Implements the user-facing surface from [ADR-020] §4 (manifest
//! discovery) and §3 (global index metadata): the user can see every
//! path the active project depends on plus its rendered metadata
//! card, without ever revealing a secret value.
//!
//! ## What this module does **not** do
//!
//! - **Read values.** `secrets list` and `describe` are metadata-only
//!   commands. Value resolution lives in the source router (epic
//!   phase P5) and is exposed exclusively through high-level provider
//!   tools per ADR-023 §3.7.
//! - **Apply rotation reminders.** `doctor` (epic phase P7.3 / P9.3)
//!   produces those, not this module.
//!
//! [ADR-020]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use devboy_storage::{
    Gate, GlobalIndex, IndexEntry, MergeOutput, OverrideField, ProjectManifest, ResolvedSecret,
    SecretOrigin, SecretPath, merge_manifest,
};
use serde::Serialize;

use crate::secrets_agent;
use crate::secrets_agent_service::{self, ServiceOptions, UserServiceLayout};

/// `devboy secrets <subcommand>` subcommand family.
#[derive(Subcommand, Debug)]
pub enum SecretsCommands {
    /// List every path the active project's manifest declares,
    /// merged with the global index. Values are never shown.
    List(ListArgs),
    /// Print the resolved metadata card for a single secret path.
    Describe(DescribeArgs),
    /// Manage the local secret-store agent daemon (ADR-023 §3.3).
    Agent {
        /// What to do with the agent.
        #[command(subcommand)]
        command: AgentCommands,
    },
}

/// `devboy secrets agent <subcommand>` family.
#[derive(Subcommand, Debug)]
pub enum AgentCommands {
    /// Report the agent socket path and whether the daemon is
    /// currently accepting connections.
    Status,
    /// Spawn the agent if it isn't already running. Idempotent —
    /// no-op when the socket is already live.
    Start(AgentStartArgs),
    /// Install a per-user service unit so the daemon starts at
    /// login and respawns on failure. macOS writes a launchd plist
    /// at `~/Library/LaunchAgents/dev.devboy.secrets.plist`; Linux
    /// writes a systemd-user unit at
    /// `~/.config/systemd/user/devboy-secrets-agent.service`. After
    /// install: verify with `launchctl print gui/$(id -u)/dev.devboy.secrets`
    /// (macOS) or `systemctl --user status devboy-secrets-agent.service`
    /// (Linux).
    Install(AgentInstallArgs),
    /// Stop the user service (if loaded) and remove the unit file
    /// written by `install`. Idempotent — running it twice is fine.
    Uninstall(AgentUninstallArgs),
}

/// Flags for `devboy secrets agent start`.
#[derive(Args, Debug, Default)]
pub struct AgentStartArgs {
    /// Override the vault file the daemon will operate on. Defaults
    /// to `<config_dir>/devboy-tools/secrets/vault.dvb`.
    #[arg(long)]
    pub vault: Option<PathBuf>,
    /// Cap on the wait-for-socket loop, in seconds. Defaults to
    /// [`secrets_agent::DEFAULT_SPAWN_TIMEOUT`].
    #[arg(long)]
    pub timeout_secs: Option<u64>,
}

/// Flags for `devboy secrets agent install`.
#[derive(Args, Debug, Default)]
pub struct AgentInstallArgs {
    /// Override the path to the `devboy-secrets-agent` binary. By
    /// default the same lookup as `secrets agent start` is used
    /// (env override → sibling-of-current_exe → `PATH`).
    #[arg(long)]
    pub binary: Option<PathBuf>,
    /// Override the daemon's socket path (`DEVBOY_AGENT_SOCKET`).
    #[arg(long)]
    pub socket: Option<PathBuf>,
    /// Override the daemon's vault path (`DEVBOY_VAULT_PATH`).
    #[arg(long)]
    pub vault: Option<PathBuf>,
    /// Skip the platform service-manager activation step (just
    /// write the unit file). Useful for previewing what would land
    /// on disk; the unit is loaded next time `launchctl/systemctl`
    /// scans its directory anyway.
    #[arg(long, default_value_t = false)]
    pub no_load: bool,
}

/// Flags for `devboy secrets agent uninstall`.
#[derive(Args, Debug, Default)]
pub struct AgentUninstallArgs {
    /// Skip the platform service-manager teardown step (just
    /// remove the unit file). The next reboot will pick up the
    /// removal anyway.
    #[arg(long, default_value_t = false)]
    pub no_unload: bool,
}

/// Flags for `devboy secrets list`.
#[derive(Args, Debug, Default)]
pub struct ListArgs {
    /// Include framework-internal paths (`__*`) in the output.
    /// Hidden by default per ADR-021 §5.
    #[arg(long)]
    pub internal: bool,
    /// Print as JSON instead of a human-readable table.
    #[arg(long)]
    pub json: bool,
}

/// Flags for `devboy secrets describe <path>`.
#[derive(Args, Debug)]
pub struct DescribeArgs {
    /// The secret path (e.g. `team/gitlab/token-deploy`).
    pub path: String,
    /// Print as JSON instead of a human-readable card.
    #[arg(long)]
    pub json: bool,
}

/// Dispatch a `devboy secrets` subcommand.
pub async fn handle(command: SecretsCommands) -> Result<()> {
    match command {
        SecretsCommands::List(args) => list(args),
        SecretsCommands::Describe(args) => describe(args),
        SecretsCommands::Agent { command } => match command {
            AgentCommands::Status => agent_status().await,
            AgentCommands::Start(args) => agent_start(args).await,
            AgentCommands::Install(args) => agent_install(args),
            AgentCommands::Uninstall(args) => agent_uninstall(args),
        },
    }
}

// =============================================================================
// agent
// =============================================================================

async fn agent_status() -> Result<()> {
    let socket_path = devboy_secrets_agent::default_socket_path()
        .context("could not resolve the default agent socket path")?;
    let live = secrets_agent::is_socket_live(&socket_path).await;
    println!("socket path:  {}", socket_path.display());
    println!("status:       {}", if live { "live" } else { "down" });
    match secrets_agent::find_agent_binary() {
        Ok(p) => println!("binary:       {}", p.display()),
        Err(e) => println!("binary:       <not found> ({e})"),
    }
    Ok(())
}

async fn agent_start(args: AgentStartArgs) -> Result<()> {
    let socket_path = devboy_secrets_agent::default_socket_path()
        .context("could not resolve the default agent socket path")?;
    let timeout = match args.timeout_secs {
        Some(s) => Duration::from_secs(s),
        None => secrets_agent::DEFAULT_SPAWN_TIMEOUT,
    };
    secrets_agent::ensure_agent_running(&socket_path, args.vault.as_deref(), timeout).await?;
    println!(
        "agent is running on {} (waited up to {:?})",
        socket_path.display(),
        timeout
    );
    Ok(())
}

fn agent_install(args: AgentInstallArgs) -> Result<()> {
    let home = dirs::home_dir().context("could not resolve the user's home directory")?;
    let binary_path = match args.binary {
        Some(p) => p,
        None => secrets_agent::find_agent_binary()
            .context("could not locate the `devboy-secrets-agent` binary to install")?,
    };
    let log_path = default_log_path()?;
    let layout = UserServiceLayout {
        binary_path,
        log_path,
        socket_path: args.socket,
        vault_path: args.vault,
    };
    let options = ServiceOptions {
        load: !args.no_load,
    };
    let path = secrets_agent_service::install_user_service(&home, &layout, &options)?;
    println!("installed user service at {}", path.display());
    if options.load {
        println!("loaded via the platform service manager");
    } else {
        println!("--no-load was set; load it manually or reboot to pick it up");
    }
    Ok(())
}

fn agent_uninstall(args: AgentUninstallArgs) -> Result<()> {
    let home = dirs::home_dir().context("could not resolve the user's home directory")?;
    let options = ServiceOptions {
        load: !args.no_unload,
    };
    let removed = secrets_agent_service::uninstall_user_service(&home, &options)?;
    if removed {
        println!("removed the user service unit file");
    } else {
        println!("no user service unit installed; nothing to remove");
    }
    Ok(())
}

fn default_log_path() -> Result<PathBuf> {
    let dir = dirs::config_dir().context("could not resolve the user's config directory")?;
    Ok(dir.join("devboy-tools").join("secrets").join("agent.log"))
}

// =============================================================================
// list
// =============================================================================

fn list(args: ListArgs) -> Result<()> {
    let (output, _) = load_resolved()?;
    let mut rows: Vec<&ResolvedSecret> = output.secrets.values().collect();
    if !args.internal {
        rows.retain(|r| !r.path.is_internal());
    }

    if args.json {
        print_list_json(&rows, &output);
    } else {
        print_list_human(&rows, &output);
    }
    Ok(())
}

#[derive(Serialize)]
struct ListJsonOutput<'a> {
    secrets: Vec<ListJsonRow<'a>>,
    warnings: Vec<&'a devboy_storage::MergeWarning>,
}

#[derive(Serialize)]
struct ListJsonRow<'a> {
    path: &'a str,
    required: bool,
    origin: ListJsonOrigin,
    description: Option<&'a str>,
    expires_at: Option<&'a str>,
    rotate_every_days: Option<u32>,
    pattern_id: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum ListJsonOrigin {
    ProjectLocal,
    Global {
        overrides_applied: Vec<OverrideField>,
    },
}

impl ListJsonOrigin {
    fn from(o: &SecretOrigin) -> Self {
        match o {
            SecretOrigin::ProjectLocal => Self::ProjectLocal,
            SecretOrigin::Global { overrides_applied } => Self::Global {
                overrides_applied: overrides_applied.clone(),
            },
        }
    }
}

fn print_list_json(rows: &[&ResolvedSecret], output: &MergeOutput) {
    let json = ListJsonOutput {
        secrets: rows
            .iter()
            .map(|r| ListJsonRow {
                path: r.path.as_str(),
                required: r.required,
                origin: ListJsonOrigin::from(&r.origin),
                description: r.metadata.description.as_deref(),
                expires_at: r.metadata.expires_at.as_deref(),
                rotate_every_days: r.metadata.rotate_every_days,
                pattern_id: r.metadata.pattern_id.as_deref(),
            })
            .collect(),
        warnings: output.warnings.iter().collect(),
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&json).unwrap_or_default()
    );
}

fn print_list_human(rows: &[&ResolvedSecret], output: &MergeOutput) {
    if rows.is_empty() {
        println!("(no secrets declared in the active project's manifest)");
        return;
    }

    // Compute column widths so the table is readable without a real
    // table crate. Headers are the minimum width.
    let mut path_w = "PATH".len();
    let mut origin_w = "ORIGIN".len();
    let mut desc_w = "DESCRIPTION".len();
    for r in rows {
        path_w = path_w.max(r.path.as_str().len());
        origin_w = origin_w.max(format_origin_short(&r.origin).len());
        desc_w = desc_w.max(short_desc(r.metadata.description.as_deref()).len());
    }

    println!(
        "{:<width_path$}  {:<8}  {:<width_origin$}  {:<width_desc$}",
        "PATH",
        "REQ",
        "ORIGIN",
        "DESCRIPTION",
        width_path = path_w,
        width_origin = origin_w,
        width_desc = desc_w,
    );
    for r in rows {
        println!(
            "{:<width_path$}  {:<8}  {:<width_origin$}  {:<width_desc$}",
            r.path.as_str(),
            if r.required { "required" } else { "optional" },
            format_origin_short(&r.origin),
            short_desc(r.metadata.description.as_deref()),
            width_path = path_w,
            width_origin = origin_w,
            width_desc = desc_w,
        );
    }

    if !output.warnings.is_empty() {
        println!();
        println!("warnings ({}):", output.warnings.len());
        for w in &output.warnings {
            println!("  - {} ({:?})", w.path, w.kind);
        }
    }
}

fn override_field_name(f: &OverrideField) -> &'static str {
    // Mirror the snake_case names produced by serde on `OverrideField`
    // so the human output matches the JSON output.
    match f {
        OverrideField::Gate => "gate",
        OverrideField::RotateEveryDays => "rotate_every_days",
        OverrideField::Description => "description",
    }
}

fn format_origin_short(o: &SecretOrigin) -> String {
    match o {
        SecretOrigin::ProjectLocal => "project-local".to_owned(),
        SecretOrigin::Global { overrides_applied } if overrides_applied.is_empty() => {
            "global".to_owned()
        }
        SecretOrigin::Global { overrides_applied } => {
            format!("global+{}", overrides_applied.len())
        }
    }
}

fn short_desc(d: Option<&str>) -> String {
    match d {
        Some(s) if s.len() <= 60 => s.to_owned(),
        Some(s) => format!("{}…", &s[..59]),
        None => "—".to_owned(),
    }
}

// =============================================================================
// describe
// =============================================================================

fn describe(args: DescribeArgs) -> Result<()> {
    let path = SecretPath::parse(&args.path)
        .with_context(|| format!("invalid secret path '{}'", args.path))?;

    let (output, manifest) = load_resolved()?;

    if let Some(resolved) = output.secrets.get(&path) {
        if args.json {
            print_describe_json(resolved);
        } else {
            print_describe_human(resolved);
        }
        return Ok(());
    }

    // Path is not in the merged view — report what we know about it
    // from the raw manifest + global so the user can see why it
    // didn't resolve.
    let global = GlobalIndex::load().context("failed to load the global index")?;
    if let Some(entry) = global.get(&path) {
        if args.json {
            print_orphan_json(&path, "global-only-not-declared", Some(entry));
        } else {
            print_orphan_human(
                &path,
                "registered in the global index but not declared in this project's \
                 .devboy/secrets.toml",
                Some(entry),
            );
        }
        return Ok(());
    }
    if let Some(entry) = manifest.secrets.get(&path) {
        if args.json {
            print_orphan_json(&path, "project-local-not-declared", Some(entry));
        } else {
            print_orphan_human(
                &path,
                "declared as `[secret.\"...\"]` in the project manifest but absent from \
                 required/optional",
                Some(entry),
            );
        }
        return Ok(());
    }

    if args.json {
        print_orphan_json(&path, "unknown", None);
    } else {
        print_orphan_human(&path, "no metadata registered for this path", None);
    }

    anyhow::bail!(
        "secret path '{}' not found in any source — register it via the global index or add a \
         `[secret.\"{path}\"]` block",
        path,
        path = path
    );
}

#[derive(Serialize)]
struct DescribeJson<'a> {
    path: &'a str,
    required: bool,
    origin: ListJsonOrigin,
    metadata: &'a IndexEntry,
}

fn print_describe_json(r: &ResolvedSecret) {
    let json = DescribeJson {
        path: r.path.as_str(),
        required: r.required,
        origin: ListJsonOrigin::from(&r.origin),
        metadata: &r.metadata,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&json).unwrap_or_default()
    );
}

fn print_describe_human(r: &ResolvedSecret) {
    println!("path:            {}", r.path);
    println!("required:        {}", r.required);
    let origin_label = match &r.origin {
        SecretOrigin::ProjectLocal => "project-local".to_owned(),
        SecretOrigin::Global { overrides_applied } if overrides_applied.is_empty() => {
            "global".to_owned()
        }
        SecretOrigin::Global { overrides_applied } => {
            let names: Vec<&str> = overrides_applied.iter().map(override_field_name).collect();
            format!("global (overridden: {})", names.join(", "))
        }
    };
    println!("origin:          {}", origin_label);
    println!();
    print_metadata_block(&r.metadata);
}

fn print_metadata_block(m: &IndexEntry) {
    println!(
        "description:     {}",
        m.description.as_deref().unwrap_or("—")
    );
    println!(
        "retrieval url:   {}",
        m.retrieval_url.as_deref().unwrap_or("—")
    );
    println!(
        "format regex:    {}",
        m.format_regex.as_deref().unwrap_or("—")
    );
    println!(
        "default gate:    {}",
        match m.default_gate {
            Some(Gate::Auto) => "auto",
            Some(Gate::Confirm) => "confirm",
            Some(Gate::Touchid) => "touchid",
            None => "—",
        }
    );
    println!(
        "expires at:      {}",
        m.expires_at.as_deref().unwrap_or("—")
    );
    println!(
        "last rotated at: {}",
        m.last_rotated_at.as_deref().unwrap_or("—")
    );
    println!(
        "rotate every:    {}",
        m.rotate_every_days
            .map(|d| format!("{d} days"))
            .unwrap_or_else(|| "—".to_owned())
    );
    println!(
        "rotation method: {}",
        m.rotation_method
            .map(|r| format!("{r:?}").to_lowercase())
            .unwrap_or_else(|| "—".to_owned())
    );
    println!(
        "required scopes: {}",
        if m.required_scopes.is_empty() {
            "—".to_owned()
        } else {
            m.required_scopes.join(", ")
        }
    );
    println!(
        "pattern id:      {}",
        m.pattern_id.as_deref().unwrap_or("—")
    );
    println!("env var:         {}", m.env_var.as_deref().unwrap_or("—"));
    println!(
        "cache ttl max:   {}",
        m.cache_ttl_seconds_max
            .map(|s| format!("{s}s"))
            .unwrap_or_else(|| "—".to_owned())
    );
}

#[derive(Serialize)]
struct OrphanJson<'a> {
    path: &'a str,
    status: &'a str,
    metadata: Option<&'a IndexEntry>,
}

fn print_orphan_json(path: &SecretPath, status: &str, entry: Option<&IndexEntry>) {
    let json = OrphanJson {
        path: path.as_str(),
        status,
        metadata: entry,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&json).unwrap_or_default()
    );
}

fn print_orphan_human(path: &SecretPath, note: &str, entry: Option<&IndexEntry>) {
    println!("path:            {}", path);
    println!("status:          {}", note);
    if let Some(m) = entry {
        println!();
        print_metadata_block(m);
    }
}

// =============================================================================
// Loading
// =============================================================================

/// Load the global index and the project manifest from their default
/// on-disk locations and merge them. Returns the merge output plus
/// the raw manifest (the latter so `describe` can also probe
/// project-local-orphan paths).
fn load_resolved() -> Result<(MergeOutput, ProjectManifest)> {
    let global = GlobalIndex::load().context("failed to load the global secrets index")?;
    let manifest = ProjectManifest::load().context("failed to load the project manifest")?;
    let output = merge_manifest(&global, &manifest)
        .with_context(|| "failed to merge manifest with global index")?;
    Ok((output, manifest))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_storage::{IndexEntry, OverrideEntry, RotationMethod};

    fn p(s: &str) -> SecretPath {
        SecretPath::parse(s).unwrap()
    }

    fn build_resolved(
        path: &str,
        required: bool,
        origin: SecretOrigin,
        meta: IndexEntry,
    ) -> ResolvedSecret {
        ResolvedSecret {
            path: p(path),
            required,
            origin,
            metadata: meta,
        }
    }

    #[test]
    fn format_origin_short_handles_each_variant() {
        assert_eq!(
            format_origin_short(&SecretOrigin::ProjectLocal),
            "project-local"
        );
        assert_eq!(
            format_origin_short(&SecretOrigin::Global {
                overrides_applied: vec![]
            }),
            "global"
        );
        assert_eq!(
            format_origin_short(&SecretOrigin::Global {
                overrides_applied: vec![devboy_storage::OverrideField::Gate],
            }),
            "global+1"
        );
    }

    #[test]
    fn short_desc_truncates_long_strings() {
        let long = "x".repeat(200);
        let s = short_desc(Some(&long));
        assert!(s.ends_with('…'));
        assert_eq!(s.chars().count(), 60);
    }

    #[test]
    fn short_desc_keeps_short_strings_intact() {
        assert_eq!(short_desc(Some("short")), "short");
        assert_eq!(short_desc(None), "—");
    }

    #[test]
    fn list_json_origin_serializes_consistently() {
        // Ensure the JSON shape is stable (downstream tooling will
        // depend on it). Snake-case from `OverrideField`'s serde
        // attribute, kebab-cased nowhere — `rotate_every_days`, not
        // `rotateeverydays`.
        let o = ListJsonOrigin::from(&SecretOrigin::Global {
            overrides_applied: vec![
                devboy_storage::OverrideField::Gate,
                devboy_storage::OverrideField::RotateEveryDays,
                devboy_storage::OverrideField::Description,
            ],
        });
        let j = serde_json::to_value(&o).unwrap();
        assert_eq!(j["kind"], "global");
        assert_eq!(j["overrides_applied"][0], "gate");
        assert_eq!(j["overrides_applied"][1], "rotate_every_days");
        assert_eq!(j["overrides_applied"][2], "description");
    }

    #[test]
    fn describe_json_includes_every_metadata_field() {
        let r = build_resolved(
            "team/gitlab/token-deploy",
            true,
            SecretOrigin::Global {
                overrides_applied: vec![devboy_storage::OverrideField::Gate],
            },
            IndexEntry {
                description: Some("Team deploy".to_owned()),
                retrieval_url: Some("https://example.test".to_owned()),
                format_regex: Some("^glpat-.*$".to_owned()),
                default_gate: Some(Gate::Touchid),
                expires_at: Some("2026-08-01".to_owned()),
                last_rotated_at: Some("2026-05-02".to_owned()),
                rotate_every_days: Some(30),
                rotation_method: Some(RotationMethod::Manual),
                required_scopes: vec!["api".to_owned()],
                pattern_id: Some("gitlab-pat".to_owned()),
                env_var: Some("GITLAB_TOKEN_DEPLOY".to_owned()),
                cache_ttl_seconds_max: Some(60),
            },
        );
        let json = serde_json::to_value(DescribeJson {
            path: r.path.as_str(),
            required: r.required,
            origin: ListJsonOrigin::from(&r.origin),
            metadata: &r.metadata,
        })
        .unwrap();

        assert_eq!(json["path"], "team/gitlab/token-deploy");
        assert_eq!(json["required"], true);
        assert_eq!(json["origin"]["kind"], "global");
        assert_eq!(json["metadata"]["description"], "Team deploy");
        assert_eq!(json["metadata"]["pattern_id"], "gitlab-pat");
        assert_eq!(json["metadata"]["cache_ttl_seconds_max"], 60);
    }

    /// Smoke test: the merge integration point compiles and runs end
    /// to end when a project-local + global path is resolved through
    /// `merge_manifest` (the same code path the CLI takes).
    #[test]
    fn list_filters_internal_paths_by_default() {
        let mut global = GlobalIndex::new();
        // Hidden internal path lives under the framework-reserved
        // `__sources/...` namespace.
        global.insert(
            SecretPath::parse_internal("__sources/vault-team/deploy").unwrap(),
            IndexEntry::default(),
        );
        global.insert(p("team/foo/token"), IndexEntry::default());

        // The CLI's `list` filters AFTER the merge; build a manifest
        // that pulls in both.
        let mut manifest = ProjectManifest::new();
        manifest.required.push(p("team/foo/token"));
        // Internal paths can't be in `required` (they fail user-facing
        // SecretPath parsing); the filter exists for cases where the
        // global has internal entries that would otherwise leak into
        // `secrets list --internal`. So this test reaches into the
        // merge output directly.

        let out = devboy_storage::merge_manifest(&global, &manifest).unwrap();
        let with_internal: Vec<&ResolvedSecret> = out.secrets.values().collect();
        let public: Vec<&ResolvedSecret> = with_internal
            .iter()
            .copied()
            .filter(|r| !r.path.is_internal())
            .collect();

        // Manifest only declared the public path, so both views match.
        assert_eq!(public.len(), 1);
        assert_eq!(with_internal.len(), 1);
    }

    #[test]
    fn override_field_name_matches_serde_snake_case() {
        // `override_field_name` (used in human output) must agree with
        // serde's `rename_all = "snake_case"` (used in JSON output).
        for f in [
            OverrideField::Gate,
            OverrideField::RotateEveryDays,
            OverrideField::Description,
        ] {
            let serde_name = serde_json::to_value(f)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(override_field_name(&f), serde_name);
        }
    }

    /// Use OverrideEntry just to keep the import alive — the CLI does
    /// not construct one directly but the type round-trips through
    /// `merge_manifest`.
    #[test]
    fn override_entry_default_is_empty() {
        let oe = OverrideEntry::default();
        assert!(oe.is_empty());
    }
}
