//! `devboy secrets ui` — backend autodetection + launcher per
//! [ADR-023] §3.4.
//!
//! Selects between the ratatui (TUI) and egui (GUI) renderers
//! based on the runtime environment. The detection logic is
//! the explicit deliverable of P12.2:
//!
//! 1. `--tui` / `--gui` flags override everything.
//! 2. On Linux: `$DISPLAY` or `$WAYLAND_DISPLAY` set → GUI.
//! 3. On macOS / Windows: GUI by default (a windowing system
//!    is always available).
//! 4. Otherwise: TUI.
//!
//! ## Launcher behaviour
//!
//! - **TUI**: spins up a real ratatui event loop with the
//!   inventory view rendered into the alternate screen. `q` /
//!   `Esc` quits. Real keystrokes drive
//!   [`devboy_secrets_ui::InventoryState`]'s key handlers.
//! - **GUI**: spins up a real eframe-backed native window
//!   that hosts the egui inventory view from
//!   [`devboy_secrets_ui::gui::inventory`]. The window opens
//!   at 1024×640 with the title `devboy — secrets inventory`
//!   and pumps frames until the user closes it.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use std::io::{self, IsTerminal, Stdout};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use devboy_secrets_ui::{InventoryState, render_inventory};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

// =============================================================================
// Backend selection
// =============================================================================

/// The renderer the user will see. Returned by [`resolve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Tui,
    Gui,
}

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Gui => "gui",
        }
    }
}

/// User intent — the CLI flags map onto this enum, then
/// [`resolve`] folds it together with the environment to pick
/// a concrete [`Backend`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendChoice {
    /// No flag — autodetect.
    #[default]
    Auto,
    /// `--tui` — force the ratatui backend regardless of
    /// environment.
    ForceTui,
    /// `--gui` — force the egui backend regardless of
    /// environment.
    ForceGui,
}

/// Snapshot of the environment relevant to backend detection.
/// Pulled out as a struct so the detection logic stays a pure
/// function and tests can poke at any combination of values
/// without going through `std::env`.
#[derive(Debug, Clone)]
pub struct DetectionEnv {
    /// Linux: `$DISPLAY` (X11). `Some` non-empty → GUI.
    pub display: Option<String>,
    /// Linux: `$WAYLAND_DISPLAY`. `Some` non-empty → GUI.
    pub wayland_display: Option<String>,
    /// `cfg!(target_os)` snapshot. Pinned to the build target
    /// on real runs; tests can override.
    pub target_os: TargetOs,
    /// Whether stdout is a TTY. Required for the TUI backend —
    /// piping to a file with no terminal makes ratatui useless,
    /// so even when otherwise selected we'll error rather than
    /// scramble the user's pipe.
    pub stdout_is_tty: bool,
}

/// Coarse OS classification. We don't need the full
/// `std::env::consts::OS` matrix here — just "is a windowing
/// system definitely available?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetOs {
    Linux,
    MacOs,
    Windows,
    Other,
}

impl TargetOs {
    /// Snapshot the build target's OS. Tests build their own
    /// `DetectionEnv` and skip this.
    pub fn from_cfg() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

impl DetectionEnv {
    /// Snapshot the real environment. Reads `$DISPLAY` /
    /// `$WAYLAND_DISPLAY` from `std::env` (these are
    /// process-globals, but autodetection happens once at
    /// startup so the race window is theoretical).
    pub fn from_real_env() -> Self {
        Self {
            display: std::env::var("DISPLAY").ok().filter(|s| !s.is_empty()),
            wayland_display: std::env::var("WAYLAND_DISPLAY")
                .ok()
                .filter(|s| !s.is_empty()),
            target_os: TargetOs::from_cfg(),
            stdout_is_tty: io::stdout().is_terminal(),
        }
    }
}

/// Detect the preferred backend from the environment alone —
/// no override applied. Pure function over [`DetectionEnv`].
pub fn detect_preferred(env: &DetectionEnv) -> Backend {
    match env.target_os {
        TargetOs::Linux => {
            if env.display.is_some() || env.wayland_display.is_some() {
                Backend::Gui
            } else {
                Backend::Tui
            }
        }
        // macOS and Windows always have a windowing system —
        // GUI is the natural default.
        TargetOs::MacOs | TargetOs::Windows => Backend::Gui,
        // Anything else (BSD without X, exotic targets) — TUI is
        // the safe fallback.
        TargetOs::Other => Backend::Tui,
    }
}

/// Resolve user intent + environment into a concrete backend.
/// Errors when the user forced TUI in a non-TTY pipe, since
/// rendering ratatui into a pipe just scrambles the output.
pub fn resolve(choice: BackendChoice, env: &DetectionEnv) -> Result<Backend> {
    let chosen = match choice {
        BackendChoice::ForceTui => Backend::Tui,
        BackendChoice::ForceGui => Backend::Gui,
        BackendChoice::Auto => detect_preferred(env),
    };
    if chosen == Backend::Tui && !env.stdout_is_tty {
        anyhow::bail!(
            "TUI backend selected, but stdout is not a terminal. \
             Run interactively or pass `--gui` if a windowing system is available."
        );
    }
    Ok(chosen)
}

// =============================================================================
// CLI surface
// =============================================================================

/// Flags for `devboy secrets ui`.
#[derive(Args, Debug, Default)]
pub struct UiArgs {
    /// Force the terminal renderer (ratatui).
    #[arg(long, conflicts_with = "gui")]
    pub tui: bool,
    /// Force the windowed renderer (egui). Opens a native
    /// window via eframe; runs until the user closes it.
    #[arg(long)]
    pub gui: bool,
    /// Open the provision dialog focused on the given path.
    /// The window still opens with the full inventory in the
    /// background, but the dialog overlay is armed at startup
    /// — useful when the AI agent (or a script) wants to put
    /// the user one click away from filling a known-missing
    /// secret. Path must be valid ADR-020.
    #[arg(long, value_name = "PATH")]
    pub provision: Option<String>,
}

impl UiArgs {
    pub fn choice(&self) -> BackendChoice {
        if self.tui {
            BackendChoice::ForceTui
        } else if self.gui {
            BackendChoice::ForceGui
        } else {
            BackendChoice::Auto
        }
    }
}

/// Top-level handler. Resolves the backend and dispatches to
/// the matching launcher.
pub async fn handle(args: UiArgs) -> Result<()> {
    let env = DetectionEnv::from_real_env();
    let backend = resolve(args.choice(), &env)?;
    eprintln!("devboy secrets ui: backend = {}", backend.label());
    match backend {
        Backend::Tui => launch_tui(),
        Backend::Gui => launch_gui(args.provision.as_deref()),
    }
}

// =============================================================================
// Launchers
// =============================================================================

fn launch_tui() -> Result<()> {
    let mut terminal = setup_terminal().context("could not enter raw mode / alternate screen")?;
    let result = run_tui_loop(&mut terminal);
    restore_terminal(&mut terminal).context("could not restore terminal")?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("enable_raw_mode failed")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("EnterAlternateScreen failed")?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).context("ratatui Terminal::new failed")
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .context("LeaveAlternateScreen failed")?;
    disable_raw_mode().context("disable_raw_mode failed")?;
    terminal.show_cursor().context("show_cursor failed")?;
    Ok(())
}

