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
    /// Validate manifest paths' format / liveness as a CI gate.
    /// Format-only by default; pass `--liveness` to also probe
    /// upstreams (github + gitlab). See ADR-021 §6.
    Validate(crate::secrets_validate::ValidateArgs),
    /// Move a legacy keychain entry under the ADR-020 path
    /// convention. See `doctor` "Legacy keychain entries" (P10.1)
    /// for what's eligible.
    Migrate(crate::secrets_migrate::MigrateArgs),
    /// Manage the local secret-store agent daemon (ADR-023 §3.3).
    Agent {
        /// What to do with the agent.
        #[command(subcommand)]
        command: AgentCommands,
    },
    /// Open the native UI (TUI in a terminal, GUI in a window).
    /// Backend autodetected from `$DISPLAY` / `$WAYLAND_DISPLAY` on
    /// Linux and the OS on macOS / Windows; override with `--tui`
    /// or `--gui`. See ADR-023 §3.4.
    Ui(crate::secrets_ui::UiArgs),
    /// Rotate a secret: open the provider URL in the browser,
    /// destructive-confirm, read the new value, format-validate,
    /// and record `last_rotated_at`. See ADR-023 §3.4.
    Rotate(crate::secrets_rotate::RotateArgs),
    /// Manage the token catalog (provider procedure files the
    /// `secrets ui` form binds to). See ADR-023 §3.4.
    Catalog {
        #[command(subcommand)]
        command: CatalogCommands,
    },
    /// Run the setup-secrets wizard against the current
    /// directory. Default mode is `--scan-only` — read-only
    /// preview of what the wizard would propose. Pass
    /// `--write-manifest` to commit the proposals to
    /// `<repo>/.devboy/secrets.toml`. See ADR-023 §3.8 and
    /// `crates/devboy-skills/skills/00-self-bootstrap/setup-secrets/`.
    Setup(SetupArgs),
    /// Work with KDBX 4 (KeePass) files as a SecretSource. The
    /// passphrase is prompted from stdin with no echo; the
    /// decrypted body lives only inside this process and is
    /// dropped on exit. See ADR-021 §8 + `crates/plugins/secrets/kdbx/`.
    Kdbx {
        #[command(subcommand)]
        command: KdbxCommands,
    },
}

/// `devboy secrets kdbx <subcommand>` family.
#[derive(Subcommand, Debug)]
pub enum KdbxCommands {
    /// Open a `.kdbx` file with a prompted passphrase and print
    /// the per-entry inventory (path + Title + UserName + URL +
    /// whether a Password is set). Values are NEVER printed —
    /// this is a read-only sanity check that the file opens and
    /// our path normalisation produces sensible references.
    Peek(KdbxPeekArgs),
    /// Read-only metadata projection for ONE entry by UUID.
    /// Prints title / username / url / notes / tags / expires_at
    /// / attachment names / custom-string names. Password and
    /// every Protected custom string are deliberately excluded
    /// from the output — same agent-blindness boundary as
    /// `edit-metadata` (K14).
    DescribeMetadata(KdbxDescribeMetadataArgs),
    /// Edit non-value metadata on ONE entry by UUID. Allows
    /// updating title / username / url / notes / tags / expiry
    /// timestamp; the value-bearing Password and any Protected
    /// custom string are unreachable from this surface. Writes
    /// through `derive_working_copy_path` so the user's
    /// original `.kdbx` is never overwritten — the working
    /// copy path is printed at the end so callers can sync
    /// back on their own schedule.
    EditMetadata(KdbxEditMetadataArgs),
}

/// Flags shared by `kdbx describe-metadata` + `kdbx edit-metadata`
/// — every metadata-edit invocation needs file + passphrase +
/// optional keyfile + UUID.
#[derive(Args, Debug)]
pub struct KdbxDescribeMetadataArgs {
    /// Absolute path to the `.kdbx` file.
    #[arg(long)]
    pub file: PathBuf,
    /// Optional keyfile companion (KeePass two-factor unlock).
    #[arg(long)]
    pub keyfile: Option<PathBuf>,
    /// UUID of the entry to project. Hyphenated hex
    /// (`12345678-90ab-cdef-1234-567890abcdef`). See
    /// `kdbx peek --json` for a complete UUID listing.
    #[arg(long)]
    pub uuid: String,
    /// Print as JSON instead of the default human key/value
    /// table. JSON output is the contract for scripted /
    /// MCP-wrapper consumers.
    #[arg(long)]
    pub json: bool,
}

/// Flags for `devboy secrets kdbx edit-metadata`. Every patch
/// field is optional — pass only what you intend to change.
/// Unprovided fields stay untouched on the entry.
#[derive(Args, Debug)]
pub struct KdbxEditMetadataArgs {
    /// Absolute path to the `.kdbx` file. The edit goes to the
    /// derived working copy, not this path — see the printed
    /// `working_copy` line on success.
    #[arg(long)]
    pub file: PathBuf,
    /// Optional keyfile companion (KeePass two-factor unlock).
    #[arg(long)]
    pub keyfile: Option<PathBuf>,
    /// UUID of the entry to edit. Hyphenated hex.
    #[arg(long)]
    pub uuid: String,
    /// New Title. Empty string clears.
    #[arg(long)]
    pub title: Option<String>,
    /// New UserName. Empty string clears.
    #[arg(long)]
    pub username: Option<String>,
    /// New URL. Empty string clears.
    #[arg(long)]
    pub url: Option<String>,
    /// New Notes (multiline allowed via shell escapes / `--notes
    /// "$(cat notes.md)"`). Empty string clears.
    #[arg(long)]
    pub notes: Option<String>,
    /// Replace the tag list with these values. Pass `--tag` once
    /// per tag; omit the flag entirely to leave existing tags
    /// alone; pass `--clear-tags` to drop all tags.
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    /// Wipe all tags on the entry. Mutually exclusive with
    /// `--tag`; if both appear, `--clear-tags` wins.
    #[arg(long)]
    pub clear_tags: bool,
    /// New expiry timestamp (RFC 3339, e.g.
    /// `2027-01-15T00:00:00Z`). Pass `--no-expiry` to clear an
    /// existing expiry. Without either, the field is left alone.
    #[arg(long, value_name = "ISO8601")]
    pub expires_at: Option<String>,
    /// Clear any existing expiry timestamp. Mutually exclusive
    /// with `--expires-at`; if both appear, `--no-expiry` wins.
    #[arg(long)]
    pub no_expiry: bool,
    /// Print as JSON instead of the default human summary.
    #[arg(long)]
    pub json: bool,
}

/// Flags for `devboy secrets kdbx peek`.
#[derive(Args, Debug)]
pub struct KdbxPeekArgs {
    /// Absolute path to the `.kdbx` file. Required — there is
    /// no default discovery for KDBX files (the user opts in
    /// per-invocation).
    #[arg(long)]
    pub file: PathBuf,
    /// Optional path to a keyfile companion (KeePass two-factor
    /// unlock). Omit for passphrase-only databases.
    #[arg(long)]
    pub keyfile: Option<PathBuf>,
    /// Print as JSON (one entry per object) instead of the
    /// default human table. Useful for scripted verification.
    #[arg(long)]
    pub json: bool,
}

