//! `devboy-secrets-ui` — native GUI window for the
//! devboy-tools secrets inventory.
//!
//! This binary is split out of the main `devboy` CLI so the
//! eframe/egui rendering stack (eframe + egui + winit +
//! glow/wayland/x11 + skrifa fonts + image decoders) is not
//! linked into the CLI on machines that never open the GUI
//! (CI runners doing `secrets list` / `secrets validate`,
//! mostly). See ADR-023 §3.4.
//!
//! Invocation contract (the `devboy` CLI launches us as a
//! subprocess; humans normally don't call this directly):
//!
//! ```text
//! devboy-secrets-ui [--provision <PATH>]
//! ```
//!
//! `--provision <PATH>` opens the inventory window with the
//! provision dialog pre-armed on `<PATH>` — same UX as
//! `devboy secrets ui --gui --provision <PATH>` was before
//! the split.

use anyhow::Result;
use clap::Parser;

mod catalog_metadata;

/// Command-line surface mirrored from the original
/// `devboy secrets ui` clap struct so existing users see
/// the same flags when they call the subprocess directly.
#[derive(Parser, Debug)]
#[command(
    name = "devboy-secrets-ui",
    version,
    about = "Native GUI window for the devboy-tools secrets inventory.",
    long_about = "Launches an eframe/egui window that renders the merged \
                  secrets inventory and offers a provision dialog. Normally \
                  spawned by `devboy secrets ui --gui`; called directly only \
                  for debugging the GUI in isolation."
)]
struct Cli {
    /// ADR-020 path the provision dialog should focus on at
    /// startup. When omitted the window opens on the
    /// inventory list with no dialog armed.
    #[arg(long, value_name = "PATH")]
    provision: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    launch_gui(cli.provision.as_deref())
}

// =============================================================================
// GUI launcher
// =============================================================================

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

// =============================================================================
// InventoryApp
// =============================================================================

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
    /// URLs that are listed in `sources.toml` but have no entry
    /// in `known_hashes.toml` — the user must explicitly trust
    /// them via the confirm dialog (P23.6) before the catalog
    /// activates. Each tuple is `(url, sha256-of-fetched-body)`.
    pending_url_confirms: Vec<(String, String)>,
    /// URLs whose body now hashes to a value different from
    /// `known_hashes.toml`. The user must explicitly accept the
    /// new hash via the warning dialog or reject the load.
    /// Tuple: `(url, known_sha, actual_sha)`.
    pending_url_warnings: Vec<(String, String, String)>,
}

impl InventoryApp {
    fn new_with_initial(initial_path: Option<String>) -> Self {
        let backend = StorageBackend::detect_from_env();
        // Catalogs first — `load_inventory_or_empty` needs them
        // to populate the per-row `catalog_override` chip (P22.2).
        let (catalogs, catalog_errors) = load_token_catalogs();
        let (rows, metadata_by_path) = load_inventory_or_empty(&backend, &catalogs);
        let (pending_url_confirms, pending_url_warnings) =
            partition_url_trust_errors(&catalog_errors);
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
            pending_url_confirms,
            pending_url_warnings,
        };
        if let Some(path) = initial_path {
            app.open_dialog_for(&path);
        }
        app
    }

    /// Re-walk the catalog sources after the user has accepted
    /// or rejected an outstanding URL prompt. Side-effect-free
    /// in the happy path: nothing changes if the resolution
    /// did not affect any source.
    fn reload_catalogs(&mut self) {
        let (catalogs, catalog_errors) = load_token_catalogs();
        let (pending_url_confirms, pending_url_warnings) =
            partition_url_trust_errors(&catalog_errors);
        self.catalogs = catalogs;
        self.catalog_errors = catalog_errors;
        self.pending_url_confirms = pending_url_confirms;
        self.pending_url_warnings = pending_url_warnings;
    }

    fn reload(&mut self) {
        let (rows, metadata) = load_inventory_or_empty(&self.backend, &self.catalogs);
        self.state.replace_rows(rows);
        self.metadata_by_path = metadata;
    }
}