fn run_tui_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    // P12.2's deliverable is the launcher — the rows themselves
    // come from the orchestration layer wired up in P14+
    // (`secrets.list` MCP + the daemon's manifest snapshot). For
    // now the inventory view starts empty and the user can
    // confirm the renderer is alive.
    let mut state = InventoryState::new(Vec::new());

    loop {
        terminal.draw(|f| {
            let area = f.area();
            render_inventory(f, &state, area);
        })?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Up | KeyCode::Char('k') => state.move_up(),
                    KeyCode::Down | KeyCode::Char('j') => state.move_down(),
                    KeyCode::Tab => state.cycle_focus_forward(),
                    KeyCode::BackTab => state.cycle_focus_backward(),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn launch_gui(provision_path: Option<&str>) -> Result<()> {
    let initial_path = provision_path.map(str::to_owned);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1024.0, 640.0])
            .with_title("devboy — secrets inventory"),
        ..Default::default()
    };
    eframe::run_native(
        "devboy-secrets",
        options,
        Box::new(move |_cc| Ok(Box::new(InventoryApp::new_with_initial(initial_path)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe failed to run native window: {e}"))
}

/// `eframe::App` shell that:
///
/// 1. Loads the global index + project manifest at startup and
///    populates the inventory view-model with one row per
///    declared path.
/// 2. Renders the inventory list in the central area; clicking
///    a row arms the provision dialog with the row's metadata.
/// 3. When armed, shows the provision dialog as a modal
///    `egui::Window` overlay. On `Save`, writes the value
///    straight to the OS keychain via `KeychainStore` and
///    refreshes the row's status.
///
/// `inventory_rows_for(...)` is the orchestration glue — pure
/// data + no `egui` types so it's testable on its own.
struct InventoryApp {
    state: devboy_secrets_ui::InventoryState,
    metadata_by_path: std::collections::HashMap<String, devboy_storage::IndexEntry>,
    dialog: Option<devboy_secrets_ui::DialogState>,
    dialog_path: Option<String>,
    last_save_error: Option<String>,
    /// Selected backend (keychain or local-vault), picked at
    /// app construction from env vars.
    backend: StorageBackend,
    /// Recovery phrase to surface once after first vault create.
    /// `None` until a vault is created, then `Some` for the
    /// rest of the session — the user must save it somewhere
    /// before closing the window.
    recovery_phrase_to_show: Option<String>,
    /// Toggle for the «show entered value» switch above the
    /// provision-dialog hidden input. Off by default — only
    /// flip on when the user wants to verify what they typed.
    reveal_value: bool,
    /// `id` of the currently selected token variant, when the
    /// dialog's path resolves to a multi-variant provider.
    /// `None` when the provider has only one variant (auto-
    /// resolved) or no catalog match at all. Driven by the
    /// radio-button picker rendered above the context card.
    selected_variant_id: Option<String>,
    /// Backend-driven token catalogs loaded at startup.
    /// Sources: bundled (compiled in), user
    /// (`~/.devboy/secrets/catalog/`), project
    /// (`<cwd>/.devboy/secrets/catalog/`). Drives the variant
    /// picker, retrieval steps, and on-Save liveness probes
    /// in subsequent P20.x tasks.
    catalogs: Vec<devboy_token_catalog::LoadedCatalog>,
    /// Per-file errors from catalog loading. Empty on the
    /// happy path; surfaced as a banner above inventory when
    /// non-empty so authors know which file is broken.
    catalog_errors: Vec<devboy_token_catalog::CatalogError>,
}

impl InventoryApp {
    fn new_with_initial(initial_path: Option<String>) -> Self {
        let backend = StorageBackend::detect_from_env();
        let (rows, metadata_by_path) = load_inventory_or_empty(&backend);
        let (catalogs, catalog_errors) = load_token_catalogs();
        let mut app = Self {
            state: devboy_secrets_ui::InventoryState::new(rows),
            metadata_by_path,
            dialog: None,
            dialog_path: None,
            last_save_error: None,
            backend,
            recovery_phrase_to_show: None,
            reveal_value: false,
            selected_variant_id: None,
            catalogs,
            catalog_errors,
        };
        if let Some(path) = initial_path {
            app.open_dialog_for(&path);
        }
        app
    }

    fn reload(&mut self) {
        let (rows, metadata) = load_inventory_or_empty(&self.backend);
        self.state.replace_rows(rows);
        self.metadata_by_path = metadata;
    }
}

/// Which backend the GUI should write secrets to.
///
/// Picked at app construction time and held for the lifetime of
/// the window. The user can switch by closing the window and
/// flipping the env var.
#[derive(Debug, Clone)]
enum StorageBackend {
    /// Default — OS keychain via `devboy_storage::KeychainStore`.
    Keychain,
    /// Local-vault file at `vault_path`, unlocked with
    /// `passphrase`. Selected when `DEVBOY_VAULT_PASSPHRASE` is
    /// set; the file is created on first save if it doesn't exist.
    LocalVault {
        vault_path: std::path::PathBuf,
        passphrase: secrecy::SecretString,
    },
}

impl StorageBackend {
    fn detect_from_env() -> Self {
        if let Ok(pass) = std::env::var("DEVBOY_VAULT_PASSPHRASE")
            && !pass.is_empty()
        {
            let vault_path = std::env::var("DEVBOY_VAULT_PATH")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|_| {
                    let mut p = dirs::config_dir().unwrap_or_else(std::env::temp_dir);
                    p.push("devboy-tools");
                    p.push("secrets");
                    p.push("local-vault.dvb");
                    p
                });
            return StorageBackend::LocalVault {
                vault_path,
                passphrase: secrecy::SecretString::new(pass.into()),
            };
        }
        StorageBackend::Keychain
    }

    fn label(&self) -> String {
        match self {
            StorageBackend::Keychain => {
                "macOS Keychain — service `devboy-tools`, account = path".to_owned()
            }
            StorageBackend::LocalVault { vault_path, .. } => {
                format!(
                    "local-vault — file `{}`, XChaCha20-Poly1305 + Argon2id passphrase",
                    vault_path.display()
                )
            }
        }
    }

    fn source_label(&self) -> &'static str {
        match self {
            StorageBackend::Keychain => "default-keychain",
            StorageBackend::LocalVault { .. } => "local-vault",
        }
    }

    /// Probe whether `path` already has a value. Errors collapse
    /// to "not provisioned" to keep the inventory render simple.
    fn has_value(&self, path: &str) -> bool {
        match self {
            StorageBackend::Keychain => {
                let store = devboy_storage::KeychainStore::new();
                matches!(
                    devboy_storage::CredentialStore::get(&store, path),
                    Ok(Some(_))
                )
            }
            StorageBackend::LocalVault {
                vault_path,
                passphrase,
            } => {
                if !vault_path.exists() {
                    return false;
                }
                let unlock = devboy_vault_crypto::UnlockMethod::Passphrase(passphrase.clone());
                let Ok(vault) = devboy_vault_crypto::Vault::open(vault_path, unlock) else {
                    return false;
                };
                matches!(vault.get(path), Ok(Some(_)))
            }
        }
    }

    /// Write `value` to the backend. Creates a vault file if it
    /// doesn't exist (LocalVault). Returns the optional recovery
    /// phrase the user must save (only on first vault create).
    fn store(&self, path: &str, value: &secrecy::SecretString) -> Result<Option<String>, String> {
        match self {
            StorageBackend::Keychain => {
                let store = devboy_storage::KeychainStore::new();
                devboy_storage::CredentialStore::store(&store, path, value)
                    .map_err(|e| format!("{e}"))?;
                Ok(None)
            }
            StorageBackend::LocalVault {
                vault_path,
                passphrase,
            } => {
                use devboy_vault_crypto::{EntryMetadata, InitialUnlock, UnlockMethod, Vault};
                if let Some(parent) = vault_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let recovery = if vault_path.exists() {
                    let mut vault =
                        Vault::open(vault_path, UnlockMethod::Passphrase(passphrase.clone()))
                            .map_err(|e| format!("vault open: {e}"))?;
                    vault
                        .put(path, value, EntryMetadata::default())
                        .map_err(|e| format!("vault put: {e}"))?;
                    None
                } else {
                    let outcome = Vault::create(
                        vault_path,
                        InitialUnlock {
                            passphrase: passphrase.clone(),
                            with_recovery: true,
                            with_keychain_account: None,
                            passphrase_params: None,
                        },
                    )
                    .map_err(|e| format!("vault create: {e}"))?;
                    let mut vault = outcome.vault;
                    vault
                        .put(path, value, EntryMetadata::default())
                        .map_err(|e| format!("vault put: {e}"))?;
                    outcome.recovery_phrase.map(|p| p.expose_words().to_owned())
                };
                Ok(recovery)
            }
        }
    }
}

/// Load token catalogs from every configured source and merge them.
///
/// Sources walked, least-to-most specific: bundled (compiled in),
/// user (`~/.devboy/secrets/catalog/`), project
/// (`<cwd>/.devboy/secrets/catalog/`). Later sources override earlier
/// ones on `provider_id` collision — see
/// `devboy_token_catalog::load_all` for the full precedence rule.
fn load_token_catalogs() -> (
    Vec<devboy_token_catalog::LoadedCatalog>,
    Vec<devboy_token_catalog::CatalogError>,
) {
    let bundled = devboy_token_catalog::bundled_catalogs();
    let user_dir = devboy_token_catalog::default_user_catalog_dir();
    let project_dir = std::env::current_dir()
        .ok()
        .map(|cwd| devboy_token_catalog::default_project_catalog_dir(&cwd));
    devboy_token_catalog::load_all(&bundled, user_dir.as_deref(), project_dir.as_deref())
}

/// Walk global index + project manifest in CWD, merge them, and
/// return inventory rows ready for `InventoryState::replace_rows`
/// plus a `path → IndexEntry` map the dialog uses to populate
/// its metadata fields.
fn load_inventory_or_empty(
    backend: &StorageBackend,
) -> (
    Vec<devboy_secrets_ui::InventoryRow>,
    std::collections::HashMap<String, devboy_storage::IndexEntry>,
) {
    use devboy_storage::{GlobalIndex, ProjectManifest, merge_manifest};

    let Ok(index) = GlobalIndex::load() else {
        return (Vec::new(), std::collections::HashMap::new());
    };
    let Ok(manifest) = ProjectManifest::load() else {
        return (Vec::new(), std::collections::HashMap::new());
    };
    let Ok(merged) = merge_manifest(&index, &manifest) else {
        return (Vec::new(), std::collections::HashMap::new());
    };

    let mut rows = Vec::with_capacity(merged.secrets.len());
    let mut metadata = std::collections::HashMap::with_capacity(merged.secrets.len());
    for resolved in merged.secrets.values() {
        let path_str = resolved.path.to_string();
        let provisioned = backend.has_value(&path_str);
        let status = if provisioned {
            devboy_secrets_ui::RowStatus::Provisioned
        } else {
            devboy_secrets_ui::RowStatus::Missing
        };
        rows.push(devboy_secrets_ui::InventoryRow {
            path: path_str.clone(),
            status,
            routed_source: provisioned.then(|| backend.source_label().to_owned()),
            expires_at: resolved.metadata.expires_at.clone(),
            provider: resolved.path.as_str().split('/').nth(1).map(str::to_owned),
            scope: resolved
                .path
                .as_str()
                .split('/')
                .next()
                .unwrap_or("")
                .to_owned(),
        });
        metadata.insert(path_str, resolved.metadata.clone());
    }
    (rows, metadata)
}

impl eframe::App for InventoryApp {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        // Backend banner — tells the user where saves go.
        ui.label(
            eframe::egui::RichText::new(format!("Backend: {}", self.backend.label()))
                .small()
                .color(eframe::egui::Color32::from_rgb(0x55, 0xaa, 0xff)),
        );
        ui.separator();

        // Recovery phrase — surfaced exactly once after first
        // vault create. The user must save this somewhere; we
        // do not show it again across runs.
        if let Some(phrase) = &self.recovery_phrase_to_show {
            ui.label(
                eframe::egui::RichText::new("⚠ Save your recovery phrase")
                    .strong()
                    .color(eframe::egui::Color32::from_rgb(0xcc, 0xa0, 0x33)),
            );
            ui.label(
                eframe::egui::RichText::new(phrase)
                    .monospace()
                    .background_color(eframe::egui::Color32::from_rgb(0x33, 0x33, 0x33)),
            );
            ui.label(
                eframe::egui::RichText::new(
                    "Without this phrase you cannot recover the vault if you forget the passphrase.",
                )
                .small()
                .italics(),
            );
            ui.separator();
        }

        if let Some(err) = &self.last_save_error {
            ui.colored_label(eframe::egui::Color32::from_rgb(0xcc, 0x44, 0x44), err);
            ui.separator();
        }

        // Track which row was selected before render so we can
        // detect a click → arm the dialog.
        let prev_selected = self.state.selected();
        devboy_secrets_ui::gui::inventory::render(ui, &mut self.state);

        // Top buttons: open dialog for selected row, refresh.
        ui.horizontal(|ui| {
            let has_row = self.state.selected_row().is_some();
            if ui
                .add_enabled(
                    has_row,
                    eframe::egui::Button::new("Add / update value for selected"),
                )
                .clicked()
                && let Some(row) = self.state.selected_row()
            {
                self.open_dialog_for(&row.path);
            }
            if ui.button("Reload from manifest").clicked() {
                self.reload();
            }
            // Surface the catalog count so the operator knows
            // how many providers + errors are loaded. The
            // variant-picker widget and per-variant rendering
            // land in P20.2+; for P20.1 this is the only proof
            // the data path actually works.
            ui.label(
                eframe::egui::RichText::new(format!(
                    "catalogs: {} loaded, {} error(s)",
                    self.catalogs.len(),
                    self.catalog_errors.len()
                ))
                .small(),
            );
        });

        // Detect click that changed selection → auto-open dialog
        // (UX convenience — same effect as the explicit button).
        if self.state.selected() != prev_selected
            && let Some(row) = self.state.selected_row()
        {
            self.open_dialog_for(&row.path);
        }

        // Dialog overlay.
        if self.dialog.is_some() {
            let mut close = false;
            let mut submitted_value = None;
            let mut submitted_path = None;
            // Render the dialog inside an egui::Window. The
            // window has two stacked sections:
            //   1. Context card  — full description, retrieval
            //      URL as a clickable hyperlink, env_var alias,
            //      pattern hint, expiry & rotation reminders.
            //      Pulled from the index entry the dialog was
            //      opened against — gives the user enough info
            //      to know WHAT to fill and WHERE to get it.
            //   2. Provision form — the existing
            //      `gui::provision_dialog` widget (path, hidden
            //      input, save / cancel).
            let dialog_path = self.dialog_path.clone().unwrap_or_default();
            let entry = self.metadata_by_path.get(&dialog_path).cloned();
            eframe::egui::Window::new(format!("Provision: {dialog_path}"))
                .collapsible(false)
                .resizable(true)
                .default_width(560.0)
                .show(ui.ctx(), |ui| {
                    // Variant picker — only visible when the
                    // active path resolves to a catalog provider
                    // with more than one variant. Single-variant
                    // providers skip the picker entirely (we
                    // already pre-selected the only choice in
                    // open_dialog_for).
                    if let Some((loaded, _)) = self.current_provider_and_variant()
                        && loaded.catalog.variants.len() > 1
                    {
                        ui.horizontal(|ui| {
                            ui.label(
                                eframe::egui::RichText::new(format!(
                                    "{} — pick a variant:",
                                    loaded.catalog.display_name
                                ))
                                .strong(),
                            );
                            ui.label(catalog_source_chip(loaded.source));
                        });
                        let variants: Vec<(String, String)> = loaded
                            .catalog
                            .variants
                            .iter()
                            .map(|v| (v.id.clone(), v.display_name.clone()))
                            .collect();
                        for (vid, vname) in variants {
                            let selected = self
                                .selected_variant_id
                                .as_deref()
                                .map(|s| s == vid)
                                .unwrap_or(false);
                            if ui.radio(selected, vname).clicked() {
                                self.selected_variant_id = Some(vid);
                            }
                        }
                        ui.separator();
                    }

                    let variant_for_card = self
                        .current_provider_and_variant()
                        .map(|(loaded, v)| (v, loaded.source));
                    render_context_card(
                        ui,
                        &dialog_path,
                        entry.as_ref(),
                        &self.backend,
                        variant_for_card,
                    );
                    ui.separator();

                    // Reveal-toggle for the value below. Echoes
                    // chars in plaintext when on. Off by default
                    // — turn on only when you need to spot-check
                    // what you typed before saving.
                    let mut reveal = self.reveal_value;
                    if ui
                        .checkbox(&mut reveal, "Show entered value (off = bullets)")
                        .clicked()
                    {
                        self.reveal_value = reveal;
                    }
                    if self.reveal_value
                        && let Some(d) = self.dialog.as_ref()
                    {
                        let val = d.value_clone_for_edit();
                        ui.label(
                            eframe::egui::RichText::new(format!("current value: «{val}»"))
                                .monospace()
                                .background_color(eframe::egui::Color32::from_rgb(
                                    0x33, 0x33, 0x33,
                                )),
                        );
                    }

                    // Live regex feedback — visible to the user
                    // while they're typing. Resolution order:
                    //   variant.format_regex (catalog override),
                    //   entry.format_regex (manifest-inline),
                    //   pattern_id → rust catalogue.
                    let variant_format_regex: Option<String> = self
                        .current_provider_and_variant()
                        .and_then(|(_, v)| v.format_regex.clone());
                    if let Some(d) = self.dialog.as_ref() {
                        let val = d.value_clone_for_edit();
                        let pattern = variant_format_regex.clone().or_else(|| {
                            entry.as_ref().and_then(|e| {
                                if let Some(re) = e.format_regex.as_deref() {
                                    Some(re.to_owned())
                                } else if let Some(pid) = e.pattern_id.as_deref() {
                                    let cat = devboy_secret_patterns::Catalogue::builtins_only();
                                    cat.find(pid).map(|p| p.format_regex().as_str().to_owned())
                                } else {
                                    None
                                }
                            })
                        });
                        match pattern {
                            Some(re) if !val.is_empty() => match regex::Regex::new(&re) {
                                Ok(compiled) => {
                                    if compiled.is_match(&val) {
                                        ui.colored_label(
                                            eframe::egui::Color32::from_rgb(0x55, 0xaa, 0x55),
                                            format!("✓ matches /{re}/"),
                                        );
                                    } else {
                                        ui.colored_label(
                                            eframe::egui::Color32::from_rgb(0xcc, 0x44, 0x44),
                                            format!("✗ mismatch — expected /{re}/"),
                                        );
                                    }
                                }
                                Err(e) => {
                                    ui.colored_label(
                                        eframe::egui::Color32::from_rgb(0xcc, 0xa0, 0x33),
                                        format!("regex error: {e}"),
                                    );
                                }
                            },
                            Some(re) => {
                                ui.label(
                                    eframe::egui::RichText::new(format!("expected format: /{re}/"))
                                        .small()
                                        .italics(),
                                );
                            }
                            None => {
                                ui.label(
                                    eframe::egui::RichText::new(
                                        "no format rule declared for this path \
                                         (any value will pass format check)",
                                    )
                                    .small()
                                    .italics(),
                                );
                            }
                        }
                    }
                    ui.separator();

                    let dialog = self.dialog.as_mut().unwrap();
                    let result = devboy_secrets_ui::gui::provision_dialog::render(ui, dialog);
                    if let Some(submission) = result.submission {
                        submitted_value = Some(submission.value);
                        submitted_path = Some(submission.path);
                    } else if result.cancelled {
                        close = true;
                    } else if result.open_url_clicked
                        && let Some(url) = dialog.metadata().provisioning_url.clone()
                    {
                        // Spawn the OS browser without blocking.
                        let _ = std::process::Command::new(if cfg!(target_os = "macos") {
                            "open"
                        } else if cfg!(target_os = "windows") {
                            "start"
                        } else {
                            "xdg-open"
                        })
                        .arg(url)
                        .spawn();
                    }
                });

            if let (Some(value), Some(path)) = (submitted_value, submitted_path) {
                use secrecy::ExposeSecret;
                let entry = self.metadata_by_path.get(&path).cloned();
                // Snapshot the variant slot up-front: format
                // regex AND liveness spec. The catalog wins over
                // the rust pattern catalogue when both apply.
                let (variant_regex, variant_liveness) = self
                    .current_provider_and_variant()
                    .map(|(_, v)| (v.format_regex.clone(), v.liveness.clone()))
                    .unwrap_or((None, None));

                // Stage 1 — format validation. Catalog regex
                // wins; otherwise reuse `validate_format` (the
                // same path `secrets validate` walks on CI).
                let format_problem: Option<String> = if let Some(re_str) = variant_regex.as_deref()
                {
                    match regex::Regex::new(re_str) {
                        Ok(re) if !re.is_match(value.expose_secret()) => Some(format!(
                            "value does not match the catalog format (`{re_str}`)"
                        )),
                        Ok(_) => None,
                        Err(e) => Some(format!("catalog format rule could not be compiled: {e}")),
                    }
                } else {
                    let format_check = entry.as_ref().map(|e| {
                        let catalogue = devboy_secret_patterns::Catalogue::builtins_only();
                        devboy_storage::validate_format(e, value.expose_secret(), &catalogue)
                    });
                    match &format_check {
                        Some(devboy_storage::FormatCheck::Mismatch { source, expected }) => {
                            Some(format!(
                                "value does not match the declared format ({source:?}, expected `{expected}`)"
                            ))
                        }
                        Some(devboy_storage::FormatCheck::Error { message }) => {
                            Some(format!("format rule could not be evaluated: {message}"))
                        }
                        _ => None,
                    }
                };

                if let Some(reason) = format_problem {
                    self.last_save_error = Some(format!("rejected before write: {reason}"));
                    if let Some(d) = self.dialog.as_mut() {
                        d.apply_status(devboy_secrets_ui::DialogStatus::ValidationFailed {
                            reason,
                        });
                    }
                } else if let Some(reason) =
                    liveness_probe(entry.as_ref(), variant_liveness.as_ref(), &value).err()
                {
                    // Stage 2 — actually call the provider's
                    // endpoint (when the pattern declares one).
                    // Synchronous blocking probe — UI hangs for
                    // up to 5s. The trade-off: we never let a
                    // dead token land in the vault.
                    self.last_save_error = Some(format!("liveness probe failed: {reason}"));
                    if let Some(d) = self.dialog.as_mut() {
                        d.apply_status(devboy_secrets_ui::DialogStatus::ValidationFailed {
                            reason,
                        });
                    }
                } else {
                    // Stage 3 — write through the selected
                    // backend (keychain or local-vault).
                    match self.backend.store(
                        &path,
                        &secrecy::SecretString::new(value.expose_secret().to_string().into()),
                    ) {
                        Ok(maybe_recovery) => {
                            self.last_save_error = None;
                            if let Some(d) = self.dialog.as_mut() {
                                d.apply_status(devboy_secrets_ui::DialogStatus::Saved);
                            }
                            if let Some(phrase) = maybe_recovery {
                                self.recovery_phrase_to_show = Some(phrase);
                            }
                            close = true;
                            self.reload();
                        }
                        Err(e) => {
                            self.last_save_error = Some(format!("backend write failed: {e}"));
                            if let Some(d) = self.dialog.as_mut() {
                                d.apply_status(devboy_secrets_ui::DialogStatus::ValidationFailed {
                                    reason: format!("backend: {e}"),
                                });
                            }
                        }
                    }
                }
            }
            if close {
                self.dialog = None;
                self.dialog_path = None;
            }
        }
    }
}