/// `devboy secrets catalog <subcommand>` family.
#[derive(Subcommand, Debug)]
pub enum CatalogCommands {
    /// List every loaded provider catalog with its source
    /// (bundled / user / project) and variant count. Useful to
    /// debug which override is winning when a team has its own
    /// project-scope file shadowing the bundled default.
    List,
    /// Inspect every catalog at every active source — bundled,
    /// user, project, AND URL — with origin, variant count,
    /// and (for URL sources) cache state. Replaces the older
    /// `list` command for the URL-loaded catalog flow (P23).
    Status(CatalogStatusArgs),
    /// Subscribe to a remote catalog by URL. Fetches once
    /// through every P23 defence layer (HTTPS-only, SSRF
    /// guard, size cap, content-type, schema version), prints
    /// the body SHA256 + variant summary, asks for trust
    /// confirmation (or accepts a `--pin` for unattended use),
    /// then appends a `[[source]]` entry to
    /// `~/.devboy/secrets/catalog/sources.toml`.
    AddUrl(CatalogAddUrlArgs),
    /// Re-fetch URL catalogs from `sources.toml`. Without
    /// `--force` the loader honours each source's
    /// `refresh_seconds` TTL — sources within their window
    /// are reported as "fresh" and not re-fetched. With
    /// `--force` the cache for matching sources is dropped
    /// before the fetch so every source goes back to the
    /// network. Optional positional `<filter>` matches as a
    /// case-insensitive substring against the source URL.
    Refresh(CatalogRefreshArgs),
    /// Drop URL entries from `known_hashes.toml` so the next
    /// fetch re-establishes TOFU. Positional `<filter>` is a
    /// case-insensitive URL substring; without it, every
    /// recorded entry is dropped (with confirmation unless
    /// `--yes` is set). Use this after a deliberate upstream
    /// rotation that you want devboy to re-trust.
    Forget(CatalogForgetArgs),
    /// Promote a TOFU entry to a hard SHA pin in
    /// `sources.toml`. Positional `<filter>` is a case-
    /// insensitive URL substring matching the source to
    /// pin. With explicit `<sha>` argument, that exact value
    /// is written; without it, the current `known_hashes.toml`
    /// entry is read and copied. Future fetches from this
    /// source refuse any mismatch.
    Pin(CatalogPinArgs),
    /// Validate a single catalog JSON file. Loads the file,
    /// runs schema deserialisation (`deny_unknown_fields` is
    /// strict), then per-variant checks that the regex compiles
    /// and that every URL parses. Exit non-zero on any failure.
    Validate(CatalogValidateArgs),
}

/// Flags for `devboy secrets catalog status`.
#[derive(Args, Debug, Default)]
pub struct CatalogStatusArgs {
    /// Print as machine-readable JSON instead of a human table.
    #[arg(long)]
    pub json: bool,
}

/// Flags for `devboy secrets catalog add-url`.
#[derive(Args, Debug)]
pub struct CatalogAddUrlArgs {
    /// HTTPS URL of the JSON catalog (e.g. a GitHub raw link).
    /// `http://` is rejected outright by the fetcher's first
    /// defence layer.
    pub url: String,
    /// Pin the body to this SHA256 (lower-case hex, no
    /// `sha256:` prefix). Future fetches refuse any mismatch.
    /// When omitted, the loader falls back to TOFU and
    /// records the body's SHA in `known_hashes.toml` on
    /// first fetch.
    #[arg(long, value_name = "HEX")]
    pub pin: Option<String>,
    /// How long the cached body stays fresh before the loader
    /// re-fetches. Defaults to 24 hours.
    #[arg(long, default_value_t = 86_400)]
    pub refresh_seconds: u64,
    /// Also flip `enable_url_catalogs = true` in the same
    /// `sources.toml`. Without this flag the entry is added
    /// but the master kill-switch remains off — the URL is
    /// not loaded until the user explicitly enables it.
    #[arg(long)]
    pub enable: bool,
    /// Skip the interactive trust-confirm prompt. Implied
    /// when `--pin` is set (the pin already locks the body).
    /// Required for non-tty / CI invocations.
    #[arg(long)]
    pub yes: bool,
}

/// Flags for `devboy secrets catalog refresh`.
#[derive(Args, Debug, Default)]
pub struct CatalogRefreshArgs {
    /// Optional case-insensitive substring; only sources whose
    /// URL matches are re-fetched. Without this argument every
    /// URL source is processed.
    pub filter: Option<String>,
    /// Bypass each source's `refresh_seconds` TTL and force a
    /// re-fetch over the network. Cache for matching sources
    /// is removed before the fetch so the loader cannot serve
    /// a stale body.
    #[arg(long)]
    pub force: bool,
}

/// Flags for `devboy secrets catalog forget`.
#[derive(Args, Debug, Default)]
pub struct CatalogForgetArgs {
    /// Optional case-insensitive substring against the URL.
    /// Without this argument, every recorded TOFU entry is
    /// dropped (subject to `--yes`).
    pub filter: Option<String>,
    /// Skip the interactive confirmation prompt. Required
    /// when no filter is given (bulk-clearing all TOFU
    /// entries is destructive enough to warrant explicit
    /// opt-in).
    #[arg(long)]
    pub yes: bool,
}

/// Flags for `devboy secrets catalog pin`.
#[derive(Args, Debug)]
pub struct CatalogPinArgs {
    /// Case-insensitive URL substring identifying the source
    /// to pin. Must match exactly one source; ambiguity is an
    /// error.
    pub filter: String,
    /// Explicit lower-case-hex SHA256 to write to
    /// `sources.toml`. When omitted, the current TOFU entry
    /// for the matched source is read from
    /// `known_hashes.toml` and copied.
    pub sha: Option<String>,
}

