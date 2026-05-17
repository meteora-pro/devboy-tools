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
#[cfg(feature = "dev-screenshot")]
mod screenshot;

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

    /// Dev-only: render one UI frame to a PNG at this path and
    /// exit, instead of opening a window. Built only with
    /// `--features dev-screenshot`. Lets an automated agent
    /// visually verify GUI changes without a human at the
    /// screen. Pair with `--screenshot-view` to pick which view.
    #[arg(long, value_name = "PATH")]
    screenshot: Option<std::path::PathBuf>,

    /// Dev-only: which view `--screenshot` renders — `provision`
    /// (default), `unlock`, or `create`. Ignored without
    /// `--screenshot`.
    #[arg(long, value_name = "VIEW", default_value = "provision")]
    screenshot_view: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    #[cfg(feature = "dev-screenshot")]
    if let Some(out) = cli.screenshot.as_deref() {
        let view = screenshot::ScreenshotView::parse(&cli.screenshot_view)?;
        return screenshot::render_to_png(view, out);
    }
    #[cfg(not(feature = "dev-screenshot"))]
    if cli.screenshot.is_some() {
        anyhow::bail!(
            "--screenshot requires a build with `--features dev-screenshot`; \
             this binary was built without it"
        );
    }

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
/// 3. When armed, shows the provision dialog as a true modal
///    overlay (`egui::Modal`) — dimmed backdrop over the
///    inventory, ESC / click-outside to dismiss. On `Save`,
///    writes the value straight to the OS keychain via
///    `KeychainStore` and refreshes the row's status.
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
    /// Vault unlock / create modal state. `Some` while the
    /// backend is a locked local-vault and the user hasn't
    /// unlocked it (or chosen the keychain escape hatch) yet —
    /// the inventory renders dimmed underneath until this is
    /// `None`.
    vault_unlock: Option<devboy_secrets_ui::VaultUnlockState>,
    /// First-run onboarding wizard. `Some` on the very first
    /// launch (no `sources.toml`, no vault file, no env
    /// passphrase) until the user finishes or skips it.
    onboarding: Option<devboy_secrets_ui::OnboardingState>,
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
        // A locked backend (local-vault OR KDBX) arms the
        // unlock modal at startup — the inventory loads
        // (everything reads as not-provisioned, since
        // `has_value` on a locked backend is `false`) but
        // stays gated behind the modal until the user unlocks
        // or picks the keychain hatch. The copy is swapped per
        // backend so the modal heading matches what's actually
        // being opened.
        let vault_unlock = if backend.is_locked() {
            let state = devboy_secrets_ui::VaultUnlockState::new(
                devboy_secrets_ui::VaultUnlockMode::Unlock,
            );
            let state = match &backend {
                StorageBackend::KdbxLocked { file, .. } => state.with_copy(
                    "Unlock KeePass database",
                    format!(
                        "Enter the passphrase that protects `{}`. The decrypted entries \
                         are held only inside this window and are never sent to the agent.",
                        file.display()
                    ),
                ),
                _ => state,
            };
            Some(state)
        } else {
            None
        };
        // First run — nothing configured anywhere — arms the
        // onboarding wizard. `vault_unlock` and `onboarding`
        // are mutually exclusive: a locked vault means the
        // user already onboarded, so `is_first_run()` returns
        // false in that case.
        let onboarding = if is_first_run() {
            Some(devboy_secrets_ui::OnboardingState::new())
        } else {
            None
        };
        let mut app = Self {
            state: devboy_secrets_ui::InventoryState::new(rows),
            metadata_by_path,
            dialog: None,
            dialog_path: None,
            last_save_error: None,
            backend,
            recovery_phrase_to_show: None,
            selected_variant_id: None,
            catalogs,
            catalog_errors,
            pending_url_confirms,
            pending_url_warnings,
            vault_unlock,
            onboarding,
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

    /// Render the vault unlock / create modal and apply its
    /// outcome. No-op when `self.vault_unlock` is `None`.
    ///
    /// On submit: attempt `backend.unlock(passphrase)`. Success
    /// swaps in the unlocked backend, clears the modal, and
    /// reloads the inventory so rows show real has-value
    /// status. Failure keeps the modal open with the reason in
    /// red. The "Use keychain instead" hatch switches the
    /// backend to keychain for the session.
    fn render_vault_unlock_modal(&mut self, ctx: &eframe::egui::Context) {
        if self.vault_unlock.is_none() {
            return;
        }
        let mut submit = false;
        let mut use_keychain = false;
        {
            let state = self.vault_unlock.as_mut().unwrap();
            eframe::egui::Modal::new(eframe::egui::Id::new("vault-unlock-modal")).show(ctx, |ui| {
                ui.set_max_width(420.0);
                let result = devboy_secrets_ui::gui::vault_unlock::render(ui, state);
                submit = result.submit;
                use_keychain = result.use_keychain;
            });
        }

        if submit {
            // Snapshot the passphrase + mode, flip the modal to
            // "working", then attempt the real open / create.
            let modal = self.vault_unlock.as_ref().expect("guarded above");
            let passphrase = modal.passphrase().clone();
            let mode = modal.mode();
            if let Some(s) = self.vault_unlock.as_mut() {
                s.apply_status(devboy_secrets_ui::VaultUnlockStatus::Working);
            }

            let outcome: Result<(StorageBackend, Option<String>), String> = match mode {
                // Unlock — open an existing `.dvb` file.
                devboy_secrets_ui::VaultUnlockMode::Unlock => {
                    self.backend.unlock(passphrase).map(|b| (b, None))
                }
                // Create — mint a new vault. The path is the
                // locked backend's path if we have one,
                // otherwise the default location.
                devboy_secrets_ui::VaultUnlockMode::Create => {
                    let vault_path = match &self.backend {
                        StorageBackend::LocalVaultLocked { vault_path }
                        | StorageBackend::LocalVault { vault_path, .. } => vault_path.clone(),
                        // Keychain, HTTP-Vault, or KDBX backend → a
                        // newly created local-vault lands at the
                        // default path. The user can always pick
                        // a different file later via the
                        // backend-picker (V5).
                        StorageBackend::Keychain
                        | StorageBackend::HttpVault { .. }
                        | StorageBackend::KdbxLocked { .. }
                        | StorageBackend::Kdbx { .. } => default_vault_path(),
                    };
                    StorageBackend::create_vault(vault_path, passphrase)
                }
            };

            match outcome {
                Ok((backend, recovery)) => {
                    self.backend = backend;
                    self.vault_unlock = None;
                    // Surface the recovery phrase exactly once
                    // (create flow only — `unlock` returns None).
                    if let Some(phrase) = recovery {
                        self.recovery_phrase_to_show = Some(phrase);
                    }
                    // Re-read the inventory: a locked / keychain
                    // backend may have reported rows differently;
                    // now it shows the real state.
                    self.reload();
                }
                Err(reason) => {
                    if let Some(s) = self.vault_unlock.as_mut() {
                        s.apply_status(devboy_secrets_ui::VaultUnlockStatus::Failed { reason });
                    }
                }
            }
        } else if use_keychain {
            // Escape hatch — fall back to the OS keychain for
            // this session.
            self.backend = StorageBackend::Keychain;
            self.vault_unlock = None;
            self.reload();
        }
    }

    /// Render the first-run onboarding modal and apply its
    /// outcome. No-op when `self.onboarding` is `None`.
    ///
    /// On "Finish setup": write `sources.toml`, create the
    /// local-vault if the user picked one (surfacing the
    /// recovery phrase), then resolve `self.backend` to the
    /// chosen primary provider. On "Skip": straight to the
    /// keychain, no files written.
    fn render_onboarding_modal(&mut self, ctx: &eframe::egui::Context) {
        if self.onboarding.is_none() {
            return;
        }
        let mut finish = false;
        let mut skip = false;
        {
            let state = self.onboarding.as_mut().unwrap();
            eframe::egui::Modal::new(eframe::egui::Id::new("onboarding-modal")).show(ctx, |ui| {
                ui.set_max_width(480.0);
                let result = devboy_secrets_ui::gui::onboarding::render(ui, state);
                finish = result.finish;
                skip = result.skip;
            });
        }

        if skip {
            self.backend = StorageBackend::Keychain;
            self.onboarding = None;
            self.reload();
            return;
        }
        if !finish {
            return;
        }

        // Snapshot everything off the modal before mutating
        // `self` (the modal borrow has to end first).
        let state = self.onboarding.as_ref().expect("guarded above");
        let toml_body = state.to_sources_toml();
        let primary = state.primary();
        let wants_local = state.local_vault_selected();
        let local_pass = state.local_passphrase().clone();
        let http = if state.http_vault_selected() {
            let access = match state.http_access() {
                devboy_secrets_ui::OnboardingAccess::Read => devboy_storage::SourceAccess::Read,
                devboy_secrets_ui::OnboardingAccess::ReadWrite => {
                    devboy_storage::SourceAccess::ReadWrite
                }
            };
            let ns = state.http_namespace().trim();
            Some((
                state.http_addr().trim().to_owned(),
                (!ns.is_empty()).then(|| ns.to_owned()),
                state.http_mount().trim().to_owned(),
                state.http_token().clone(),
                access,
            ))
        } else {
            None
        };

        if let Some(s) = self.onboarding.as_mut() {
            s.apply_status(devboy_secrets_ui::OnboardingStatus::Working);
        }

        // 1. Write sources.toml.
        if let Err(reason) = write_sources_toml(&toml_body) {
            if let Some(s) = self.onboarding.as_mut() {
                s.apply_status(devboy_secrets_ui::OnboardingStatus::Failed { reason });
            }
            return;
        }

        // 2. Create the local-vault if the user picked one.
        let mut created_local: Option<StorageBackend> = None;
        if wants_local {
            match StorageBackend::create_vault(default_vault_path(), local_pass) {
                Ok((backend, recovery)) => {
                    if let Some(phrase) = recovery {
                        self.recovery_phrase_to_show = Some(phrase);
                    }
                    created_local = Some(backend);
                }
                Err(reason) => {
                    if let Some(s) = self.onboarding.as_mut() {
                        s.apply_status(devboy_secrets_ui::OnboardingStatus::Failed { reason });
                    }
                    return;
                }
            }
        }

        // 3. Resolve `self.backend` to the chosen primary.
        self.backend = match primary {
            devboy_secrets_ui::OnboardingProvider::Keychain => StorageBackend::Keychain,
            devboy_secrets_ui::OnboardingProvider::LocalVault => {
                // `wants_local` must be true if local-vault is
                // primary (the wizard only lets you pick a
                // selected provider) — but fall back to
                // keychain rather than panic if something drifts.
                created_local.unwrap_or(StorageBackend::Keychain)
            }
            devboy_secrets_ui::OnboardingProvider::HttpVault => {
                let (addr, namespace, mount, token, access) =
                    http.expect("http-vault primary implies http-vault selected");
                StorageBackend::HttpVault {
                    addr,
                    namespace,
                    mount,
                    token,
                    access,
                }
            }
        };
        self.onboarding = None;
        self.reload();
    }
}