/// Render the "what is this secret + where to take it from"
/// card above the provision form. Fed straight from the merged
/// `IndexEntry` so the user sees everything the manifest knows
/// about the path before they have to type anything.
/// Try to call the provider's liveness endpoint with the
/// given value. Resolution order:
///
/// 1. `catalog_liveness` (the variant's JSON-declared probe) —
///    catalog wins because the user explicitly picked a variant.
/// 2. `entry.pattern_id` → rust catalogue's `LivenessSpec`.
///
/// Returns `Ok(())` when no probe is declared (nothing to do),
/// when the probe returned the expected HTTP status, or when
/// neither resolution path yields a spec. Returns `Err(reason)`
/// when the probe ran and the upstream rejected the value, or
/// when the network call itself failed — we'd rather block save
/// on a transient than silently land a dead token.
fn liveness_probe(
    entry: Option<&devboy_storage::IndexEntry>,
    catalog_liveness: Option<&devboy_token_catalog::LivenessSpec>,
    value: &secrecy::SecretString,
) -> Result<(), String> {
    use devboy_secret_patterns::{HttpMethod, LivenessAuth, LivenessKind};
    use secrecy::ExposeSecret;

    // Catalog override path — variant's JSON-declared probe.
    if let Some(spec) = catalog_liveness {
        return run_catalog_liveness(spec, value);
    }

    let Some(entry) = entry else { return Ok(()) };
    let Some(pid) = entry.pattern_id.as_deref() else {
        return Ok(());
    };
    let cat = devboy_secret_patterns::Catalogue::builtins_only();
    let Some(pattern) = cat.find(pid) else {
        return Ok(());
    };
    let Some(spec) = pattern.liveness() else {
        return Ok(());
    };
    let LivenessKind::Http {
        url,
        method,
        auth,
        expect_status,
    } = &spec.kind;

    // Blocking client — we run inside an egui frame so async
    // wouldn't help anyway. 5-second timeout.
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => return Err(format!("could not build HTTP client: {e}")),
    };
    let mut req = match method {
        HttpMethod::Get => client.get(*url),
        HttpMethod::Post => client.post(*url),
        HttpMethod::Head => client.head(*url),
    };
    let raw = value.expose_secret();
    req = match auth {
        LivenessAuth::Bearer => req.bearer_auth(raw),
        LivenessAuth::BasicUser => req.basic_auth(raw, None::<&str>),
        LivenessAuth::BasicPassword => req.basic_auth("", Some(raw)),
        LivenessAuth::Header { name } => req.header(*name, raw),
    };
    let resp = req.send().map_err(|e| format!("network: {e}"))?;
    let status = resp.status();
    if status.as_u16() == *expect_status {
        Ok(())
    } else {
        Err(format!(
            "upstream returned HTTP {status} (expected {expect_status})"
        ))
    }
}