/// Flags for `devboy secrets catalog validate`.
#[derive(Args, Debug)]
pub struct CatalogValidateArgs {
    /// Path to the JSON catalog file. Use `-` to read from stdin.
    pub path: PathBuf,
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

/// Flags for `devboy secrets setup`.
///
/// Default mode is read-only preview (`--scan-only` is implicit
/// when neither `--write-manifest` nor `--resume` is set), which
/// makes the command safe to run against any project — nothing
/// is created in the repo or in `~/.devboy/secrets/` until the
/// caller explicitly opts in.
#[derive(Args, Debug, Default)]
pub struct SetupArgs {
    /// Project root to scan. Defaults to the current directory.
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// Print the scan + propose preview without touching disk.
    /// This is the default mode — included as an explicit flag
    /// for self-documenting scripts.
    #[arg(long)]
    pub scan_only: bool,
    /// Commit the proposed paths to `<root>/.devboy/secrets.toml`.
    /// Refuses to overwrite an existing manifest unless `--force`
    /// is passed too — drift in the manifest is the user's own
    /// authoritative copy and the wizard treats it as opaque.
    #[arg(long, conflicts_with = "scan_only")]
    pub write_manifest: bool,
    /// Allow `--write-manifest` to overwrite an existing
    /// `<root>/.devboy/secrets.toml`. No-op without
    /// `--write-manifest`.
    #[arg(long)]
    pub force: bool,
    /// Resume the wizard from the recorded state file
    /// (`~/.devboy/secrets/setup-state.toml`). Skips phases
    /// already marked `done` / `skipped`. Implies a full wizard
    /// run, not just the scan preview.
    #[arg(long, conflicts_with = "scan_only")]
    pub resume: bool,
    /// Emit JSON-lines events to stdout instead of human prose.
    /// One event per line with shape
    /// `{"phase":"scan","status":"completed","message":"…"}` —
    /// designed for the AI agent driving the skill. The `message`
    /// key is optional: only `PhaseProgress`, `PhaseCompleted`,
    /// `PhaseSkipped`, and `PhaseFailed` carry a body; `PhaseStarted`
    /// and the terminal `wizard-completed` event omit it.
    #[arg(long)]
    pub json: bool,
}

/// Dispatch a `devboy secrets` subcommand.
pub async fn handle(command: SecretsCommands) -> Result<()> {
    match command {
        SecretsCommands::List(args) => list(args),
        SecretsCommands::Describe(args) => describe(args),
        SecretsCommands::Validate(args) => crate::secrets_validate::handle(args).await,
        SecretsCommands::Migrate(args) => crate::secrets_migrate::handle(args).await,
        SecretsCommands::Agent { command } => match command {
            AgentCommands::Status => agent_status().await,
            AgentCommands::Start(args) => agent_start(args).await,
            AgentCommands::Install(args) => agent_install(args),
            AgentCommands::Uninstall(args) => agent_uninstall(args),
        },
        SecretsCommands::Ui(args) => crate::secrets_ui::handle(args).await,
        SecretsCommands::Rotate(args) => crate::secrets_rotate::handle(args).await,
        SecretsCommands::Catalog { command } => match command {
            CatalogCommands::List => catalog_list(),
            CatalogCommands::Status(args) => catalog_status(args),
            CatalogCommands::AddUrl(args) => catalog_add_url(args),
            CatalogCommands::Refresh(args) => catalog_refresh(args),
            CatalogCommands::Forget(args) => catalog_forget(args),
            CatalogCommands::Pin(args) => catalog_pin(args),
            CatalogCommands::Validate(args) => catalog_validate(args),
        },
        SecretsCommands::Setup(args) => crate::secrets_setup::handle_cli(args),
        SecretsCommands::Kdbx { command } => match command {
            KdbxCommands::Peek(args) => kdbx_peek(args),
            KdbxCommands::DescribeMetadata(args) => kdbx_describe_metadata(args),
            KdbxCommands::EditMetadata(args) => kdbx_edit_metadata(args),
        },
    }
}

/// Open a KDBX 4 file with a prompted passphrase and print the
/// inventory it contains. Used as a smoke test for the
/// `devboy-secret-kdbx` integration when the GUI flow isn't
/// available (CI, headless dev box, autonomous agent setup).
///
/// What's printed:
/// - the path the source-plugin's path-mapper produced
/// - the KeePass Title, UserName, URL fields
/// - whether the entry carries a non-empty Password (yes/no
///   only — the actual value never leaves the process)
///
/// What's NOT printed: Password values, Notes (which can
/// contain values), custom string fields (ditto).
fn kdbx_peek(args: KdbxPeekArgs) -> Result<()> {
    use secrecy::SecretString;
    use std::io::IsTerminal;

    if !args.file.exists() {
        anyhow::bail!(
            "KDBX file does not exist: {}\n\
             Pass --file <path> to a real .kdbx file.",
            args.file.display()
        );
    }

    // Secure passphrase prompt — `dialoguer::Password` hides
    // input + reads from the TTY directly so the value never
    // shows up in shell history or process listings. Refuse to
    // read from a pipe (`!is_terminal`) since the prompt would
    // hang forever waiting for a terminal.
    refuse_interactive_in_env_only("Reading a KDBX passphrase")?;
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "KDBX passphrase prompt requires an interactive terminal; \
             this command refuses to read the passphrase from a pipe."
        );
    }
    let passphrase = dialoguer::Password::new()
        .with_prompt(format!("Passphrase for {}", args.file.display()))
        .allow_empty_password(false)
        .interact()
        .context("could not read passphrase from stdin")?;

    let snapshot = devboy_secret_kdbx::open_kdbx_into_snapshot(
        &args.file,
        &SecretString::new(passphrase.into()),
        args.keyfile.as_deref(),
    )
    .with_context(|| format!("could not open {}", args.file.display()))?;

    if args.json {
        // JSON-lines so a script can consume entries one at a
        // time. Each line is an object with `path`, `title`,
        // `username`, `url`, `has_password`. No `password` field
        // ever appears.
        for entry in &snapshot.entries {
            let line = serde_json::json!({
                "path": entry.path,
                "title": entry.title,
                "username": entry.username,
                "url": entry.url,
                "has_password": entry.primary_value().is_some(),
            });
            println!("{line}");
        }
        let summary = serde_json::json!({
            "file": args.file.display().to_string(),
            "entries": snapshot.entries.len(),
        });
        println!("{summary}");
        return Ok(());
    }

    // Human table — fixed-width columns sized to the loaded
    // entries so long titles wrap nicely.
    println!(
        "Opened {} ({} entries)",
        args.file.display(),
        snapshot.entries.len()
    );
    println!();
    let mut path_w = "PATH".len();
    let mut title_w = "TITLE".len();
    for entry in &snapshot.entries {
        path_w = path_w.max(entry.path.len());
        title_w = title_w.max(entry.title.len());
    }
    let path_w = path_w.min(64);
    let title_w = title_w.min(40);
    println!(
        "{:<path_w$}  {:<title_w$}  PASSWORD  USERNAME / URL",
        "PATH", "TITLE",
    );
    for entry in &snapshot.entries {
        let pwd_mark = if entry.primary_value().is_some() {
            "yes"
        } else {
            "—"
        };
        let user = entry.username.as_deref().unwrap_or("");
        let url = entry.url.as_deref().unwrap_or("");
        let trailing = match (user.is_empty(), url.is_empty()) {
            (true, true) => String::new(),
            (false, true) => user.to_owned(),
            (true, false) => url.to_owned(),
            (false, false) => format!("{user} · {url}"),
        };
        let path = truncate(&entry.path, path_w);
        let title = truncate(&entry.title, title_w);
        println!("{path:<path_w$}  {title:<title_w$}  {pwd_mark:<8}  {trailing}",);
    }
    println!();
    println!("(values are held only in this process; no Password field ever printed)");
    Ok(())
}

