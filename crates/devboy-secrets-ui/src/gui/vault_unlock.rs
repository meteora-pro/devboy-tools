//! egui vault unlock / create modal — see
//! [`crate::vault_unlock`] for the shared view-model.
//!
//! Thin render over [`VaultUnlockState`]. The binary crate
//! shows this inside an `egui::Modal` at startup when the
//! resolved backend is a locked local-vault, and owns the
//! actual `Vault::open` / `Vault::create` call.

use crate::vault_unlock::{
    VaultUnlockFocus, VaultUnlockFrameResult, VaultUnlockMode, VaultUnlockState, VaultUnlockStatus,
};

/// Render the unlock / create modal. Returns the user's
/// intent for this frame ([`VaultUnlockFrameResult`]).
///
/// Layout, top to bottom:
///
/// 1. heading — "Unlock local vault" / "Create encrypted local vault"
/// 2. one-line explanation of what's being unlocked / created
/// 3. passphrase input + eye-toggle
/// 4. confirm-passphrase input (Create mode only)
/// 5. status line — the failure reason in red, when present
/// 6. actions — primary submit + "Use keychain instead" escape hatch
pub fn render(ui: &mut egui::Ui, state: &mut VaultUnlockState) -> VaultUnlockFrameResult {
    let mut out = VaultUnlockFrameResult::default();
    let mode = state.mode();

    // 1. Heading.
    ui.heading(mode.title());

    // 2. Explanation — what this modal is gating.
    let blurb = match mode {
        VaultUnlockMode::Unlock => {
            "Your secrets are stored in an encrypted local vault \
             (XChaCha20-Poly1305 + Argon2id). Enter the passphrase to unlock it."
        }
        VaultUnlockMode::Create => {
            "No local vault exists yet. Pick a passphrase to create one — \
             you'll get a recovery phrase to save before the vault opens."
        }
    };
    ui.label(egui::RichText::new(blurb).small().weak());
    ui.add_space(6.0);

    let revealed = state.reveal();
    let mut toggle_reveal = false;

    // 3. Passphrase input + eye-toggle. The eye-toggle drives
    //    `state.reveal()`, shared by both fields.
    ui.horizontal(|ui| {
        ui.label("Passphrase");
        let mut buf = state.passphrase_clone_for_edit();
        let resp = ui.add(
            egui::TextEdit::singleline(&mut buf)
                .password(!revealed)
                .hint_text("vault passphrase")
                .desired_width(280.0),
        );
        if resp.changed() {
            state.replace_passphrase_str(buf);
        }
        if resp.gained_focus() {
            state.focus_field(VaultUnlockFocus::Passphrase);
        }
        let (glyph, hover) = if revealed {
            ("🙈", "Hide passphrase")
        } else {
            ("👁", "Show passphrase")
        };
        if ui.button(glyph).on_hover_text(hover).clicked() {
            toggle_reveal = true;
        }
    });

    // 4. Confirm field — Create mode only. Same eye-toggle
    //    state; a mismatch is surfaced by `validate()` on
    //    submit, not live, to avoid flagging a half-typed
    //    confirmation.
    if mode.needs_confirm() {
        ui.horizontal(|ui| {
            ui.label("Confirm   ");
            let mut buf = state.confirm_clone_for_edit();
            let resp = ui.add(
                egui::TextEdit::singleline(&mut buf)
                    .password(!revealed)
                    .hint_text("re-enter the passphrase")
                    .desired_width(280.0),
            );
            if resp.changed() {
                state.replace_confirm_str(buf);
            }
            if resp.gained_focus() {
                state.focus_field(VaultUnlockFocus::Confirm);
            }
        });
    }

    if toggle_reveal {
        state.toggle_reveal();
    }

    ui.label(
        egui::RichText::new(format!(
            "{} · {} chars",
            if revealed { "shown" } else { "hidden" },
            state.passphrase_len(),
        ))
        .small()
        .weak(),
    );

    // 5. Status line — only the failure reason is loud; the
    //    other states are quiet.
    match state.status() {
        VaultUnlockStatus::Idle => {}
        VaultUnlockStatus::Working => {
            ui.label("working…");
        }
        VaultUnlockStatus::Unlocked => {
            ui.colored_label(egui::Color32::from_rgb(0x55, 0xaa, 0x55), "unlocked");
        }
        VaultUnlockStatus::Failed { reason } => {
            ui.colored_label(
                egui::Color32::from_rgb(0xcc, 0x44, 0x44),
                format!("✗ {reason}"),
            );
        }
    }

    ui.separator();

    // 6. Actions. The primary button runs the local guard
    //    (`validate`) first — only a clean result flips
    //    `out.submit`, so the binary never gets handed an
    //    empty / mismatched passphrase.
    ui.horizontal(|ui| {
        let working = matches!(state.status(), VaultUnlockStatus::Working);
        if ui
            .add_enabled(!working, egui::Button::new(mode.submit_label()))
            .clicked()
        {
            match state.validate() {
                Ok(()) => out.submit = true,
                Err(reason) => {
                    state.apply_status(VaultUnlockStatus::Failed { reason });
                }
            }
        }
        // Escape hatch — fall back to the OS keychain for this
        // session instead of unlocking the vault.
        if ui
            .button("Use keychain instead")
            .on_hover_text("Skip the vault and use the OS keychain for this session")
            .clicked()
        {
            out.use_keychain = true;
        }
    });

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_unlock_mode_does_not_panic() {
        let mut state = VaultUnlockState::new(VaultUnlockMode::Unlock);
        egui::__run_test_ui(|ui| {
            let _ = render(ui, &mut state);
        });
    }

    #[test]
    fn render_create_mode_does_not_panic() {
        let mut state = VaultUnlockState::new(VaultUnlockMode::Create);
        egui::__run_test_ui(|ui| {
            let _ = render(ui, &mut state);
        });
    }

    #[test]
    fn render_failed_status_does_not_panic() {
        let mut state = VaultUnlockState::new(VaultUnlockMode::Unlock);
        state.apply_status(VaultUnlockStatus::Failed {
            reason: "wrong passphrase".into(),
        });
        egui::__run_test_ui(|ui| {
            let _ = render(ui, &mut state);
        });
    }

    #[test]
    fn render_with_revealed_passphrase_does_not_panic() {
        let mut state = VaultUnlockState::new(VaultUnlockMode::Create);
        state.replace_passphrase_str("hunter2".into());
        state.replace_confirm_str("hunter2".into());
        state.toggle_reveal();
        assert!(state.reveal());
        egui::__run_test_ui(|ui| {
            let _ = render(ui, &mut state);
        });
    }
}