/// Write `body` to the canonical `sources.toml` path, creating
/// the parent directory if needed. Surfaces a human-facing
/// reason on failure.
fn write_sources_toml(body: &str) -> Result<(), String> {
    let path = devboy_storage::RouterConfig::default_path()
        .map_err(|e| format!("could not resolve the sources.toml path: {e}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create the secrets config directory: {e}"))?;
    }
    std::fs::write(&path, body).map_err(|e| format!("could not write {}: {e}", path.display()))
}

// =============================================================================
// StorageBackend
// =============================================================================

/// Resolve the local-vault file path: `DEVBOY_VAULT_PATH` if
/// set, otherwise `<config>/devboy-tools/secrets/local-vault.dvb`.
fn default_vault_path() -> std::path::PathBuf {
    std::env::var("DEVBOY_VAULT_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let mut p = dirs::config_dir().unwrap_or_else(std::env::temp_dir);
            p.push("devboy-tools");
            p.push("secrets");
            p.push("local-vault.dvb");
            p
        })
}

/// `true` when nothing is configured anywhere — no
/// `sources.toml`, no local-vault file, no
/// `DEVBOY_VAULT_PASSPHRASE`. That's the signal to show the
/// first-run onboarding wizard. Any one of those three
/// existing means the user has been here before, so the
/// wizard is skipped (the unlock modal or the env fast-path
/// takes over instead).
fn is_first_run() -> bool {
    let sources_exists = devboy_storage::RouterConfig::default_path()
        .map(|p| p.exists())
        .unwrap_or(false);
    let vault_exists = default_vault_path().exists();
    let env_passphrase = std::env::var("DEVBOY_VAULT_PASSPHRASE")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    !sources_exists && !vault_exists && !env_passphrase
}