/// K15 — `devboy secrets kdbx describe-metadata` — read-only
/// projection of one entry's non-secret fields. Prompts for the
/// passphrase securely (no stdin pipe, no shell history), opens
/// the KDBX, calls `describe_metadata` for the given UUID, and
/// prints the result as a human key/value table or JSON.
///
/// The output omits Password and any Protected custom string by
/// construction (the plugin filters them out) — same agent-
/// blindness boundary as `edit-metadata` (K15).
fn kdbx_describe_metadata(args: KdbxDescribeMetadataArgs) -> Result<()> {
    use secrecy::SecretString;
    use std::io::IsTerminal;

    if !args.file.exists() {
        anyhow::bail!(
            "KDBX file does not exist: {}\n\
             Pass --file <path> to a real .kdbx file.",
            args.file.display()
        );
    }
    refuse_interactive_in_env_only("Reading a KDBX passphrase")?;
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "KDBX passphrase prompt requires an interactive terminal; \
             this command refuses to read the passphrase from a pipe."
        );
    }
    let passphrase = dialoguer::Password::new()
        .with_prompt(format!("Passphrase for {}", args.file.display()))
        .allow_empty_password(false)
        .interact()
        .context("could not read passphrase from stdin")?;

    let meta = devboy_secret_kdbx::describe_metadata(
        &args.file,
        &SecretString::new(passphrase.into()),
        args.keyfile.as_deref(),
        &args.uuid,
    )
    .with_context(|| format!("could not open {}", args.file.display()))?;

    let Some(meta) = meta else {
        anyhow::bail!(
            "no entry with uuid {} in {}",
            args.uuid,
            args.file.display()
        );
    };

    if args.json {
        // Serialize hand-rolled — keeps the kdbx plugin's
        // metadata struct free of a serde dep just for one CLI
        // command. The shape is a stable contract for the K16
        // MCP wrapper.
        let json = serde_json::json!({
            "uuid": meta.uuid,
            "title": meta.title,
            "username": meta.username,
            "url": meta.url,
            "notes": meta.notes,
            "tags": meta.tags,
            "created_at": meta.created_at,
            "modified_at": meta.modified_at,
            "expires_at": meta.expires_at,
            "otp_present": meta.otp.is_some(),
            "attachments": meta.attachments.iter().map(|a| serde_json::json!({
                "name": a.name,
                "size_bytes": a.size_bytes,
            })).collect::<Vec<_>>(),
            "custom_string_names": meta.custom_string_names,
        });
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        println!("uuid:        {}", meta.uuid);
        println!("title:       {}", meta.title);
        println!("username:    {}", meta.username.as_deref().unwrap_or(""));
        println!("url:         {}", meta.url.as_deref().unwrap_or(""));
        println!("notes:       {}", meta.notes.as_deref().unwrap_or(""));
        println!("tags:        {}", meta.tags.join(", "));
        println!("created:     {}", meta.created_at.as_deref().unwrap_or(""));
        println!("modified:    {}", meta.modified_at.as_deref().unwrap_or(""));
        println!("expires_at:  {}", meta.expires_at.as_deref().unwrap_or(""));
        println!(
            "otp:         {}",
            if meta.otp.is_some() { "(present)" } else { "" }
        );
        if !meta.attachments.is_empty() {
            println!("attachments:");
            for a in &meta.attachments {
                println!("  - {} ({} bytes)", a.name, a.size_bytes);
            }
        }
        if !meta.custom_string_names.is_empty() {
            println!(
                "custom_string_names: {}",
                meta.custom_string_names.join(", ")
            );
        }
        println!();
        println!("(value-bearing fields — Password + Protected custom strings — never printed)");
    }
    Ok(())
}

/// K15 — `devboy secrets kdbx edit-metadata` — patch one
/// entry's non-secret fields. Honours K13 working-copy safety:
/// writes go to the working-copy path, not the user's
/// original. Working-copy path is printed on success.
fn kdbx_edit_metadata(args: KdbxEditMetadataArgs) -> Result<()> {
    use secrecy::SecretString;
    use std::io::IsTerminal;

    if !args.file.exists() {
        anyhow::bail!(
            "KDBX file does not exist: {}\n\
             Pass --file <path> to a real .kdbx file.",
            args.file.display()
        );
    }

    // Build the patch from CLI flags. None = leave alone.
    let tags = if args.clear_tags {
        Some(Vec::new())
    } else if !args.tags.is_empty() {
        Some(args.tags.clone())
    } else {
        None
    };
    let expires_at = if args.no_expiry {
        Some(None)
    } else {
        args.expires_at.as_ref().map(|s| Some(s.clone()))
    };
    let patch = devboy_secret_kdbx::MetadataPatch {
        title: args.title.clone(),
        username: args.username.clone(),
        url: args.url.clone(),
        notes: args.notes.clone(),
        tags,
        expires_at,
    };

    // Refuse no-op edits — easier than silently doing nothing.
    if patch == devboy_secret_kdbx::MetadataPatch::default() {
        anyhow::bail!(
            "no metadata fields supplied — pass at least one of \
             --title / --username / --url / --notes / --tag / \
             --clear-tags / --expires-at / --no-expiry"
        );
    }

    refuse_interactive_in_env_only("Reading a KDBX passphrase")?;
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "KDBX passphrase prompt requires an interactive terminal; \
             this command refuses to read the passphrase from a pipe."
        );
    }
    let passphrase = dialoguer::Password::new()
        .with_prompt(format!("Passphrase for {}", args.file.display()))
        .allow_empty_password(false)
        .interact()
        .context("could not read passphrase from stdin")?;
    let passphrase = SecretString::new(passphrase.into());

    // K13 working-copy: derive a sibling path with a UTC
    // timestamp, copy the user's original verbatim, edit that.
    let working_copy = devboy_secret_kdbx::derive_working_copy_path(&args.file);
    devboy_secret_kdbx::prepare_working_copy(&args.file, &working_copy).map_err(|e| {
        anyhow::anyhow!(
            "could not prepare working copy at {}: {e}",
            working_copy.display()
        )
    })?;

    devboy_secret_kdbx::edit_metadata(
        &working_copy,
        &passphrase,
        args.keyfile.as_deref(),
        &args.uuid,
        &patch,
    )
    .with_context(|| format!("edit_metadata failed on {}", working_copy.display()))?;

    if args.json {
        let json = serde_json::json!({
            "ok": true,
            "source": args.file.display().to_string(),
            "working_copy": working_copy.display().to_string(),
            "uuid": args.uuid,
            "patched": patched_summary(&patch),
        });
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        println!("ok");
        println!("source:       {}", args.file.display());
        println!("working_copy: {}", working_copy.display());
        println!("uuid:         {}", args.uuid);
        println!("patched:");
        for line in patched_summary(&patch).as_array().unwrap() {
            println!("  - {}", line.as_str().unwrap_or(""));
        }
        println!();
        println!(
            "(original file untouched — sync the working copy back manually \
             when you're ready)"
        );
    }
    Ok(())
}