// =============================================================================
// StorageBackend
// =============================================================================

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

/// `(url, sha256)` for a URL whose trust must be confirmed
/// for the first time.
type FirstFetchPrompt = (String, String);
/// `(url, known_sha, current_sha)` for a URL whose body now
/// hashes to a different value than `known_hashes.toml` records.
type TofuPrompt = (String, String, String);

/// Split catalog-load errors into the two trust-related lists
/// the GUI needs: first-fetch URLs awaiting user confirmation
/// and TOFU mismatches awaiting a "trust the new sha?" decision.
/// Anything else stays in `catalog_errors` for the inline banner.
fn partition_url_trust_errors(
    errors: &[devboy_token_catalog::CatalogError],
) -> (Vec<FirstFetchPrompt>, Vec<TofuPrompt>) {
    use devboy_token_catalog::{CatalogError, FetchError};
    let mut confirms = Vec::new();
    let mut warnings = Vec::new();
    for e in errors {
        if let CatalogError::Fetch { source, .. } = e {
            match source {
                FetchError::FirstFetchNeedsConfirmation { url, sha256 } => {
                    confirms.push((url.clone(), sha256.clone()));
                }
                FetchError::TofuMismatch { url, known, actual } => {
                    warnings.push((url.clone(), known.clone(), actual.clone()));
                }
                _ => {}
            }
        }
    }
    (confirms, warnings)
}

/// Load token catalogs from every configured source and merge them.
///
/// Sources walked, least-to-most specific: bundled (compiled in),
/// user (`~/.devboy/secrets/catalog/`), project
/// (`<cwd>/.devboy/secrets/catalog/`), URL (`sources.toml`, opt-in).
/// Later sources override earlier ones on `provider_id` collision —
/// see `devboy_token_catalog::load_all_with_urls` for the rule.
///
/// First-fetch policy is `RequireConfirmation`: a URL whose hash
/// has not yet been recorded in `known_hashes.toml` surfaces a
/// `FirstFetchNeedsConfirmation` error instead of being silently
/// trusted. The GUI catches it and shows a confirm dialog.
fn load_token_catalogs() -> (
    Vec<devboy_token_catalog::LoadedCatalog>,
    Vec<devboy_token_catalog::CatalogError>,
) {
    let bundled = devboy_token_catalog::bundled_catalogs();
    let user_dir = devboy_token_catalog::default_user_catalog_dir();
    let project_dir = std::env::current_dir()
        .ok()
        .map(|cwd| devboy_token_catalog::default_project_catalog_dir(&cwd));
    let url_config = devboy_token_catalog::default_sources_toml_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|body| devboy_token_catalog::parse_sources_toml(&body).ok());
    let known_hashes_path = devboy_token_catalog::default_known_hashes_path();
    let cache_dir = devboy_token_catalog::default_catalog_cache_dir();
    let audit_log_path = devboy_token_catalog::default_catalog_audit_log_path();
    devboy_token_catalog::load_all_with_urls(
        &bundled,
        user_dir.as_deref(),
        project_dir.as_deref(),
        url_config.as_ref(),
        known_hashes_path.as_deref(),
        cache_dir.as_deref(),
        devboy_token_catalog::FirstFetchPolicy::RequireConfirmation,
        audit_log_path.as_deref(),
    )
}

/// Walk global index + project manifest in CWD, merge them, and
/// return inventory rows ready for `InventoryState::replace_rows`
/// plus a `path → IndexEntry` map the dialog uses to populate
/// its metadata fields.
///
/// `catalogs` feeds the per-row `catalog_override` chip (P22.2):
/// when a row's middle path segment matches a catalog whose
/// source is *not* `Bundled`, the chip is set so the GUI can
/// render a coloured tag inline.
fn load_inventory_or_empty(
    backend: &StorageBackend,
    catalogs: &[devboy_token_catalog::LoadedCatalog],
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
        let provider_segment = resolved.path.as_str().split('/').nth(1);
        rows.push(devboy_secrets_ui::InventoryRow {
            path: path_str.clone(),
            status,
            routed_source: provisioned.then(|| backend.source_label().to_owned()),
            expires_at: resolved.metadata.expires_at.clone(),
            provider: provider_segment.map(str::to_owned),
            scope: resolved
                .path
                .as_str()
                .split('/')
                .next()
                .unwrap_or("")
                .to_owned(),
            catalog_override: provider_segment.and_then(|p| catalog_override_for(catalogs, p)),
        });
        metadata.insert(path_str, resolved.metadata.clone());
    }
    (rows, metadata)
}

