//! egui first-run onboarding wizard — see
//! [`crate::onboarding`] for the shared view-model.
//!
//! Thin render over [`OnboardingState`]. The binary shows this
//! inside an `egui::Modal` on first run (no `sources.toml`, no
//! vault file) and owns the `sources.toml` write + the
//! `Vault::create` call.

use crate::onboarding::{
    OnboardingAccess, OnboardingFrameResult, OnboardingProvider, OnboardingState, OnboardingStatus,
};

/// Render the onboarding wizard. Returns the user's intent for
/// this frame ([`OnboardingFrameResult`]).
///
/// Layout, top to bottom:
///
/// 1. heading + intro blurb
/// 2. three provider cards (checkbox each); ticking local-vault
///    or http-vault reveals that backend's fields
/// 3. "Primary backend" radio over the selected providers
/// 4. status line — the failure reason in red, when present
/// 5. actions — "Finish setup" + "Skip — use keychain"
pub fn render(ui: &mut egui::Ui, state: &mut OnboardingState) -> OnboardingFrameResult {
    let mut out = OnboardingFrameResult::default();

    // 1. Heading + intro.
    ui.heading("Set up devboy secrets");
    ui.label(
        egui::RichText::new(
            "Pick where your secrets live. You can combine backends — \
             one is marked primary and drives the UI until multi-source \
             routing lands.",
        )
        .small()
        .weak(),
    );
    ui.add_space(8.0);

    let revealed = state.reveal();
    let mut toggle_reveal = false;

    // 2. Provider cards.

    // -- keychain --------------------------------------------
    let mut keychain = state.keychain_selected();
    if ui
        .checkbox(&mut keychain, "OS keychain")
        .on_hover_text("Already there, biometric unlock, zero config")
        .changed()
    {
        state.toggle_provider(OnboardingProvider::Keychain, keychain);
    }

    // -- local-vault -----------------------------------------
    let mut local_vault = state.local_vault_selected();
    if ui
        .checkbox(&mut local_vault, "Encrypted local vault")
        .on_hover_text("On-disk XChaCha20-Poly1305 + Argon2id vault file")
        .changed()
    {
        state.toggle_provider(OnboardingProvider::LocalVault, local_vault);
    }
    if state.local_vault_selected() {
        ui.indent("onboarding-local-vault", |ui| {
            ui.horizontal(|ui| {
                ui.label("Passphrase");
                let mut buf = state.local_passphrase_clone_for_edit();
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut buf)
                            .password(!revealed)
                            .hint_text("new vault passphrase")
                            .desired_width(240.0),
                    )
                    .changed()
                {
                    state.replace_local_passphrase_str(buf);
                }
                let (glyph, hover) = if revealed {
                    ("🙈", "Hide")
                } else {
                    ("👁", "Show")
                };
                if ui.button(glyph).on_hover_text(hover).clicked() {
                    toggle_reveal = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Confirm   ");
                let mut buf = state.local_confirm_clone_for_edit();
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut buf)
                            .password(!revealed)
                            .hint_text("re-enter the passphrase")
                            .desired_width(240.0),
                    )
                    .changed()
                {
                    state.replace_local_confirm_str(buf);
                }
            });
        });
    }

    // -- http-vault (HCP) ------------------------------------
    let mut http_vault = state.http_vault_selected();
    if ui
        .checkbox(&mut http_vault, "HashiCorp Cloud Vault")
        .on_hover_text("Remote Vault over the HTTP KV v2 API")
        .changed()
    {
        state.toggle_provider(OnboardingProvider::HttpVault, http_vault);
    }
    if state.http_vault_selected() {
        ui.indent("onboarding-http-vault", |ui| {
            ui.horizontal(|ui| {
                ui.label("Address  ");
                let mut buf = state.http_addr().to_owned();
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut buf)
                            .hint_text("https://<cluster>.hashicorp.cloud:8200")
                            .desired_width(320.0),
                    )
                    .changed()
                {
                    state.set_http_addr(buf);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Namespace");
                let mut buf = state.http_namespace().to_owned();
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut buf)
                            .hint_text("admin (HCP) — leave blank for OSS Vault")
                            .desired_width(320.0),
                    )
                    .changed()
                {
                    state.set_http_namespace(buf);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Mount    ");
                let mut buf = state.http_mount().to_owned();
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut buf)
                            .hint_text("secret/data")
                            .desired_width(320.0),
                    )
                    .changed()
                {
                    state.set_http_mount(buf);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Token    ");
                let mut buf = state.http_token_clone_for_edit();
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut buf)
                            .password(!revealed)
                            .hint_text("hvs.… Vault token")
                            .desired_width(320.0),
                    )
                    .changed()
                {
                    state.replace_http_token_str(buf);
                }
            });
            // Access radio — read vs read-write. The wizard
            // notes that write-back isn't built yet (P15) so
            // `readwrite` is forward-looking.
            ui.horizontal(|ui| {
                ui.label("Access   ");
                let mut access = state.http_access();
                if ui
                    .radio_value(&mut access, OnboardingAccess::Read, "read-only")
                    .changed()
                {
                    state.set_http_access(access);
                }
                if ui
                    .radio_value(&mut access, OnboardingAccess::ReadWrite, "read-write")
                    .on_hover_text("write-back ships in P15; read works today either way")
                    .changed()
                {
                    state.set_http_access(access);
                }
            });
        });
    }

    if toggle_reveal {
        state.toggle_reveal();
    }

    ui.add_space(6.0);
    ui.separator();

    // 3. Primary-backend radio — only the ticked providers.
    let selected = state.selected_providers();
    if selected.len() > 1 {
        ui.label(
            egui::RichText::new(
                "Primary backend — the one the UI works against until multi-source routing lands:",
            )
            .small(),
        );
        let mut primary = state.primary();
        for provider in &selected {
            if ui
                .radio_value(&mut primary, *provider, provider.label())
                .changed()
            {
                state.set_primary(primary);
            }
        }
        ui.separator();
    }

    // 4. Status line.
    match state.status() {
        OnboardingStatus::Idle => {}
        OnboardingStatus::Working => {
            ui.label("working…");
        }
        OnboardingStatus::Done => {
            ui.colored_label(egui::Color32::from_rgb(0x55, 0xaa, 0x55), "done");
        }
        OnboardingStatus::Failed { reason } => {
            ui.colored_label(
                egui::Color32::from_rgb(0xcc, 0x44, 0x44),
                format!("✗ {reason}"),
            );
        }
    }

    // 5. Actions. "Finish setup" runs the local guard first —
    //    only a clean `validate()` flips `out.finish`.
    ui.horizontal(|ui| {
        let working = matches!(state.status(), OnboardingStatus::Working);
        if ui
            .add_enabled(!working, egui::Button::new("Finish setup"))
            .clicked()
        {
            match state.validate() {
                Ok(()) => out.finish = true,
                Err(reason) => {
                    state.apply_status(OnboardingStatus::Failed { reason });
                }
            }
        }
        if ui
            .button("Skip — use keychain")
            .on_hover_text("Go straight to the OS keychain, write no config")
            .clicked()
        {
            out.skip = true;
        }
    });

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_keychain_only_does_not_panic() {
        let mut state = OnboardingState::new();
        egui::__run_test_ui(|ui| {
            let _ = render(ui, &mut state);
        });
    }

    #[test]
    fn render_with_local_vault_selected_does_not_panic() {
        let mut state = OnboardingState::new();
        state.toggle_provider(OnboardingProvider::LocalVault, true);
        state.replace_local_passphrase_str("pass".into());
        egui::__run_test_ui(|ui| {
            let _ = render(ui, &mut state);
        });
    }

    #[test]
    fn render_with_http_vault_selected_does_not_panic() {
        let mut state = OnboardingState::new();
        state.toggle_provider(OnboardingProvider::HttpVault, true);
        state.set_http_addr("https://vault.example.invalid".into());
        state.replace_http_token_str("hvs.tok".into());
        egui::__run_test_ui(|ui| {
            let _ = render(ui, &mut state);
        });
    }

    #[test]
    fn render_all_three_selected_does_not_panic() {
        let mut state = OnboardingState::new();
        state.toggle_provider(OnboardingProvider::LocalVault, true);
        state.toggle_provider(OnboardingProvider::HttpVault, true);
        egui::__run_test_ui(|ui| {
            let _ = render(ui, &mut state);
        });
    }

    #[test]
    fn render_failed_status_does_not_panic() {
        let mut state = OnboardingState::new();
        state.apply_status(OnboardingStatus::Failed {
            reason: "pick at least one backend".into(),
        });
        egui::__run_test_ui(|ui| {
            let _ = render(ui, &mut state);
        });
    }

    #[test]
    fn render_with_revealed_secrets_does_not_panic() {
        let mut state = OnboardingState::new();
        state.toggle_provider(OnboardingProvider::LocalVault, true);
        state.toggle_provider(OnboardingProvider::HttpVault, true);
        state.replace_local_passphrase_str("pass".into());
        state.replace_http_token_str("hvs.tok".into());
        state.toggle_reveal();
        assert!(state.reveal());
        egui::__run_test_ui(|ui| {
            let _ = render(ui, &mut state);
        });
    }
}