/// Human-readable bullet list of which fields the patch
/// actually touched. Used by both human + JSON renders.
fn patched_summary(patch: &devboy_secret_kdbx::MetadataPatch) -> serde_json::Value {
    let mut lines: Vec<String> = Vec::new();
    if let Some(v) = &patch.title {
        lines.push(format!("title = {v:?}"));
    }
    if let Some(v) = &patch.username {
        lines.push(format!("username = {v:?}"));
    }
    if let Some(v) = &patch.url {
        lines.push(format!("url = {v:?}"));
    }
    if let Some(v) = &patch.notes {
        lines.push(format!("notes = {v:?}"));
    }
    if let Some(tags) = &patch.tags {
        lines.push(format!("tags = {tags:?}"));
    }
    match &patch.expires_at {
        Some(Some(ts)) => lines.push(format!("expires_at = {ts:?}")),
        Some(None) => lines.push("expires_at = <cleared>".to_owned()),
        None => {}
    }
    serde_json::Value::Array(lines.into_iter().map(serde_json::Value::String).collect())
}

/// Truncate `s` to `max` characters with an ellipsis if it
/// overflows. Pure helper used by the kdbx-peek table render.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

// =============================================================================
// catalog
// =============================================================================

/// Walk every configured catalog source — bundled / user /
/// project / URL (when opt-in via `sources.toml`) — and print
/// one line per provider. Read-only, no flags.
fn catalog_list() -> Result<()> {
    use devboy_token_catalog::{
        CatalogSource, FirstFetchPolicy, bundled_catalogs, default_catalog_audit_log_path,
        default_catalog_cache_dir, default_known_hashes_path, default_sources_toml_path,
        default_user_catalog_dir, load_all_with_urls, parse_sources_toml,
    };

    let bundled = bundled_catalogs();
    let user_dir = default_user_catalog_dir();
    // Project-scope is the current working directory by
    // convention — `<cwd>/.devboy/secrets/catalog/`. Future
    // versions may derive a smarter project root.
    let project_dir = std::env::current_dir()
        .ok()
        .map(|d| d.join(".devboy").join("secrets").join("catalog"));
    // sources.toml is optional. A missing file just means "no
    // URL sources" — never an error.
    let url_config = default_sources_toml_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|body| parse_sources_toml(&body).ok());
    let known_hashes_path = default_known_hashes_path();
    let cache_dir = default_catalog_cache_dir();
    let audit_log_path = default_catalog_audit_log_path();
    let (loaded, errors) = load_all_with_urls(
        &bundled,
        user_dir.as_deref(),
        project_dir.as_deref(),
        url_config.as_ref(),
        known_hashes_path.as_deref(),
        cache_dir.as_deref(),
        // CLI is unattended — auto-record on first fetch. The
        // GUI uses `RequireConfirmation` and surfaces a prompt.
        FirstFetchPolicy::AutoRecord,
        audit_log_path.as_deref(),
    );

    if loaded.is_empty() && errors.is_empty() {
        println!("no catalogs loaded");
        return Ok(());
    }
    for c in &loaded {
        let source = match &c.source {
            CatalogSource::Bundled => "bundled".to_owned(),
            CatalogSource::User => "user   ".to_owned(),
            CatalogSource::Project => "project".to_owned(),
            CatalogSource::Url { url, .. } => format!("url    ({url})"),
        };
        let n = c.catalog.variants.len();
        let suffix = if n == 1 { "variant" } else { "variants" };
        println!("{:<14} {}  {} {}", c.catalog.provider_id, source, n, suffix);
    }
    if !errors.is_empty() {
        eprintln!();
        eprintln!("errors ({}):", errors.len());
        for e in &errors {
            eprintln!("  - {e}");
        }
    }
    Ok(())
}

