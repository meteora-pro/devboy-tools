//! Dev-only `--screenshot` mode (feature `dev-screenshot`).
//!
//! Renders a single provision-dialog frame to a PNG through an
//! offscreen `egui_kittest` wgpu harness, then exits — instead
//! of spinning up the eframe event loop and a native window.
//!
//! Why this exists: an automated agent (or a CI visual-diff
//! job) can run
//!
//! ```sh
//! devboy-secrets-ui --screenshot /tmp/dialog.png
//! ```
//!
//! and inspect the PNG, closing the "I can't see the GUI" gap
//! without a human looking at the native window. It renders the
//! exact same `gui::provision_dialog::render` the live app
//! calls, so the screenshot is a faithful preview, not a
//! re-implementation.
//!
//! Gated behind the `dev-screenshot` cargo feature so the
//! shipped binary does not link a second (wgpu) rendering
//! backend.

use std::path::Path;

use anyhow::{Context, Result};
use devboy_secrets_ui::{
    DialogMetadata, DialogMode, DialogState, VaultUnlockMode, VaultUnlockState, VaultUnlockStatus,
};

/// Which view the `--screenshot` mode should render. Selected
/// by `--screenshot-view`; `provision` is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenshotView {
    /// The provision dialog, catalog-matched (`team/openai/api-key`).
    Provision,
    /// The vault unlock modal, `Unlock` mode with a sample
    /// wrong-passphrase failure shown.
    VaultUnlock,
    /// The vault unlock modal, `Create` mode (passphrase +
    /// confirm fields).
    VaultCreate,
}

impl ScreenshotView {
    /// Parse the `--screenshot-view` CLI value.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "provision" => Ok(Self::Provision),
            "unlock" => Ok(Self::VaultUnlock),
            "create" => Ok(Self::VaultCreate),
            other => Err(anyhow::anyhow!(
                "unknown --screenshot-view `{other}` (expected: provision, unlock, create)"
            )),
        }
    }
}

/// Render the requested `view` to a PNG at `out`.
pub fn render_to_png(view: ScreenshotView, out: &Path) -> Result<()> {
    match view {
        ScreenshotView::Provision => render_provision_dialog_to_png(out),
        ScreenshotView::VaultUnlock => render_vault_unlock_to_png(out, VaultUnlockMode::Unlock),
        ScreenshotView::VaultCreate => render_vault_unlock_to_png(out, VaultUnlockMode::Create),
    }
}

/// Render one provision-dialog frame and write it to `out` as a
/// PNG. The dialog is populated with a catalog-matched sample
/// (`team/openai/api-key`) so every catalog-derived block —
/// description, steps, notes, docs link, rotation section —
/// shows up in the preview.
pub fn render_provision_dialog_to_png(out: &Path) -> Result<()> {
    let mut state = DialogState::new(DialogMode::Provision, sample_metadata());

    let mut harness = egui_kittest::Harness::new_ui(move |ui| {
        let _ = devboy_secrets_ui::gui::provision_dialog::render(ui, &mut state);
    });
    harness.run();

    let image = harness
        .render()
        .map_err(|e| anyhow::anyhow!("offscreen wgpu render failed: {e:?}"))?;
    image
        .save(out)
        .with_context(|| format!("could not write screenshot to {}", out.display()))?;
    Ok(())
}

/// Render the vault unlock / create modal to `out` as a PNG.
/// `Unlock` mode is shown with a sample failure message so the
/// error styling is visible; `Create` mode shows the empty
/// passphrase + confirm fields.
fn render_vault_unlock_to_png(out: &Path, mode: VaultUnlockMode) -> Result<()> {
    let mut state = VaultUnlockState::new(mode);
    if mode == VaultUnlockMode::Unlock {
        // Seed a failure so the screenshot exercises the red
        // error line — the most layout-sensitive state.
        state.replace_passphrase_str("wrong-guess".into());
        state.apply_status(VaultUnlockStatus::Failed {
            reason: "wrong passphrase".into(),
        });
    }

    let mut harness = egui_kittest::Harness::new_ui(move |ui| {
        let _ = devboy_secrets_ui::gui::vault_unlock::render(ui, &mut state);
    });
    harness.run();

    let image = harness
        .render()
        .map_err(|e| anyhow::anyhow!("offscreen wgpu render failed: {e:?}"))?;
    image
        .save(out)
        .with_context(|| format!("could not write screenshot to {}", out.display()))?;
    Ok(())
}

/// A representative catalog-matched metadata bundle — mirrors
/// what `catalog_metadata::metadata_from_catalog_and_entry`
/// produces for `team/openai/api-key` against the bundled
/// OpenAI catalog. Kept in sync by hand; the goal is a faithful
/// *visual* preview, not a byte-exact match.
fn sample_metadata() -> DialogMetadata {
    DialogMetadata {
        path: "team/openai/api-key".into(),
        provider: "openai".into(),
        rotation_method: "manual".into(),
        provisioning_url: Some("https://platform.openai.com/api-keys".into()),
        format_hint: Some("starts with sk- (or sk-proj-), 20+ characters".into()),
        description: Some(
            "Personal or workspace key issued at platform.openai.com. The sk-proj-... \
             shape is project-scoped."
                .into(),
        ),
        retrieval_steps: vec![
            "Open https://platform.openai.com/api-keys".into(),
            "Sign in with your OpenAI account (workspace SSO if applicable)".into(),
            "Click Create new secret key".into(),
            "Pick a name + the project / workspace scope".into(),
            "Copy the value — it is shown once and never again".into(),
        ],
        retrieval_notes: Some(
            "Project-scoped keys inherit billing from the parent project and can be \
             revoked independently."
                .into(),
        ),
        docs_url: Some("https://platform.openai.com/docs/api-reference/authentication".into()),
        rotation_guide_url: Some(
            "https://platform.openai.com/docs/guides/production-best-practices".into(),
        ),
        rotation_notes: Some(
            "Create the replacement key first and deploy it, THEN delete the old one. \
             No overlap window — both keys stay live until you delete the old."
                .into(),
        ),
    }
}