/// Which backend the GUI should write secrets to.
///
/// Picked at app construction time. The passphrase-bearing
/// `LocalVault` variant can be reached two ways: the
/// `DEVBOY_VAULT_PASSPHRASE` env var (resolved straight to
/// unlocked at startup), or interactively — `LocalVaultLocked`
/// is the intermediate state the UI shows an unlock prompt for.
/// The user can also switch backends live (V5).
#[derive(Debug, Clone)]
enum StorageBackend {
    /// Default — OS keychain via `devboy_storage::KeychainStore`.
    Keychain,
    /// Local-vault file exists but no passphrase is held yet —
    /// the UI must prompt for one before reads / writes work.
    /// Reached when a `.dvb` file is present but
    /// `DEVBOY_VAULT_PASSPHRASE` was not set.
    LocalVaultLocked { vault_path: std::path::PathBuf },
    /// Local-vault file at `vault_path`, unlocked with
    /// `passphrase`. Reached from the env var, from a
    /// successful [`StorageBackend::unlock`], or from a
    /// first-run create flow. The file is created on first save
    /// if it doesn't exist.
    LocalVault {
        vault_path: std::path::PathBuf,
        passphrase: secrecy::SecretString,
    },
    /// A remote HashiCorp / HCP Vault, reached over the HTTP
    /// KV v2 API via `devboy_secret_vault::VaultSource`.
    /// Configured through the onboarding wizard (W3-W5) and
    /// written into `sources.toml`.
    ///
    /// Read-only in practice: the `SecretSource` trait has no
    /// write surface yet (that's P15), so `store` refuses with
    /// an honest message. `access` still carries the configured
    /// `read` / `readwrite` so the UI can explain *why* a write
    /// was refused.
    HttpVault {
        addr: String,
        namespace: Option<String>,
        /// KV v2 data mount prefix, e.g. `secret/data`. An
        /// ADR-020 path `team/openai/api-key` becomes the Vault
        /// reference `<mount>/team/openai/api-key`.
        mount: String,
        token: secrecy::SecretString,
        access: devboy_storage::SourceAccess,
    },
    /// KDBX file declared (env var `DEVBOY_KDBX_FILE`) but the
    /// passphrase has not been collected yet — the UI shows the
    /// passphrase prompt modal (K4) before any read.
    KdbxLocked {
        file: std::path::PathBuf,
        keyfile: Option<std::path::PathBuf>,
    },
    /// KDBX file unlocked. The decrypted snapshot is held in
    /// process so `has_value` / inventory probes don't repeat the
    /// Argon2id KDF on every call (an 8 MiB DB takes ~1s of CPU
    /// to derive). Snapshot is shared via `Arc` so the enum stays
    /// `Clone` without re-decrypting.
    ///
    /// Read-only MVP: `store` refuses with an honest message
    /// until the KDBX write path (K6 follow-up) ships.
    Kdbx {
        file: std::path::PathBuf,
        keyfile: Option<std::path::PathBuf>,
        /// Held so the user can re-unlock after a `lock()` cycle
        /// without re-entering the passphrase. Lives only inside
        /// this process — never sent to the agent.
        passphrase: secrecy::SecretString,
        /// Flattened, decrypted entries.
        snapshot: std::sync::Arc<devboy_secret_kdbx::KdbxSnapshot>,
    },
}

impl StorageBackend {
    fn detect_from_env() -> Self {
        // 1. Explicit env passphrase → unlocked straight away.
        if let Ok(pass) = std::env::var("DEVBOY_VAULT_PASSPHRASE")
            && !pass.is_empty()
        {
            return StorageBackend::LocalVault {
                vault_path: default_vault_path(),
                passphrase: secrecy::SecretString::new(pass.into()),
            };
        }
        // 2. DEVBOY_KDBX_FILE pointed at a real file → either
        //    locked (passphrase modal) or unlocked-via-env (the
        //    DEVBOY_KDBX_PASSPHRASE escape hatch, mostly for
        //    tests and CI dry-runs). Always preferred over the
        //    local-vault default when set, since the user
        //    explicitly opted into the KDBX backend.
        if let Ok(kdbx_path) = std::env::var("DEVBOY_KDBX_FILE")
            && !kdbx_path.is_empty()
        {
            let file = std::path::PathBuf::from(kdbx_path);
            let keyfile = std::env::var("DEVBOY_KDBX_KEYFILE")
                .ok()
                .filter(|s| !s.is_empty())
                .map(std::path::PathBuf::from);
            if file.exists()
                && let Ok(pass) = std::env::var("DEVBOY_KDBX_PASSPHRASE")
                && !pass.is_empty()
            {
                // Try to unlock straight away. If the open fails
                // (wrong passphrase, corrupt body), fall through
                // to the Locked variant so the UI can show its
                // unlock modal with a fresh prompt.
                let passphrase = secrecy::SecretString::new(pass.into());
                match devboy_secret_kdbx::open_kdbx_into_snapshot(
                    &file,
                    &passphrase,
                    keyfile.as_deref(),
                ) {
                    Ok(snapshot) => {
                        return StorageBackend::Kdbx {
                            file,
                            keyfile,
                            passphrase,
                            snapshot: std::sync::Arc::new(snapshot),
                        };
                    }
                    Err(_) => return StorageBackend::KdbxLocked { file, keyfile },
                }
            }
            // No env passphrase OR the unlock attempt failed →
            // hand to the UI's unlock-modal flow.
            return StorageBackend::KdbxLocked { file, keyfile };
        }
        // 3. No env passphrase, but a vault file exists → the UI
        //    should prompt for the passphrase (V2 unlock modal).
        let vault_path = default_vault_path();
        if vault_path.exists() {
            return StorageBackend::LocalVaultLocked { vault_path };
        }
        // 4. Nothing → keychain default.
        StorageBackend::Keychain
    }