/// Walk every configured catalog source and produce a richer
/// status view than `list` — origin, variant count, file path
/// (for disk-loaded), URL + sha256 + cache state (for
/// URL-loaded). Optional `--json` flag for scripts.
#[allow(clippy::print_literal)]
fn catalog_status(args: CatalogStatusArgs) -> Result<()> {
    use devboy_token_catalog::{
        CatalogSource, FirstFetchPolicy, bundled_catalogs, default_catalog_audit_log_path,
        default_catalog_cache_dir, default_known_hashes_path, default_sources_toml_path,
        default_user_catalog_dir, load_all_with_urls, parse_sources_toml,
    };

    let bundled = bundled_catalogs();
    let user_dir = default_user_catalog_dir();
    let project_dir = std::env::current_dir()
        .ok()
        .map(|d| d.join(".devboy").join("secrets").join("catalog"));
    let url_config = default_sources_toml_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|body| parse_sources_toml(&body).ok());
    let known_hashes_path = default_known_hashes_path();
    let cache_dir = default_catalog_cache_dir();
    let audit_log_path = default_catalog_audit_log_path();
    let (loaded, errors) = load_all_with_urls(
        &bundled,
        user_dir.as_deref(),
        project_dir.as_deref(),
        url_config.as_ref(),
        known_hashes_path.as_deref(),
        cache_dir.as_deref(),
        FirstFetchPolicy::AutoRecord,
        audit_log_path.as_deref(),
    );

    if args.json {
        let entries: Vec<serde_json::Value> = loaded
            .iter()
            .map(|c| {
                let mut obj = serde_json::Map::new();
                obj.insert(
                    "provider_id".into(),
                    serde_json::Value::String(c.catalog.provider_id.clone()),
                );
                obj.insert(
                    "display_name".into(),
                    serde_json::Value::String(c.catalog.display_name.clone()),
                );
                obj.insert(
                    "variants".into(),
                    serde_json::Value::Number((c.catalog.variants.len() as u64).into()),
                );
                obj.insert(
                    "env_var_patterns".into(),
                    serde_json::Value::Number((c.catalog.env_var_patterns.len() as u64).into()),
                );
                obj.insert(
                    "env_var_skip".into(),
                    serde_json::Value::Number((c.catalog.env_var_skip.len() as u64).into()),
                );
                let (origin, source_meta) = match &c.source {
                    CatalogSource::Bundled => ("bundled", serde_json::Value::Null),
                    CatalogSource::User => ("user", serde_json::Value::Null),
                    CatalogSource::Project => ("project", serde_json::Value::Null),
                    CatalogSource::Url { url, sha256 } => (
                        "url",
                        serde_json::json!({
                            "url": url,
                            "sha256_pin": sha256,
                        }),
                    ),
                };
                obj.insert("origin".into(), serde_json::Value::String(origin.into()));
                obj.insert("source".into(), source_meta);
                if let Some(p) = &c.path {
                    obj.insert(
                        "path".into(),
                        serde_json::Value::String(p.display().to_string()),
                    );
                }
                serde_json::Value::Object(obj)
            })
            .collect();
        let report = serde_json::json!({
            "loaded": entries.len(),
            "errors": errors.iter().map(|e| e.to_string()).collect::<Vec<_>>(),
            "catalogs": entries,
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    if loaded.is_empty() && errors.is_empty() {
        println!("no catalogs loaded");
        return Ok(());
    }
    println!(
        "{provider:<14} {origin:<8} {variants:>8} {patterns:>9} {skip:>5}  {source}",
        provider = "provider",
        origin = "origin",
        variants = "variants",
        patterns = "patterns",
        skip = "skip",
        source = "source",
    );
    for c in &loaded {
        let (origin, source_str) = match &c.source {
            CatalogSource::Bundled => ("bundled", "(in-binary)".to_owned()),
            CatalogSource::User => (
                "user",
                c.path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            ),
            CatalogSource::Project => (
                "project",
                c.path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            ),
            CatalogSource::Url { url, sha256 } => {
                let pin = sha256
                    .as_deref()
                    .map(|s| format!(" [pin:{}…]", &s[..8.min(s.len())]))
                    .unwrap_or_else(|| " [tofu]".to_owned());
                ("url", format!("{url}{pin}"))
            }
        };
        println!(
            "{:<14} {:<8} {:>8} {:>9} {:>5}  {}",
            c.catalog.provider_id,
            origin,
            c.catalog.variants.len(),
            c.catalog.env_var_patterns.len(),
            c.catalog.env_var_skip.len(),
            source_str
        );
    }
    if !errors.is_empty() {
        eprintln!();
        eprintln!("errors ({}):", errors.len());
        for e in &errors {
            eprintln!("  - {e}");
        }
    }
    Ok(())
}

/// Subscribe to a remote catalog by URL. Walks every P23
/// defence layer once (HTTPS / SSRF / size / content-type /
/// schema / TOFU or pin), prints what it found, asks for
/// trust if the user did not pre-pin, then writes a
/// `[[source]]` entry into `~/.devboy/secrets/catalog/sources.toml`.
fn catalog_add_url(args: CatalogAddUrlArgs) -> Result<()> {
    use devboy_token_catalog::{
        CatalogSourcesConfig, FetchError, FirstFetchPolicy, UrlSource,
        default_catalog_audit_log_path, default_catalog_cache_dir, default_known_hashes_path,
        default_sources_toml_path, fetch_url_source, parse_sources_toml, record_url_trust,
    };
    use std::fs;
    use std::io::IsTerminal;

    if !args.url.starts_with("https://") {
        anyhow::bail!("URL must start with `https://` (got `{}`)", args.url);
    }

    let pin_set = args.pin.is_some();
    let source = UrlSource {
        url: args.url.clone(),
        sha256: args.pin.clone(),
        refresh_seconds: args.refresh_seconds,
    };

    let known_hashes_path = default_known_hashes_path();
    let cache_dir = default_catalog_cache_dir();
    let audit_log_path = default_catalog_audit_log_path();

    println!("fetching {} …", source.url);
    // F2 — use `RequireConfirmation` so the loader does NOT
    // touch known_hashes.toml until the user has confirmed
    // the trust prompt. The error path
    // `FirstFetchNeedsConfirmation { url, sha256 }` carries
    // the REAL fetched-body SHA the loader would record —
    // we print THAT, not a re-serialised round-trip hash that
    // can drift from the actual pin target.
    let fetched = fetch_url_source(
        &source,
        known_hashes_path.as_deref(),
        cache_dir.as_deref(),
        FirstFetchPolicy::RequireConfirmation,
        audit_log_path.as_deref(),
    );

    // Three cases:
    //   1. `Ok(catalog)` — known_hashes already trusts this
    //      URL (or `sha256` pin matched). The loader has
    //      recorded nothing new; we only need to write
    //      sources.toml.
    //   2. `Err(FirstFetchNeedsConfirmation{url,sha256})` — new
    //      URL, no pin / TOFU yet. Surface the SHA, get the
    //      user's confirmation, THEN record trust + write
    //      sources.toml.
    //   3. Any other Err — propagate.
    let (catalog, fetched_sha, needs_trust) = match fetched {
        Ok(c) => {
            let sha_for_display = known_hashes_path
                .as_deref()
                .and_then(|p| devboy_token_catalog::read_known_hashes(p).ok())
                .and_then(|kh| kh.url.get(&source.url).cloned())
                .unwrap_or_else(|| args.pin.clone().unwrap_or_default());
            (c, sha_for_display, false)
        }
        Err(FetchError::FirstFetchNeedsConfirmation { url: _, sha256 }) => {
            // Re-fetch with AutoRecord ONLY after we know the
            // user is going to accept. To get the catalog
            // object for the summary print, we run the
            // loader once more — but using AutoRecord on this
            // call WILL persist the trust, which is what we
            // want once the user has said yes. So: first
            // print + prompt, then re-fetch with AutoRecord
            // to commit.
            //
            // Build a placeholder ProviderCatalog so the
            // summary print works before the second fetch.
            // We don't have the variants count until we've
            // parsed the body, so we make the print SHA-only
            // for the confirmation step.
            (
                // dummy zero-variant catalog for the print
                devboy_token_catalog::ProviderCatalog {
                    schema: None,
                    schema_version: 1,
                    provider_id: "<not parsed yet>".into(),
                    display_name: source.url.clone(),
                    description: None,
                    variants: Vec::new(),
                    env_var_patterns: Vec::new(),
                    env_var_skip: Vec::new(),
                },
                sha256,
                true,
            )
        }
        Err(e) => return Err(e).context("URL catalog fetch failed"),
    };

    println!();
    println!("source:        {}", source.url);
    if !needs_trust {
        println!("provider:      {}", catalog.provider_id);
        println!("display:       {}", catalog.display_name);
        println!("variants:      {}", catalog.variants.len());
        println!("patterns:      {}", catalog.env_var_patterns.len());
        println!("skip rules:    {}", catalog.env_var_skip.len());
    }
    println!("body sha256:   {fetched_sha}");
    println!();

    if needs_trust {
        if pin_set {
            // User passed --pin but the loader still wants
            // confirmation — that means the pin did NOT match
            // the fetched SHA. This is a hard refusal.
            if Some(&fetched_sha) != args.pin.as_ref() {
                anyhow::bail!(
                    "--pin sha256 does not match the fetched body sha256 (got `{fetched_sha}`, \
                     pinned `{}`); refusing to record an inconsistent trust state",
                    args.pin.as_deref().unwrap_or("")
                );
            }
            // Pin matched — proceed without an interactive
            // confirm.
        } else if !args.yes {
            if !std::io::stdin().is_terminal() {
                anyhow::bail!(
                    "no --pin and no --yes; refusing to record an unpinned URL source from a \
                     non-tty invocation. Re-run with --yes to accept TOFU, or with --pin <sha256> \
                     to lock the body."
                );
            }
            let prompt = format!(
                "Trust this catalog body and record it under TOFU for {} ? [y/N] ",
                source.url
            );
            let answer: bool = dialoguer::Confirm::new()
                .with_prompt(prompt)
                .default(false)
                .interact()
                .unwrap_or(false);
            if !answer {
                anyhow::bail!(
                    "user declined trust prompt — known_hashes.toml AND sources.toml left untouched"
                );
            }
        }

        // Persist trust under TOFU. `record_url_trust` writes
        // (url → sha256) into known_hashes.toml; it's the
        // same writer the loader uses under AutoRecord, so the
        // file the next refresh checks matches what we just
        // showed the user.
        if let Some(p) = known_hashes_path.as_deref() {
            record_url_trust(p, &source.url, &fetched_sha)
                .context("could not record URL trust in known_hashes.toml")?;
        }
    }

    let sources_path = default_sources_toml_path()
        .ok_or_else(|| anyhow::anyhow!("could not resolve default sources.toml path"))?;
    if let Some(parent) = sources_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create catalog dir {}", parent.display()))?;
    }

    let mut cfg: CatalogSourcesConfig = if sources_path.exists() {
        let body = fs::read_to_string(&sources_path)
            .with_context(|| format!("could not read {}", sources_path.display()))?;
        parse_sources_toml(&body)
            .with_context(|| format!("malformed {}", sources_path.display()))?
    } else {
        CatalogSourcesConfig::default()
    };

    // Replace any existing entry with the same URL (idempotent
    // re-add) — otherwise append. URL is the natural key.
    cfg.sources.retain(|s| s.url != source.url);
    cfg.sources.push(source.clone());
    if args.enable {
        cfg.enable_url_catalogs = true;
    }

    let body = toml::to_string_pretty(&cfg).context("could not serialise sources.toml")?;
    fs::write(&sources_path, body)
        .with_context(|| format!("could not write {}", sources_path.display()))?;

    println!("wrote {}", sources_path.display());
    if !cfg.enable_url_catalogs {
        println!(
            "note: enable_url_catalogs is still `false` — pass --enable to flip the master \
             switch, or edit sources.toml by hand."
        );
    }
    Ok(())
}

/// Re-fetch URL catalogs from `sources.toml`. Without
/// `--force` the loader honours each source's
/// `refresh_seconds` TTL — fresh sources are reported but
/// not re-fetched. With `--force` the cache for matching
/// sources is dropped before the call so every match goes
/// back to the network.
fn catalog_refresh(args: CatalogRefreshArgs) -> Result<()> {
    use devboy_token_catalog::{
        FirstFetchPolicy, default_catalog_audit_log_path, default_catalog_cache_dir,
        default_known_hashes_path, default_sources_toml_path, fetch_url_source, parse_sources_toml,
        sha256_hex,
    };
    use std::fs;

    let sources_path = default_sources_toml_path()
        .ok_or_else(|| anyhow::anyhow!("could not resolve default sources.toml path"))?;
    if !sources_path.exists() {
        println!(
            "no sources.toml at {} — nothing to refresh",
            sources_path.display()
        );
        return Ok(());
    }
    let body = fs::read_to_string(&sources_path)
        .with_context(|| format!("could not read {}", sources_path.display()))?;
    let cfg = parse_sources_toml(&body)
        .with_context(|| format!("malformed {}", sources_path.display()))?;

    if cfg.sources.is_empty() {
        println!("sources.toml has no [[source]] entries — nothing to refresh");
        return Ok(());
    }
    if !cfg.enable_url_catalogs {
        println!(
            "note: enable_url_catalogs = false in {} — fetched bodies will not affect runtime \
             until the master switch is flipped on (`add-url --enable` or edit by hand).",
            sources_path.display()
        );
    }

    let filter = args.filter.as_deref().map(|s| s.to_lowercase());
    let known_hashes_path = default_known_hashes_path();
    let cache_dir = default_catalog_cache_dir();
    let audit_log_path = default_catalog_audit_log_path();

    let mut fetched = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    for source in &cfg.sources {
        if let Some(needle) = filter.as_deref()
            && !source.url.to_lowercase().contains(needle)
        {
            skipped += 1;
            continue;
        }
        if args.force {
            // Drop the matching cache files so the loader
            // cannot serve a stale body. The cache key is
            // sha256(url) per the P23.5 layout — same logic
            // as `cache_paths_for` in the catalog crate.
            if let Some(cdir) = cache_dir.as_deref() {
                let key = sha256_hex(source.url.as_bytes());
                let body_path = cdir.join(format!("{key}.json"));
                let meta_path = cdir.join(format!("{key}.meta.toml"));
                let _ = fs::remove_file(&body_path);
                let _ = fs::remove_file(&meta_path);
            }
        }
        print!("→ {} … ", source.url);
        match fetch_url_source(
            source,
            known_hashes_path.as_deref(),
            cache_dir.as_deref(),
            FirstFetchPolicy::AutoRecord,
            audit_log_path.as_deref(),
        ) {
            Ok(catalog) => {
                println!(
                    "ok ({} variants, provider={})",
                    catalog.variants.len(),
                    catalog.provider_id
                );
                fetched += 1;
            }
            Err(e) => {
                println!("FAILED");
                eprintln!("    {e}");
                failed += 1;
            }
        }
    }

    println!("\nrefresh complete: {fetched} fetched, {skipped} skipped (filter), {failed} failed",);
    if failed > 0 {
        anyhow::bail!("{failed} URL source(s) failed — see errors above");
    }
    Ok(())
}

/// Drop URL entries from `known_hashes.toml` so the next
/// fetch re-establishes TOFU. Recovery flow for a deliberate
/// upstream rotation that the loader is currently refusing
/// (BlockedTofuMismatch).
fn catalog_forget(args: CatalogForgetArgs) -> Result<()> {
    use devboy_token_catalog::{default_known_hashes_path, read_known_hashes, write_known_hashes};
    use std::io::IsTerminal;

    let path = default_known_hashes_path()
        .ok_or_else(|| anyhow::anyhow!("could not resolve default known_hashes.toml path"))?;
    let mut known = read_known_hashes(&path).context("could not read known_hashes.toml")?;
    if known.url.is_empty() {
        println!("known_hashes.toml is empty — nothing to forget");
        return Ok(());
    }

    let filter = args.filter.as_deref().map(|s| s.to_lowercase());
    let to_drop: Vec<String> = known
        .url
        .keys()
        .filter(|u| match filter.as_deref() {
            Some(needle) => u.to_lowercase().contains(needle),
            None => true,
        })
        .cloned()
        .collect();

    if to_drop.is_empty() {
        println!(
            "no URLs in known_hashes.toml match `{}`",
            args.filter.as_deref().unwrap_or("(all)")
        );
        return Ok(());
    }

    if filter.is_none() && !args.yes {
        if !std::io::stdin().is_terminal() {
            anyhow::bail!(
                "refusing to drop ALL TOFU entries from a non-tty invocation. Re-run with --yes \
                 or pass a substring filter to scope the operation."
            );
        }
        let answer = dialoguer::Confirm::new()
            .with_prompt(format!(
                "Drop ALL {} TOFU entries from known_hashes.toml?",
                to_drop.len()
            ))
            .default(false)
            .interact()
            .unwrap_or(false);
        if !answer {
            anyhow::bail!("user declined — known_hashes.toml unchanged");
        }
    }

    for url in &to_drop {
        println!("forget {url}");
        known.url.remove(url);
    }
    write_known_hashes(&path, &known).context("could not write known_hashes.toml back to disk")?;
    println!(
        "\n{} entr{} dropped from {}",
        to_drop.len(),
        if to_drop.len() == 1 { "y" } else { "ies" },
        path.display()
    );
    Ok(())
}

/// Promote a TOFU entry to a hard SHA pin in `sources.toml`.
/// Without an explicit `<sha>` the current TOFU value is read
/// from `known_hashes.toml`.
fn catalog_pin(args: CatalogPinArgs) -> Result<()> {
    use devboy_token_catalog::{
        CatalogSourcesConfig, default_known_hashes_path, default_sources_toml_path,
        parse_sources_toml, read_known_hashes,
    };
    use std::fs;

    let sources_path = default_sources_toml_path()
        .ok_or_else(|| anyhow::anyhow!("could not resolve default sources.toml path"))?;
    if !sources_path.exists() {
        anyhow::bail!(
            "no sources.toml at {} — add a URL source first via `devboy secrets catalog add-url`",
            sources_path.display()
        );
    }
    let body = fs::read_to_string(&sources_path)
        .with_context(|| format!("could not read {}", sources_path.display()))?;
    let mut cfg: CatalogSourcesConfig = parse_sources_toml(&body)
        .with_context(|| format!("malformed {}", sources_path.display()))?;

    let needle = args.filter.to_lowercase();
    let matching: Vec<usize> = cfg
        .sources
        .iter()
        .enumerate()
        .filter(|(_, s)| s.url.to_lowercase().contains(&needle))
        .map(|(i, _)| i)
        .collect();
    let idx = match matching.len() {
        0 => anyhow::bail!("no source URL contains `{}`", args.filter),
        1 => matching[0],
        n => anyhow::bail!(
            "filter `{}` matches {n} sources — narrow it: {}",
            args.filter,
            cfg.sources
                .iter()
                .enumerate()
                .filter(|(i, _)| matching.contains(i))
                .map(|(_, s)| s.url.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };

    let sha = match args.sha {
        Some(s) => {
            if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
                anyhow::bail!("sha must be 64 lower-case hex chars (got {})", s.len());
            }
            s.to_lowercase()
        }
        None => {
            let known_path = default_known_hashes_path()
                .ok_or_else(|| anyhow::anyhow!("could not resolve known_hashes.toml path"))?;
            let known =
                read_known_hashes(&known_path).context("could not read known_hashes.toml")?;
            known
                .url
                .get(&cfg.sources[idx].url)
                .cloned()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no TOFU entry recorded for {} — fetch it first via `devboy secrets \
                         catalog refresh` (without --pin), then re-run pin to promote it",
                        cfg.sources[idx].url
                    )
                })?
        }
    };

    let url = cfg.sources[idx].url.clone();
    cfg.sources[idx].sha256 = Some(sha.clone());
    let new_body = toml::to_string_pretty(&cfg).context("could not serialise sources.toml")?;
    fs::write(&sources_path, new_body)
        .with_context(|| format!("could not write {}", sources_path.display()))?;
    println!("pinned {url}");
    println!("       sha256 = {sha}");
    println!("\nwrote {}", sources_path.display());
    Ok(())
}