/// Run an HTTP liveness probe defined in a `devboy-token-catalog`
/// JSON entry. Mirrors the rust-catalogue path in
/// [`liveness_probe`] but reads its config from the
/// string-typed JSON shape instead.
fn run_catalog_liveness(
    spec: &devboy_token_catalog::LivenessSpec,
    value: &secrecy::SecretString,
) -> Result<(), String> {
    use secrecy::ExposeSecret;
    if spec.kind != "http" {
        return Err(format!("unsupported liveness kind: {}", spec.kind));
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("could not build HTTP client: {e}"))?;
    let mut req = match spec.method.to_ascii_uppercase().as_str() {
        "GET" => client.get(&spec.url),
        "POST" => client.post(&spec.url),
        "HEAD" => client.head(&spec.url),
        m => return Err(format!("unsupported HTTP method: {m}")),
    };
    let raw = value.expose_secret();
    req = match &spec.auth {
        devboy_token_catalog::AuthSpec::Bearer => req.bearer_auth(raw),
        devboy_token_catalog::AuthSpec::BasicUser => req.basic_auth(raw, None::<&str>),
        devboy_token_catalog::AuthSpec::BasicPassword => req.basic_auth("", Some(raw)),
        devboy_token_catalog::AuthSpec::Header { name } => req.header(name.as_str(), raw),
    };
    let resp = req.send().map_err(|e| format!("network: {e}"))?;
    let status = resp.status();
    if status.as_u16() == spec.expect_status {
        Ok(())
    } else {
        Err(format!(
            "upstream returned HTTP {} (expected {})",
            status, spec.expect_status
        ))
    }
}