    /// Whether the backend needs a passphrase before reads /
    /// writes will work — drives whether the UI shows the
    /// unlock modal at startup.
    fn is_locked(&self) -> bool {
        matches!(
            self,
            StorageBackend::LocalVaultLocked { .. } | StorageBackend::KdbxLocked { .. }
        )
    }

    /// Attempt to unlock a `LocalVaultLocked` or `KdbxLocked`
    /// backend with `passphrase`. On success returns the
    /// unlocked variant; on failure returns a human-facing
    /// reason (wrong passphrase, corrupt file, …). Error for
    /// already-unlocked / keychain / HTTP-vault backends.
    fn unlock(&self, passphrase: secrecy::SecretString) -> Result<StorageBackend, String> {
        match self {
            StorageBackend::LocalVaultLocked { vault_path } => {
                let unlock = devboy_vault_crypto::UnlockMethod::Passphrase(passphrase.clone());
                devboy_vault_crypto::Vault::open(vault_path, unlock).map_err(|e| format!("{e}"))?;
                Ok(StorageBackend::LocalVault {
                    vault_path: vault_path.clone(),
                    passphrase,
                })
            }
            StorageBackend::KdbxLocked { file, keyfile } => {
                let snapshot = devboy_secret_kdbx::open_kdbx_into_snapshot(
                    file,
                    &passphrase,
                    keyfile.as_deref(),
                )
                .map_err(|e| format!("{e}"))?;
                Ok(StorageBackend::Kdbx {
                    file: file.clone(),
                    keyfile: keyfile.clone(),
                    passphrase,
                    snapshot: std::sync::Arc::new(snapshot),
                })
            }
            _ => Err("backend does not have a passphrase-locked state".to_owned()),
        }
    }

    /// Re-open the KDBX file with the already-held passphrase
    /// and rebuild the snapshot. Lets the user pick up edits
    /// they made in KeePass / KeePassXC without re-typing the
    /// passphrase. No-op (returns the original backend) for
    /// every other variant.
    ///
    /// Wired into the UI's reload-from-manifest button in K4
    /// (silenced for now so the K3 build is warning-clean).
    #[allow(dead_code)]
    fn refresh(&self) -> Result<StorageBackend, String> {
        let StorageBackend::Kdbx {
            file,
            keyfile,
            passphrase,
            ..
        } = self
        else {
            return Ok(self.clone());
        };
        let snapshot =
            devboy_secret_kdbx::open_kdbx_into_snapshot(file, passphrase, keyfile.as_deref())
                .map_err(|e| format!("{e}"))?;
        Ok(StorageBackend::Kdbx {
            file: file.clone(),
            keyfile: keyfile.clone(),
            passphrase: passphrase.clone(),
            snapshot: std::sync::Arc::new(snapshot),
        })
    }

    /// Drop the held passphrase + snapshot, returning the
    /// backend to its locked state. No-op for keychain /
    /// already-locked / HTTP Vault (the Vault token lives in
    /// `sources.toml`, there is no in-memory unlock state to
    /// drop).
    fn lock(&self) -> StorageBackend {
        match self {
            StorageBackend::LocalVault { vault_path, .. }
            | StorageBackend::LocalVaultLocked { vault_path } => StorageBackend::LocalVaultLocked {
                vault_path: vault_path.clone(),
            },
            StorageBackend::Kdbx { file, keyfile, .. }
            | StorageBackend::KdbxLocked { file, keyfile } => StorageBackend::KdbxLocked {
                file: file.clone(),
                keyfile: keyfile.clone(),
            },
            StorageBackend::Keychain => StorageBackend::Keychain,
            StorageBackend::HttpVault { .. } => self.clone(),
        }
    }