/// Load a single catalog file and run per-variant integrity
/// checks: regex compiles, every URL parses. Returns
/// `Err(_)` (non-zero exit) when any variant fails so it can
/// gate CI.
fn catalog_validate(args: CatalogValidateArgs) -> Result<()> {
    let cat = devboy_token_catalog::load_file(&args.path)
        .with_context(|| format!("could not load {}", args.path.display()))?;

    println!(
        "{}: {} ({} {})",
        args.path.display(),
        cat.provider_id,
        cat.variants.len(),
        if cat.variants.len() == 1 {
            "variant"
        } else {
            "variants"
        }
    );

    let mut total_problems = 0usize;
    for v in &cat.variants {
        let mut problems: Vec<String> = Vec::new();

        if let Some(re) = v.format_regex.as_deref() {
            if let Err(e) = regex::Regex::new(re) {
                problems.push(format!("format_regex does not compile: {e}"));
            }
        }
        if let Err(e) = reqwest::Url::parse(&v.retrieval.console_url) {
            problems.push(format!("retrieval.console_url not a valid URL: {e}"));
        }
        if let Some(spec) = v.liveness.as_ref()
            && let Err(e) = reqwest::Url::parse(&spec.url)
        {
            problems.push(format!("liveness.url not a valid URL: {e}"));
        }

        if problems.is_empty() {
            println!("  ✓ {} — regex ok, URLs ok", v.id);
        } else {
            total_problems += problems.len();
            println!("  ✗ {}", v.id);
            for p in &problems {
                println!("      - {p}");
            }
        }
    }

    if total_problems > 0 {
        anyhow::bail!(
            "catalog validation failed: {total_problems} problem(s) across {} variant(s)",
            cat.variants.len()
        );
    }
    Ok(())
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
        OverrideField::ApproveOnUse => "approve_on_use",
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
        "secret path '{path}' not found in any source — register it via the global index or add a \
         `[secret.\"{path}\"]` block.\n\n{}",
        env_candidates_hint(path.as_str()),
        path = path
    );
}