/// Determine the inline catalog-override badge for a row whose
/// middle path segment is `provider_id`. Returns `None` for
/// bundled sources (the common case — no chip needed) and for
/// paths whose provider isn't in any loaded catalog.
fn catalog_override_for(
    catalogs: &[devboy_token_catalog::LoadedCatalog],
    provider_id: &str,
) -> Option<String> {
    use devboy_token_catalog::CatalogSource;
    let loaded = catalogs
        .iter()
        .find(|l| l.catalog.provider_id == provider_id)?;
    match &loaded.source {
        CatalogSource::Bundled => None,
        CatalogSource::User => Some("user".to_owned()),
        CatalogSource::Project => Some("project".to_owned()),
        CatalogSource::Url { url, .. } => {
            let host = reqwest::Url::parse(url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_else(|| "url".to_owned());
            Some(format!("url:{host}"))
        }
    }
}

// =============================================================================
// eframe::App impl
// =============================================================================

impl eframe::App for InventoryApp {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        // Backend banner — tells the user where saves go.
        ui.label(
            eframe::egui::RichText::new(format!("Backend: {}", self.backend.label()))
                .small()
                .color(eframe::egui::Color32::from_rgb(0x55, 0xaa, 0xff)),
        );
        ui.separator();

        // URL trust prompts (P23.6). Surface as modeless `egui::Window`s
        // so the user can compare against the inventory before deciding.
        // Each is rendered through `render_url_trust_prompts`, which
        // returns the user's decision + the URL it concerns; we apply
        // it before the rest of the frame so the next reload reflects
        // the new state.
        let trust_decisions = self.render_url_trust_prompts(ui.ctx());
        if !trust_decisions.is_empty() {
            self.apply_trust_decisions(trust_decisions);
        }

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
                            ui.label(catalog_source_chip(&loaded.source));
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
                        .map(|(loaded, v)| (v, &loaded.source));
                    render_context_card(
                        ui,
                        &dialog_path,
                        entry.as_ref(),
                        &self.backend,
                        variant_for_card,
                    );
                    ui.separator();

                    // Format hint (P22.1). Human-readable shape
                    // ("starts with sk-, 32+ alphanumeric") sourced
                    // from the catalog variant's `format_hint`.
                    // Distinct from the regex feedback rendered
                    // *below* the input: the hint is a shape the
                    // user reads BEFORE typing; the feedback
                    // confirms what they typed AFTER.
                    let variant_format_hint: Option<String> = self
                        .current_provider_and_variant()
                        .and_then(|(_, v)| v.format_hint.clone());
                    if let Some(hint) = variant_format_hint {
                        ui.label(
                            eframe::egui::RichText::new(format!("Format: {hint}"))
                                .small()
                                .italics(),
                        );
                    }

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

// =============================================================================
// Liveness probes
// =============================================================================

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

    // SSRF guard — refuse to dial private / loopback / link-local
    // / cloud-metadata addresses even when the rust-catalogue
    // ships them (defence in depth: same check fires in
    // `run_catalog_liveness`). See P23.4.
    devboy_token_catalog::check_ssrf_safe(url)
        .map_err(|e| format!("liveness URL refused for safety: {e}"))?;

    // Blocking client — we run inside an egui frame so async
    // wouldn't help anyway. 5-second timeout. The SSRF-safe
    // builder re-checks every redirect target so an HTTPS
    // upstream cannot 30x into RFC1918 / cloud-metadata after
    // the original-URL guard passes.
    let client = devboy_token_catalog::ssrf_safe_blocking_client(std::time::Duration::from_secs(5))
        .map_err(|e| format!("could not build HTTP client: {e}"))?;
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

    // SSRF guard — same check the rust-catalogue path runs.
    // The catalog gets to declare *where* the GUI ships a
    // freshly-typed secret, so this is the most security-
    // critical chokepoint in the URL-source threat model. See
    // P23.4 / `project_url_catalog_design.md`.
    devboy_token_catalog::check_ssrf_safe(&spec.url)
        .map_err(|e| format!("liveness URL refused for safety: {e}"))?;

    // SSRF-safe blocking client — re-checks redirects so the
    // catalog-declared liveness URL cannot 30x into private
    // / loopback / cloud-metadata space.
    let client = devboy_token_catalog::ssrf_safe_blocking_client(std::time::Duration::from_secs(5))
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

// =============================================================================
// Render helpers
// =============================================================================

/// Render-ready chip indicating where a catalog entry came
/// from. Used both in the multi-variant picker title and in
/// the context card so a team override (project-scope JSON
/// shadowing the bundled default) is visible at a glance.
fn catalog_source_chip(source: &devboy_token_catalog::CatalogSource) -> eframe::egui::RichText {
    use devboy_token_catalog::CatalogSource;
    use eframe::egui::{Color32, RichText};
    let (label, color) = match source {
        CatalogSource::Bundled => ("bundled".to_owned(), Color32::from_rgb(0x99, 0x99, 0x99)),
        CatalogSource::User => ("user".to_owned(), Color32::from_rgb(0x55, 0xa0, 0xcc)),
        CatalogSource::Project => ("project".to_owned(), Color32::from_rgb(0x55, 0xaa, 0x55)),
        // URL sources get an orange chip + the hostname so the
        // user can see at a glance whether a remote catalog is
        // shadowing a bundled default. P23.6 layers the
        // first-fetch confirm + diff-on-change UX on top of
        // this; the chip itself is the always-on tell.
        CatalogSource::Url { url, .. } => {
            let host = reqwest::Url::parse(url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_else(|| "url".to_owned());
            (format!("url:{host}"), Color32::from_rgb(0xdd, 0x88, 0x33))
        }
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
        &devboy_token_catalog::CatalogSource,
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

// =============================================================================
// InventoryApp helpers + dialog open
// =============================================================================

impl InventoryApp {
    fn open_dialog_for(&mut self, path: &str) {
        let entry = match self.metadata_by_path.get(path) {
            Some(e) => e.clone(),
            None => devboy_storage::IndexEntry::default(),
        };
        // Resolve the variant first so the catalog builder can
        // surface the matching variant's description / steps /
        // notes (P26 / S2). Falls back to the catalog's first
        // variant inside the builder when nothing matched.
        self.selected_variant_id = self.resolve_initial_variant(&entry);
        let metadata = crate::catalog_metadata::metadata_from_catalog_and_entry(
            path,
            &entry,
            &self.catalogs,
            self.selected_variant_id.as_deref(),
        );
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

    /// Render every outstanding URL trust prompt and collect
    /// the user's responses. Modeless `egui::Window`s — the
    /// user can compare against the rest of the GUI before
    /// deciding. Returned tuples are applied by
    /// [`Self::apply_trust_decisions`].
    fn render_url_trust_prompts(&mut self, ctx: &eframe::egui::Context) -> Vec<UrlTrustDecision> {
        use eframe::egui::{Color32, RichText};
        let mut out: Vec<UrlTrustDecision> = Vec::new();

        // First-fetch confirms — orange chip; "Trust this catalog"
        // accepts and persists the SHA into known_hashes.toml.
        for (url, sha) in self.pending_url_confirms.clone() {
            let title = format!("Trust new URL catalog: {url}");
            eframe::egui::Window::new(&title)
                .resizable(false)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(
                        RichText::new(
                            "This URL is listed in sources.toml but has not been seen before.",
                        )
                        .strong(),
                    );
                    ui.label("Verify the URL is one your team operates before accepting.");
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("URL:").strong());
                        ui.hyperlink(&url);
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("SHA256:").strong());
                        ui.label(RichText::new(&sha).monospace());
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button(
                                RichText::new("Trust this catalog")
                                    .color(Color32::from_rgb(0x55, 0xaa, 0x55)),
                            )
                            .clicked()
                        {
                            out.push(UrlTrustDecision::Accept {
                                url: url.clone(),
                                sha256: sha.clone(),
                            });
                        }
                        if ui
                            .button(
                                RichText::new("Reject").color(Color32::from_rgb(0xcc, 0x44, 0x44)),
                            )
                            .clicked()
                        {
                            out.push(UrlTrustDecision::Reject { url: url.clone() });
                        }
                    });
                });
        }

        // SHA mismatch — red chip; far stronger language. The
        // legitimate-rotation case is recoverable via the
        // "Trust the new SHA" button, but the typical reason
        // for surfacing this is upstream compromise — so the
        // copy leans toward suspicion.
        for (url, known, actual) in self.pending_url_warnings.clone() {
            let title = format!("⚠ Catalog SHA changed: {url}");
            eframe::egui::Window::new(&title)
                .resizable(false)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(
                        RichText::new("This URL's body has changed since you last trusted it.")
                            .color(Color32::from_rgb(0xcc, 0x44, 0x44))
                            .strong(),
                    );
                    ui.label(
                        "Most often this is the upstream rotating its file legitimately, \
                         but it is also exactly what an upstream compromise looks like. \
                         When in doubt, refuse and verify out-of-band before accepting.",
                    );
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("URL:").strong());
                        ui.hyperlink(&url);
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Known SHA256:").strong());
                        ui.label(RichText::new(&known).monospace());
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Current SHA256:").strong());
                        ui.label(
                            RichText::new(&actual)
                                .monospace()
                                .color(Color32::from_rgb(0xcc, 0x44, 0x44)),
                        );
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new("Trust the new SHA")).clicked() {
                            out.push(UrlTrustDecision::Accept {
                                url: url.clone(),
                                sha256: actual.clone(),
                            });
                        }
                        if ui
                            .button(
                                RichText::new("Reject and keep old SHA")
                                    .color(Color32::from_rgb(0xcc, 0x44, 0x44)),
                            )
                            .clicked()
                        {
                            out.push(UrlTrustDecision::Reject { url: url.clone() });
                        }
                    });
                });
        }

        out
    }

    /// Apply user decisions from
    /// [`Self::render_url_trust_prompts`] to `known_hashes.toml`,
    /// then re-walk the catalog sources so accepted catalogs
    /// activate (or rejected ones drop from the pending list).
    fn apply_trust_decisions(&mut self, decisions: Vec<UrlTrustDecision>) {
        if decisions.is_empty() {
            return;
        }
        let Some(known_hashes_path) = devboy_token_catalog::default_known_hashes_path() else {
            self.last_save_error = Some("could not resolve known_hashes.toml path".to_owned());
            return;
        };
        for decision in decisions {
            match decision {
                UrlTrustDecision::Accept { url, sha256 } => {
                    if let Err(e) =
                        devboy_token_catalog::record_url_trust(&known_hashes_path, &url, &sha256)
                    {
                        self.last_save_error =
                            Some(format!("could not record trust for {url}: {e}"));
                    }
                }
                UrlTrustDecision::Reject { url } => {
                    self.pending_url_confirms.retain(|(u, _)| u != &url);
                    self.pending_url_warnings.retain(|(u, _, _)| u != &url);
                }
            }
        }
        self.reload_catalogs();
    }
}

/// One decision the user made about a pending URL trust prompt.
/// Emitted by [`InventoryApp::render_url_trust_prompts`] and
/// consumed by [`InventoryApp::apply_trust_decisions`].
#[derive(Debug, Clone)]
enum UrlTrustDecision {
    Accept { url: String, sha256: String },
    Reject { url: String },
}