    /// Create a brand-new encrypted vault at `vault_path` with
    /// `passphrase`, with a recovery phrase generated. On
    /// success returns the unlocked `LocalVault` backend plus
    /// the recovery phrase the UI must surface exactly once.
    ///
    /// Refuses if a file already exists at `vault_path` — the
    /// caller should have routed to `unlock` instead.
    fn create_vault(
        vault_path: std::path::PathBuf,
        passphrase: secrecy::SecretString,
    ) -> Result<(StorageBackend, Option<String>), String> {
        use devboy_vault_crypto::{InitialUnlock, Vault};
        if vault_path.exists() {
            return Err(format!(
                "a vault file already exists at {} — unlock it instead of creating a new one",
                vault_path.display()
            ));
        }
        if let Some(parent) = vault_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create vault directory: {e}"))?;
        }
        let outcome = Vault::create(
            &vault_path,
            InitialUnlock {
                passphrase: passphrase.clone(),
                with_recovery: true,
                with_keychain_account: None,
                passphrase_params: None,
            },
        )
        .map_err(|e| format!("vault create failed: {e}"))?;
        let recovery = outcome.recovery_phrase.map(|p| p.expose_words().to_owned());
        Ok((
            StorageBackend::LocalVault {
                vault_path,
                passphrase,
            },
            recovery,
        ))
    }

    fn label(&self) -> String {
        match self {
            StorageBackend::Keychain => {
                "macOS Keychain — service `devboy-tools`, account = path".to_owned()
            }
            StorageBackend::LocalVaultLocked { vault_path } => {
                format!(
                    "local-vault (locked) — file `{}`, awaiting passphrase",
                    vault_path.display()
                )
            }
            StorageBackend::LocalVault { vault_path, .. } => {
                format!(
                    "local-vault — file `{}`, XChaCha20-Poly1305 + Argon2id passphrase",
                    vault_path.display()
                )
            }
            StorageBackend::HttpVault {
                addr,
                namespace,
                access,
                ..
            } => {
                let ns = namespace
                    .as_deref()
                    .map(|n| format!(", namespace `{n}`"))
                    .unwrap_or_default();
                let mode = match access {
                    devboy_storage::SourceAccess::Read => "read-only",
                    devboy_storage::SourceAccess::ReadWrite => "read-write",
                };
                format!("HashiCorp Vault — `{addr}`{ns} ({mode})")
            }
            StorageBackend::KdbxLocked { file, .. } => {
                format!(
                    "KDBX (locked) — file `{}`, awaiting passphrase",
                    file.display()
                )
            }
            StorageBackend::Kdbx { file, snapshot, .. } => {
                format!(
                    "KDBX 4 — file `{}`, {} entr{} loaded (read-only)",
                    file.display(),
                    snapshot.entries.len(),
                    if snapshot.entries.len() == 1 {
                        "y"
                    } else {
                        "ies"
                    }
                )
            }
        }
    }

    fn source_label(&self) -> &'static str {
        match self {
            StorageBackend::Keychain => "default-keychain",
            StorageBackend::LocalVaultLocked { .. } | StorageBackend::LocalVault { .. } => {
                "local-vault"
            }
            StorageBackend::HttpVault { .. } => "vault",
            StorageBackend::KdbxLocked { .. } | StorageBackend::Kdbx { .. } => "kdbx",
        }
    }

    /// Probe whether `path` already has a value. Errors collapse
    /// to "not provisioned" to keep the inventory render simple.
    /// A locked vault reports everything as not-provisioned —
    /// the UI gates on the unlock modal before this matters.
    fn has_value(&self, path: &str) -> bool {
        match self {
            StorageBackend::Keychain => {
                let store = devboy_storage::KeychainStore::new();
                matches!(
                    devboy_storage::CredentialStore::get(&store, path),
                    Ok(Some(_))
                )
            }
            StorageBackend::LocalVaultLocked { .. } => false,
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
            StorageBackend::HttpVault { .. } => {
                // `VaultSource::get` is async; block_on it on a
                // tiny current-thread runtime. Any error (no
                // network, bad token, sealed) collapses to
                // "not provisioned" — the same forgiving render
                // contract the other backends follow.
                use devboy_storage::SecretSource as _;
                match self.http_vault_source() {
                    Some(src) => {
                        let reference = self.http_vault_reference(path);
                        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        else {
                            return false;
                        };
                        rt.block_on(async move { matches!(src.get(&reference).await, Ok(Some(_))) })
                    }
                    None => false,
                }
            }
            StorageBackend::KdbxLocked { .. } => false,
            StorageBackend::Kdbx { snapshot, .. } => snapshot
                .entries
                .iter()
                .any(|e| e.path == path && e.password.is_some()),
        }
    }

    /// Build a `VaultSource` from an `HttpVault` backend.
    /// `None` for any other variant (or if the HTTP client
    /// fails to build, which is essentially never).
    fn http_vault_source(&self) -> Option<devboy_secret_vault::VaultSource> {
        let StorageBackend::HttpVault {
            addr,
            namespace,
            token,
            ..
        } = self
        else {
            return None;
        };
        let mut src = devboy_secret_vault::VaultSource::new(
            "http-vault",
            addr.clone(),
            devboy_secret_vault::VaultAuth::Token(token.clone()),
        )
        .ok()?;
        if let Some(ns) = namespace {
            src = src.with_namespace(ns);
        }
        Some(src)
    }

    /// Translate an ADR-020 path into a Vault KV v2 reference:
    /// `<mount>/<path>` (e.g. `secret/data/team/openai/api-key`).
    fn http_vault_reference(&self, path: &str) -> String {
        match self {
            StorageBackend::HttpVault { mount, .. } => {
                format!("{}/{}", mount.trim_end_matches('/'), path)
            }
            // Not an HTTP-vault backend — return the path
            // verbatim; callers only reach this on the HttpVault
            // arm anyway.
            _ => path.to_owned(),
        }
    }

    /// Write `value` to the backend. Creates a vault file if it
    /// doesn't exist (LocalVault). Returns the optional recovery
    /// phrase the user must save (only on first vault create).
    /// A locked vault refuses the write — the UI must unlock it
    /// first.
    fn store(&self, path: &str, value: &secrecy::SecretString) -> Result<Option<String>, String> {
        match self {
            StorageBackend::Keychain => {
                let store = devboy_storage::KeychainStore::new();
                devboy_storage::CredentialStore::store(&store, path, value)
                    .map_err(|e| format!("{e}"))?;
                Ok(None)
            }
            StorageBackend::LocalVaultLocked { .. } => Err(
                "local-vault is locked — unlock it with your passphrase before saving".to_owned(),
            ),
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
            StorageBackend::KdbxLocked { .. } => {
                Err("KDBX file is locked — unlock it with your passphrase before saving".to_owned())
            }
            StorageBackend::Kdbx { .. } => Err(
                "writing to a KDBX file is not implemented yet (read-only MVP) — \
                 edit the entry in KeePass / KeePassXC and reload the inventory"
                    .to_owned(),
            ),
            // HTTP Vault is read-only from the UI today. The
            // `SecretSource` trait has no write surface yet
            // (P15) — refuse honestly rather than silently
            // dropping the value. `access` distinguishes "you
            // chose read-only" from "writes aren't built yet".
            StorageBackend::HttpVault { access, .. } => match access {
                devboy_storage::SourceAccess::Read => Err(
                    "this Vault source is configured read-only (`access = \"read\"`) — \
                     change it to `readwrite` in sources.toml once write support lands"
                        .to_owned(),
                ),
                devboy_storage::SourceAccess::ReadWrite => Err(
                    "writing to HashiCorp Vault from the UI is not implemented yet — \
                     the SecretSource write surface ships in P15. Until then, write the \
                     value with the Vault CLI / UI and devboy will read it back."
                        .to_owned(),
                ),
            },
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

    // KDBX-backed rows: include every decrypted entry whose path
    // isn't already in the manifest. Gives the user a one-shot
    // view of their KeePass DB without needing to write a
    // manifest for it first. The agent never sees these rows —
    // they live entirely inside the UI process snapshot.
    if let StorageBackend::Kdbx { snapshot, .. } = backend {
        for entry in &snapshot.entries {
            // Skip duplicates already covered by the manifest
            // pass — manifest entries take precedence so the
            // user's curated descriptions / rotation hints win.
            if rows.iter().any(|r| r.path == entry.path) {
                continue;
            }
            let scope = entry.path.split('/').next().unwrap_or("kdbx").to_owned();
            let provider_segment = entry.path.split('/').nth(1);
            // KDBX entries are always "provisioned" — they exist
            // in the unlocked snapshot, that's the whole point.
            rows.push(devboy_secrets_ui::InventoryRow {
                path: entry.path.clone(),
                status: devboy_secrets_ui::RowStatus::Provisioned,
                routed_source: Some(backend.source_label().to_owned()),
                expires_at: None,
                provider: provider_segment.map(str::to_owned),
                scope,
                catalog_override: provider_segment.and_then(|p| catalog_override_for(catalogs, p)),
            });
            // Surface KDBX metadata in the index-entry map so
            // the provision dialog's context card can show the
            // KeePass-side Title / UserName / URL / Notes.
            metadata.insert(
                entry.path.clone(),
                devboy_storage::IndexEntry {
                    description: Some(format!("KeePass entry: {}", entry.title)),
                    retrieval_url: entry.url.clone(),
                    env_var: entry.username.clone(),
                    ..devboy_storage::IndexEntry::default()
                },
            );
        }
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
        // First-run onboarding wizard — gates everything on the
        // very first launch. `onboarding` and `vault_unlock`
        // are mutually exclusive (a locked vault means the user
        // already onboarded), so checking onboarding first is
        // safe.
        if self.onboarding.is_some() {
            self.render_onboarding_modal(ui.ctx());
        }

        // Vault unlock / create modal — gates everything else
        // while the backend is a locked local-vault. The
        // `egui::Modal` dims the inventory underneath; the
        // user can't touch a row until they unlock or pick the
        // keychain escape hatch.
        if self.vault_unlock.is_some() {
            self.render_vault_unlock_modal(ui.ctx());
        }

        // Backend banner + switcher — tells the user where
        // saves go, and lets them flip keychain ↔ local-vault
        // live (V5). The switch is session-scoped; the env var
        // stays the durable override.
        let backend_label = self.backend.label();
        let mut switch_to_vault = false;
        let mut lock_vault = false;
        let mut switch_to_keychain = false;
        ui.horizontal(|ui| {
            ui.label(
                eframe::egui::RichText::new(format!("Backend: {backend_label}"))
                    .small()
                    .color(eframe::egui::Color32::from_rgb(0x55, 0xaa, 0xff)),
            );
            match &self.backend {
                StorageBackend::Keychain => {
                    if ui
                        .small_button("Switch to encrypted vault")
                        .on_hover_text(
                            "Use the on-disk XChaCha20-Poly1305 vault instead of the OS keychain",
                        )
                        .clicked()
                    {
                        switch_to_vault = true;
                    }
                }
                StorageBackend::LocalVault { .. } => {
                    if ui
                        .small_button("Lock vault")
                        .on_hover_text("Drop the passphrase; the unlock prompt returns")
                        .clicked()
                    {
                        lock_vault = true;
                    }
                    if ui
                        .small_button("Switch to keychain")
                        .on_hover_text("Use the OS keychain for this session")
                        .clicked()
                    {
                        switch_to_keychain = true;
                    }
                }
                // Locked → the unlock modal is already up; no
                // extra switcher button needed (the modal has
                // its own "Use keychain instead" hatch).
                StorageBackend::LocalVaultLocked { .. } => {}
                // HTTP Vault is configured in sources.toml — the
                // only live switch is back to the keychain for
                // the session.
                StorageBackend::HttpVault { .. } => {
                    if ui
                        .small_button("Switch to keychain")
                        .on_hover_text("Use the OS keychain for this session")
                        .clicked()
                    {
                        switch_to_keychain = true;
                    }
                }
                // KDBX: locked → unlock modal already up. Unlocked
                // → offer "Lock" to drop the passphrase + snapshot
                // and "Switch to keychain" as an escape hatch.
                StorageBackend::KdbxLocked { .. } => {}
                StorageBackend::Kdbx { .. } => {
                    if ui
                        .small_button("Lock KDBX")
                        .on_hover_text("Drop the passphrase + snapshot; the unlock prompt returns")
                        .clicked()
                    {
                        lock_vault = true;
                    }
                    if ui
                        .small_button("Switch to keychain")
                        .on_hover_text("Use the OS keychain for this session")
                        .clicked()
                    {
                        switch_to_keychain = true;
                    }
                }
            }
        });
        if switch_to_vault {
            // Existing `.dvb` → unlock flow; no file yet →
            // create flow.
            let vault_path = default_vault_path();
            let mode = if vault_path.exists() {
                // `unlock` needs a `LocalVaultLocked` backend to
                // act on, so move there first.
                self.backend = StorageBackend::LocalVaultLocked { vault_path };
                devboy_secrets_ui::VaultUnlockMode::Unlock
            } else {
                devboy_secrets_ui::VaultUnlockMode::Create
            };
            self.vault_unlock = Some(devboy_secrets_ui::VaultUnlockState::new(mode));
        }
        if lock_vault {
            self.backend = self.backend.lock();
            self.vault_unlock = Some(devboy_secrets_ui::VaultUnlockState::new(
                devboy_secrets_ui::VaultUnlockMode::Unlock,
            ));
            self.reload();
        }
        if switch_to_keychain {
            self.backend = StorageBackend::Keychain;
            self.reload();
        }
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
        // do not show it again across runs. An explicit
        // "I've saved this" acknowledgement is the gate — the
        // banner stays until the user confirms, so a freshly
        // created vault can't have its only recovery path
        // scrolled away by accident.
        let mut recovery_acknowledged = false;
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
            if ui
                .button("I've saved this phrase — dismiss")
                .on_hover_text("The phrase is shown only once; copy it somewhere safe first")
                .clicked()
            {
                recovery_acknowledged = true;
            }
            ui.separator();
        }
        if recovery_acknowledged {
            self.recovery_phrase_to_show = None;
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
            // Render the dialog as a true modal overlay: a dimmed
            // backdrop over the inventory, focus trapped, ESC /
            // click-outside dismiss it. `egui::Modal` (vs the old
            // `egui::Window`) is the window-in-window primitive —
            // the inventory stays visible underneath instead of
            // being replaced like a route.
            let modal = eframe::egui::Modal::new(eframe::egui::Id::new("provision-modal")).show(
                ui.ctx(),
                |ui| {
                    // Cap the width so the modal reads as a card,
                    // not a full-bleed panel.
                    ui.set_max_width(560.0);
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

                    // The value's masked / unmasked state lives
                    // in `DialogState` now — the provision-dialog
                    // widget renders the eye-toggle right next to
                    // the input (standard password-field UX). No
                    // separate "Show entered value" checkbox, no
                    // echoed `current value: «…»` line here.

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
                    }
                    // The console / docs / rotation-guide links are
                    // real egui hyperlinks now — egui opens the OS
                    // browser itself. No `open_url_clicked` to
                    // broker, no subprocess to spawn.
                },
            );

            // ESC or a click on the dimmed backdrop dismisses the
            // modal — same path as the dialog's Cancel button.
            if modal.should_close() {
                close = true;
            }

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

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;

    /// A vault path inside a fresh tempdir — never collides with
    /// a real `~/.devboy` vault, cleaned up on drop.
    fn temp_vault_path(dir: &tempfile::TempDir) -> std::path::PathBuf {
        dir.path().join("test-vault.dvb")
    }

    // -- StorageBackend state machine --------------------------

    #[test]
    fn is_locked_only_true_for_the_locked_variant() {
        assert!(!StorageBackend::Keychain.is_locked());
        let dir = tempfile::tempdir().unwrap();
        let locked = StorageBackend::LocalVaultLocked {
            vault_path: temp_vault_path(&dir),
        };
        assert!(locked.is_locked());
        let unlocked = StorageBackend::LocalVault {
            vault_path: temp_vault_path(&dir),
            passphrase: SecretString::new("p".to_string().into()),
        };
        assert!(!unlocked.is_locked());
    }

    #[test]
    fn lock_drops_the_passphrase_back_to_locked() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_vault_path(&dir);
        let unlocked = StorageBackend::LocalVault {
            vault_path: path.clone(),
            passphrase: SecretString::new("p".to_string().into()),
        };
        let locked = unlocked.lock();
        assert!(matches!(
            locked,
            StorageBackend::LocalVaultLocked { vault_path } if vault_path == path
        ));
        // Keychain is unaffected by lock.
        assert!(matches!(
            StorageBackend::Keychain.lock(),
            StorageBackend::Keychain
        ));
    }

    // -- create_vault → unlock round-trip ----------------------

    #[test]
    fn create_vault_mints_a_file_and_returns_a_recovery_phrase() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_vault_path(&dir);
        let (backend, recovery) = StorageBackend::create_vault(
            path.clone(),
            SecretString::new("correct-horse".to_string().into()),
        )
        .expect("create must succeed on a fresh path");
        assert!(path.exists(), "the .dvb file must be written");
        assert!(
            matches!(backend, StorageBackend::LocalVault { .. }),
            "create returns an unlocked backend"
        );
        let phrase = recovery.expect("with_recovery: true must yield a phrase");
        assert!(
            phrase.split_whitespace().count() >= 12,
            "recovery phrase should be a multi-word mnemonic, got: {phrase}"
        );
    }

    #[test]
    fn create_vault_refuses_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_vault_path(&dir);
        StorageBackend::create_vault(path.clone(), SecretString::new("first".to_string().into()))
            .unwrap();
        // Second create against the same path must refuse.
        let err =
            StorageBackend::create_vault(path, SecretString::new("second".to_string().into()))
                .unwrap_err();
        assert!(
            err.contains("already exists"),
            "error must explain the file clash, got: {err}"
        );
    }

    #[test]
    fn unlock_opens_a_vault_created_with_the_same_passphrase() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_vault_path(&dir);
        StorageBackend::create_vault(
            path.clone(),
            SecretString::new("the-passphrase".to_string().into()),
        )
        .unwrap();
        // Now resolve as locked and unlock with the right pass.
        let locked = StorageBackend::LocalVaultLocked {
            vault_path: path.clone(),
        };
        let unlocked = locked
            .unlock(SecretString::new("the-passphrase".to_string().into()))
            .expect("correct passphrase must unlock");
        assert!(matches!(
            unlocked,
            StorageBackend::LocalVault { vault_path, .. } if vault_path == path
        ));
    }

    #[test]
    fn unlock_rejects_a_wrong_passphrase() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_vault_path(&dir);
        StorageBackend::create_vault(
            path.clone(),
            SecretString::new("the-real-one".to_string().into()),
        )
        .unwrap();
        let locked = StorageBackend::LocalVaultLocked { vault_path: path };
        let err = locked
            .unlock(SecretString::new("a-wrong-guess".to_string().into()))
            .expect_err("a wrong passphrase must not unlock the vault");
        assert!(
            !err.is_empty(),
            "unlock failure must carry a human-facing reason"
        );
    }

    #[test]
    fn unlock_is_a_no_op_error_on_a_keychain_backend() {
        let err = StorageBackend::Keychain
            .unlock(SecretString::new("p".to_string().into()))
            .expect_err("keychain has nothing to unlock");
        assert!(
            err.contains("does not have a passphrase-locked state"),
            "got: {err}"
        );
    }

    // -- Kdbx backend variants --------------------------------

    #[test]
    fn kdbx_locked_reports_locked_and_refuses_writes() {
        let backend = StorageBackend::KdbxLocked {
            file: std::path::PathBuf::from("/tmp/nonexistent.kdbx"),
            keyfile: None,
        };
        assert!(backend.is_locked());
        assert!(!backend.has_value("any/path"));
        let err = backend
            .store("any/path", &SecretString::new("v".to_string().into()))
            .expect_err("locked KDBX must refuse writes");
        assert!(err.contains("unlock it"), "got: {err}");
    }

    #[test]
    fn kdbx_source_label_is_kdbx_in_both_states() {
        let locked = StorageBackend::KdbxLocked {
            file: std::path::PathBuf::from("/tmp/x.kdbx"),
            keyfile: None,
        };
        assert_eq!(locked.source_label(), "kdbx");
    }

    #[test]
    fn kdbx_lock_round_trip_clears_passphrase_back_to_locked() {
        // Build an unlocked Kdbx variant with an empty snapshot.
        let unlocked = StorageBackend::Kdbx {
            file: std::path::PathBuf::from("/tmp/x.kdbx"),
            keyfile: Some(std::path::PathBuf::from("/tmp/x.keyx")),
            passphrase: SecretString::new("hunter2".to_string().into()),
            snapshot: std::sync::Arc::new(devboy_secret_kdbx::KdbxSnapshot::default()),
        };
        let relocked = unlocked.lock();
        match relocked {
            StorageBackend::KdbxLocked { file, keyfile } => {
                assert_eq!(file, std::path::PathBuf::from("/tmp/x.kdbx"));
                assert_eq!(
                    keyfile.as_deref(),
                    Some(std::path::Path::new("/tmp/x.keyx"))
                );
            }
            other => panic!("lock should land on KdbxLocked, got {other:?}"),
        }
    }

    #[test]
    fn kdbx_unlocked_writes_are_refused_with_read_only_message() {
        let backend = StorageBackend::Kdbx {
            file: std::path::PathBuf::from("/tmp/x.kdbx"),
            keyfile: None,
            passphrase: SecretString::new("p".to_string().into()),
            snapshot: std::sync::Arc::new(devboy_secret_kdbx::KdbxSnapshot::default()),
        };
        let err = backend
            .store("any/path", &SecretString::new("v".to_string().into()))
            .expect_err("KDBX is read-only");
        assert!(err.contains("not implemented yet"), "got: {err}");
    }

    // -- has_value / store on a locked backend -----------------

    #[test]
    fn locked_backend_reports_no_values_and_refuses_writes() {
        let dir = tempfile::tempdir().unwrap();
        let locked = StorageBackend::LocalVaultLocked {
            vault_path: temp_vault_path(&dir),
        };
        // A locked vault reads as empty — the UI gates on the
        // unlock modal before this matters.
        assert!(!locked.has_value("team/openai/api-key"));
        // …and refuses writes with a clear message.
        let err = locked
            .store(
                "team/openai/api-key",
                &SecretString::new("sk-whatever".to_string().into()),
            )
            .expect_err("a locked vault must refuse a write");
        assert!(err.contains("locked"), "got: {err}");
    }

    // -- labels ------------------------------------------------

    #[test]
    fn labels_distinguish_the_three_backend_states() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_vault_path(&dir);
        assert!(StorageBackend::Keychain.label().contains("Keychain"));
        assert!(
            StorageBackend::LocalVaultLocked {
                vault_path: path.clone()
            }
            .label()
            .contains("locked")
        );
        let unlocked = StorageBackend::LocalVault {
            vault_path: path,
            passphrase: SecretString::new("p".to_string().into()),
        };
        let label = unlocked.label();
        assert!(label.contains("local-vault") && !label.contains("locked"));
    }

    #[test]
    fn source_label_collapses_both_vault_states_to_local_vault() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_vault_path(&dir);
        assert_eq!(StorageBackend::Keychain.source_label(), "default-keychain");
        assert_eq!(
            StorageBackend::LocalVaultLocked {
                vault_path: path.clone()
            }
            .source_label(),
            "local-vault"
        );
        assert_eq!(
            StorageBackend::LocalVault {
                vault_path: path,
                passphrase: SecretString::new("p".to_string().into()),
            }
            .source_label(),
            "local-vault"
        );
    }

    // -- StorageBackend::HttpVault (W2) ------------------------

    fn http_vault(access: devboy_storage::SourceAccess) -> StorageBackend {
        StorageBackend::HttpVault {
            addr: "https://vault.example.invalid:8200".to_owned(),
            namespace: Some("admin".to_owned()),
            mount: "secret/data".to_owned(),
            token: SecretString::new("hvs.tok".to_string().into()),
            access,
        }
    }

    #[test]
    fn http_vault_label_surfaces_addr_namespace_and_access_mode() {
        let read = http_vault(devboy_storage::SourceAccess::Read).label();
        assert!(read.contains("HashiCorp Vault"));
        assert!(read.contains("vault.example.invalid"));
        assert!(read.contains("namespace `admin`"));
        assert!(read.contains("read-only"));
        let rw = http_vault(devboy_storage::SourceAccess::ReadWrite).label();
        assert!(rw.contains("read-write"));
    }

    #[test]
    fn http_vault_source_label_is_vault() {
        assert_eq!(
            http_vault(devboy_storage::SourceAccess::ReadWrite).source_label(),
            "vault"
        );
    }

    #[test]
    fn http_vault_reference_prefixes_the_mount() {
        let backend = http_vault(devboy_storage::SourceAccess::ReadWrite);
        assert_eq!(
            backend.http_vault_reference("team/openai/api-key"),
            "secret/data/team/openai/api-key"
        );
    }

    #[test]
    fn http_vault_reference_trims_a_trailing_slash_on_the_mount() {
        let backend = StorageBackend::HttpVault {
            addr: "https://v.invalid".to_owned(),
            namespace: None,
            mount: "secret/data/".to_owned(),
            token: SecretString::new("t".to_string().into()),
            access: devboy_storage::SourceAccess::ReadWrite,
        };
        assert_eq!(
            backend.http_vault_reference("team/x/y"),
            "secret/data/team/x/y"
        );
    }

    #[test]
    fn http_vault_read_access_refuses_a_write_pointing_at_the_config() {
        let err = http_vault(devboy_storage::SourceAccess::Read)
            .store("team/x/y", &SecretString::new("v".to_string().into()))
            .expect_err("read-only HTTP Vault must refuse a write");
        assert!(err.contains("read-only"), "got: {err}");
        assert!(err.contains("sources.toml"), "got: {err}");
    }

    #[test]
    fn http_vault_readwrite_refuses_a_write_pointing_at_p15() {
        // `readwrite` doesn't mean write WORKS yet — the
        // SecretSource write surface ships in P15. The refusal
        // must say so honestly rather than silently dropping
        // the value.
        let err = http_vault(devboy_storage::SourceAccess::ReadWrite)
            .store("team/x/y", &SecretString::new("v".to_string().into()))
            .expect_err("HTTP Vault write is not implemented yet");
        assert!(err.contains("P15"), "got: {err}");
    }

    #[test]
    fn http_vault_source_builds_with_namespace() {
        // `http_vault_source` should produce a `VaultSource`
        // for an HttpVault backend (and `None` for others).
        let backend = http_vault(devboy_storage::SourceAccess::ReadWrite);
        assert!(backend.http_vault_source().is_some());
        assert!(StorageBackend::Keychain.http_vault_source().is_none());
    }

    #[test]
    fn http_vault_lock_is_a_no_op() {
        // The Vault token lives in sources.toml — there is no
        // in-memory unlock state to drop, so `lock` returns the
        // backend unchanged.
        let backend = http_vault(devboy_storage::SourceAccess::Read);
        assert!(matches!(backend.lock(), StorageBackend::HttpVault { .. }));
        assert!(!backend.is_locked());
    }
}