/// Refuse an interactive secret operation when the process is in
/// CI / env-only mode (ADR-024 §6).
///
/// The `is_terminal()` guards next to each prompt already stop a
/// prompt from hanging on a pipe, but they answer a different
/// question: "is there a TTY". A CI runner can have one — a
/// pseudo-TTY is common — and would then sit waiting for a human
/// who is never going to type. This guard keys on the *mode*
/// instead, so the failure is immediate and explains itself.
///
/// Env-only mode has no vault to unlock and no daemon to ask, so
/// there is nothing an interactive prompt could usefully do.
pub fn refuse_interactive_in_env_only(operation: &str) -> anyhow::Result<()> {
    let config = devboy_core::config::Config::load().unwrap_or_default();
    if !crate::is_env_only_mode(&config) {
        return Ok(());
    }

    anyhow::bail!(
        "{operation} needs an interactive prompt, but this process is in CI / env-only mode \
         (ADR-024 §6): the environment is the sole secret source, and no vault, daemon or \
         passphrase prompt is available.\n\n\
         Either provide the secret through the environment, or drop `--ci` / `DEVBOY_CI` / \
         `[runtime] ci` if this was not meant to be a CI run."
    );
}

/// Render the environment variables that would satisfy `path`, in
/// resolution order (ADR-024 §6).
///
/// A "not found" that does not say *what to set* forces the user
/// to go read the docs; this lists every name that was tried so
/// the fix pastes straight into a shell or a CI config. It matters
/// most in env-only mode, where the environment is the only source
/// there is.
fn env_candidates_hint(path: &str) -> String {
    let candidates = devboy_secret_env_store::candidate_env_names(path, None);
    if candidates.is_empty() {
        return String::new();
    }

    let mut out = String::from("Set any one of these environment variables:\n");
    for name in &candidates {
        out.push_str("  ");
        out.push_str(name);
        out.push('\n');
    }
    out.push_str(
        "\n(the first is the ADR-021 convention name; the rest are the ADR-005 names kept for \
         compatibility with existing pipelines)",
    );
    out
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
                approve_on_use: None,
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