/// Render-ready chip indicating where a catalog entry came
/// from. Used both in the multi-variant picker title and in
/// the context card so a team override (project-scope JSON
/// shadowing the bundled default) is visible at a glance.
fn catalog_source_chip(source: devboy_token_catalog::CatalogSource) -> eframe::egui::RichText {
    use devboy_token_catalog::CatalogSource;
    use eframe::egui::{Color32, RichText};
    let (label, color) = match source {
        CatalogSource::Bundled => ("bundled", Color32::from_rgb(0x99, 0x99, 0x99)),
        CatalogSource::User => ("user", Color32::from_rgb(0x55, 0xa0, 0xcc)),
        CatalogSource::Project => ("project", Color32::from_rgb(0x55, 0xaa, 0x55)),
    };
    RichText::new(format!("[{label}]")).small().color(color)
}

fn render_context_card(
    ui: &mut eframe::egui::Ui,
    path: &str,
    entry: Option<&devboy_storage::IndexEntry>,
    backend: &StorageBackend,
    variant: Option<(
        &devboy_token_catalog::TokenVariant,
        devboy_token_catalog::CatalogSource,
    )>,
) {
    use eframe::egui::{Color32, RichText};

    ui.heading("How to fill this secret");
    ui.add_space(4.0);

    // Variant block — when the active path resolved to a
    // catalog variant, render the source-origin chip followed
    // by `description`, the `console_url` hyperlink, the
    // `retrieval.steps` as a numbered procedure, and finally
    // `retrieval.notes` (if any) as a small italic footnote.
    // The grid below skips rows it would duplicate
    // (Description, Where to take from) when a variant is
    // present.
    if let Some((v, source)) = variant {
        ui.horizontal(|ui| {
            ui.label(RichText::new(&v.display_name).strong());
            ui.label(catalog_source_chip(source));
        });
        ui.label(&v.description);
        ui.add_space(2.0);
        ui.hyperlink_to("Where to take from →", &v.retrieval.console_url);
        ui.add_space(4.0);
        for (idx, step) in v.retrieval.steps.iter().enumerate() {
            ui.label(format!("{}. {}", idx + 1, step));
        }
        if let Some(notes) = v.retrieval.notes.as_deref() {
            ui.add_space(4.0);
            ui.label(RichText::new(notes).small().italics());
        }
        ui.add_space(6.0);
    }

    eframe::egui::Grid::new(format!("ctx-grid-{path}"))
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(RichText::new("Path").strong());
            ui.label(RichText::new(path).monospace());
            ui.end_row();

            if let Some(e) = entry {
                if variant.is_none()
                    && let Some(desc) = e.description.as_deref()
                {
                    ui.label(RichText::new("Description").strong());
                    ui.label(desc);
                    ui.end_row();
                }
                if variant.is_none()
                    && let Some(url) = e.retrieval_url.as_deref()
                {
                    ui.label(RichText::new("Where to take from").strong());
                    ui.hyperlink(url);
                    ui.end_row();
                }
                if let Some(env_var) = e.env_var.as_deref() {
                    ui.label(RichText::new("Env var alias").strong());
                    ui.label(RichText::new(env_var).monospace());
                    ui.end_row();
                }
                if let Some(pat) = e.pattern_id.as_deref() {
                    ui.label(RichText::new("Pattern").strong());
                    ui.label(RichText::new(pat).monospace());
                    ui.end_row();
                }
                if let Some(method) = e.rotation_method {
                    ui.label(RichText::new("Rotation").strong());
                    ui.label(format!(
                        "{method:?} ({} days)",
                        e.rotate_every_days.unwrap_or(0)
                    ));
                    ui.end_row();
                }
                if let Some(last) = e.last_rotated_at.as_deref() {
                    ui.label(RichText::new("Last rotated").strong());
                    ui.label(last);
                    ui.end_row();
                }
                if let Some(exp) = e.expires_at.as_deref() {
                    ui.label(RichText::new("Expires at").strong());
                    ui.label(exp);
                    ui.end_row();
                }
            } else {
                ui.label(RichText::new("Note").strong());
                ui.label(
                    RichText::new(
                        "no metadata for this path in the global index — \
                         only the manifest declared it",
                    )
                    .color(Color32::from_rgb(0xcc, 0xa0, 0x33)),
                );
                ui.end_row();
            }

            ui.label(RichText::new("Stored in").strong());
            ui.label(RichText::new(backend.label()).small());
            ui.end_row();
        });

    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "Paste the value below. It is held in a SecretString \
             (zeroized on drop) and written straight to the OS keychain — \
             the agent layer never sees it.",
        )
        .small()
        .italics(),
    );
}

impl InventoryApp {
    fn open_dialog_for(&mut self, path: &str) {
        let entry = match self.metadata_by_path.get(path) {
            Some(e) => e.clone(),
            None => devboy_storage::IndexEntry::default(),
        };
        let metadata = devboy_secrets_ui::DialogMetadata {
            path: path.to_owned(),
            provider: path.split('/').nth(1).unwrap_or("unknown").to_owned(),
            rotation_method: entry
                .rotation_method
                .map(|m| format!("{m:?}").to_lowercase())
                .unwrap_or_else(|| "manual".to_owned()),
            provisioning_url: entry.retrieval_url.clone(),
            format_hint: entry.description.clone(),
        };
        self.selected_variant_id = self.resolve_initial_variant(&entry);
        self.dialog = Some(devboy_secrets_ui::DialogState::new(
            devboy_secrets_ui::DialogMode::Provision,
            metadata,
        ));
        self.dialog_path = Some(path.to_owned());
        self.last_save_error = None;
    }

    /// Pick the variant that should be selected when the
    /// dialog opens. Two strategies in priority order:
    ///
    /// 1. `entry.pattern_id` matches a specific variant id
    ///    (`kimi-cn`, `kimi-global`, …) — pre-select that
    ///    one.
    /// 2. `entry.pattern_id` matches a `provider_id` (`kimi`)
    ///    — pre-select the provider's first variant. The
    ///    user can flip via radio.
    ///
    /// Returns `None` when no catalog match — the dialog
    /// renders without a picker.
    fn resolve_initial_variant(&self, entry: &devboy_storage::IndexEntry) -> Option<String> {
        let pid = entry.pattern_id.as_deref()?;
        // Specific-variant match wins.
        if let Some((_, variant)) = devboy_token_catalog::find_variant_by_id(
            &self
                .catalogs
                .iter()
                .map(|l| l.catalog.clone())
                .collect::<Vec<_>>(),
            pid,
        ) {
            return Some(variant.id.clone());
        }
        // Otherwise: pick first variant of the matching provider.
        self.catalogs
            .iter()
            .find(|l| l.catalog.provider_id == pid)
            .and_then(|l| l.catalog.variants.first().map(|v| v.id.clone()))
    }

    /// Look up the loaded catalog whose provider owns the
    /// currently-selected variant. Returns the catalog plus
    /// the variant slot, when both resolve.
    fn current_provider_and_variant(
        &self,
    ) -> Option<(
        &devboy_token_catalog::LoadedCatalog,
        &devboy_token_catalog::TokenVariant,
    )> {
        let variant_id = self.selected_variant_id.as_deref()?;
        for loaded in &self.catalogs {
            if let Some(v) = loaded.catalog.variants.iter().find(|v| v.id == variant_id) {
                return Some((loaded, v));
            }
        }
        None
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn env_linux(display: Option<&str>, wayland: Option<&str>, tty: bool) -> DetectionEnv {
        DetectionEnv {
            display: display.map(str::to_string),
            wayland_display: wayland.map(str::to_string),
            target_os: TargetOs::Linux,
            stdout_is_tty: tty,
        }
    }

    fn env_macos(tty: bool) -> DetectionEnv {
        DetectionEnv {
            display: None,
            wayland_display: None,
            target_os: TargetOs::MacOs,
            stdout_is_tty: tty,
        }
    }

    fn env_windows(tty: bool) -> DetectionEnv {
        DetectionEnv {
            display: None,
            wayland_display: None,
            target_os: TargetOs::Windows,
            stdout_is_tty: tty,
        }
    }

    fn env_other(tty: bool) -> DetectionEnv {
        DetectionEnv {
            display: None,
            wayland_display: None,
            target_os: TargetOs::Other,
            stdout_is_tty: tty,
        }
    }

    // -- detect_preferred --------------------------------------

    #[test]
    fn linux_with_display_picks_gui() {
        assert_eq!(
            detect_preferred(&env_linux(Some(":0"), None, true)),
            Backend::Gui
        );
    }

    #[test]
    fn linux_with_wayland_display_picks_gui() {
        assert_eq!(
            detect_preferred(&env_linux(None, Some("wayland-0"), true)),
            Backend::Gui
        );
    }

    #[test]
    fn linux_without_either_display_var_picks_tui() {
        assert_eq!(detect_preferred(&env_linux(None, None, true)), Backend::Tui);
    }

    #[test]
    fn linux_treats_empty_display_string_as_unset() {
        // `DISPLAY=` (set but empty) is what some sshd configs
        // leak through — that's not a usable display.
        let env = DetectionEnv {
            display: Some(String::new()).filter(|s| !s.is_empty()),
            wayland_display: None,
            target_os: TargetOs::Linux,
            stdout_is_tty: true,
        };
        assert_eq!(detect_preferred(&env), Backend::Tui);
    }

    #[test]
    fn macos_picks_gui_unconditionally() {
        assert_eq!(detect_preferred(&env_macos(true)), Backend::Gui);
    }

    #[test]
    fn windows_picks_gui_unconditionally() {
        assert_eq!(detect_preferred(&env_windows(true)), Backend::Gui);
    }

    #[test]
    fn unknown_os_falls_back_to_tui() {
        assert_eq!(detect_preferred(&env_other(true)), Backend::Tui);
    }

    // -- resolve overrides -------------------------------------

    #[test]
    fn force_tui_overrides_gui_environment() {
        let backend = resolve(BackendChoice::ForceTui, &env_macos(true)).unwrap();
        assert_eq!(backend, Backend::Tui);
    }

    #[test]
    fn force_gui_overrides_tui_environment() {
        let backend = resolve(BackendChoice::ForceGui, &env_linux(None, None, true)).unwrap();
        assert_eq!(backend, Backend::Gui);
    }

    #[test]
    fn auto_picks_the_detected_backend() {
        let backend = resolve(BackendChoice::Auto, &env_macos(true)).unwrap();
        assert_eq!(backend, Backend::Gui);
        let backend = resolve(BackendChoice::Auto, &env_linux(None, None, true)).unwrap();
        assert_eq!(backend, Backend::Tui);
    }

    // -- TTY guard ---------------------------------------------

    #[test]
    fn tui_in_a_pipe_errors_loudly_rather_than_scrambling_output() {
        // Force TUI but stdin/stdout aren't a terminal — must
        // refuse, not write raw escape codes into the pipe.
        let err = resolve(BackendChoice::ForceTui, &env_macos(false)).unwrap_err();
        assert!(err.to_string().contains("not a terminal"));
    }

    #[test]
    fn auto_on_linux_without_display_in_a_pipe_also_errors() {
        // No display vars → would pick TUI → no TTY → must fail
        // cleanly so the user knows to attach a terminal.
        let err = resolve(BackendChoice::Auto, &env_linux(None, None, false)).unwrap_err();
        assert!(err.to_string().contains("not a terminal"));
    }

    #[test]
    fn force_gui_in_a_pipe_succeeds_because_gui_does_not_use_stdout() {
        // The GUI launcher renders into its own window; a piped
        // stdout doesn't matter.
        let backend = resolve(BackendChoice::ForceGui, &env_macos(false)).unwrap();
        assert_eq!(backend, Backend::Gui);
    }

    // -- UiArgs.choice -----------------------------------------

    #[test]
    fn ui_args_default_is_auto() {
        assert_eq!(UiArgs::default().choice(), BackendChoice::Auto);
    }

    #[test]
    fn ui_args_tui_flag_forces_tui() {
        let args = UiArgs {
            tui: true,
            gui: false,
            provision: None,
        };
        assert_eq!(args.choice(), BackendChoice::ForceTui);
    }

    #[test]
    fn ui_args_gui_flag_forces_gui() {
        let args = UiArgs {
            tui: false,
            gui: true,
            provision: None,
        };
        assert_eq!(args.choice(), BackendChoice::ForceGui);
    }
}
