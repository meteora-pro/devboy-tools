//! egui Provision/rotation dialog — see
//! [`crate::provision_dialog`] for the shared view-model and
//! [ADR-023] §3.4 for the contract.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use crate::provision_dialog::{DialogMode, DialogState, DialogStatus, DialogSubmission};

/// Render result of one frame. `submission` is `Some` when the
/// user clicked `Validate & save` and the guards passed; the
/// caller hands it to the daemon. `cancelled` flips when the
/// user clicked Cancel.
#[derive(Debug, Default)]
pub struct DialogFrameResult {
    pub submission: Option<DialogSubmission>,
    pub cancelled: bool,
    pub open_url_clicked: bool,
}

pub fn render(ui: &mut egui::Ui, state: &mut DialogState) -> DialogFrameResult {
    let mut out = DialogFrameResult::default();

    ui.heading(state.mode().title());

    let meta = state.metadata().clone();
    egui::Grid::new("provision_meta")
        .num_columns(2)
        .show(ui, |ui| {
            ui.strong("PATH");
            ui.label(meta.path.as_str());
            ui.end_row();
            ui.strong("VIA");
            ui.label(meta.provider.as_str());
            ui.end_row();
            ui.strong("ROTATION");
            ui.label(meta.rotation_method.as_str());
            ui.end_row();
            if let Some(hint) = meta.format_hint.as_deref() {
                ui.strong("FORMAT");
                ui.label(hint);
                ui.end_row();
            }
        });

    ui.separator();

    let url_enabled = meta.provisioning_url.is_some();
    if ui
        .add_enabled(url_enabled, egui::Button::new("Open URL"))
        .clicked()
    {
        out.open_url_clicked = true;
    }

    // Hidden value input — egui's password mode handles the
    // visual masking. The buffer round-trips through
    // `value_clone_for_edit` / `replace_value_str` so the
    // canonical store stays a `SecretString`.
    let mut buf = state.value_clone_for_edit();
    let resp = ui.add(
        egui::TextEdit::singleline(&mut buf)
            .password(true)
            .hint_text("value"),
    );
    if resp.changed() {
        state.replace_value_str(buf);
    }
    ui.label(format!("(hidden, {} chars)", state.value_len()));

    if state.mode() == DialogMode::Rotation {
        let mut checked = state.confirm_checked();
        if ui
            .checkbox(
                &mut checked,
                "I understand this overwrites the current secret",
            )
            .changed()
            && checked != state.confirm_checked()
        {
            state.toggle_confirm();
        }
    }

    ui.separator();

    ui.horizontal(|ui| {
        if ui.button("Validate & save").clicked() {
            out.submission = state.submit();
        }
        if ui.button("Cancel").clicked() {
            state.cancel();
            out.cancelled = true;
        }
    });

    render_status_line(ui, state.status());

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provision_dialog::{DialogMetadata, DialogState};

    fn fixture() -> DialogState {
        DialogState::new(
            DialogMode::Provision,
            DialogMetadata {
                path: "team/jira/api-key".into(),
                provider: "1password".into(),
                rotation_method: "provider-ui".into(),
                provisioning_url: Some("https://example.invalid/x".into()),
                format_hint: Some("Bearer 36 chars".into()),
            },
        )
    }

    #[test]
    fn render_does_not_panic_in_headless_egui_context() {
        let mut state = fixture();
        egui::__run_test_ui(|ui| {
            let _ = render(ui, &mut state);
        });
    }
}

fn render_status_line(ui: &mut egui::Ui, status: &DialogStatus) {
    match status {
        DialogStatus::Idle => {}
        DialogStatus::Submitting => {
            ui.label("submitting…");
        }
        DialogStatus::Saved => {
            ui.colored_label(egui::Color32::from_rgb(0x55, 0xaa, 0x55), "saved");
        }
        DialogStatus::ValidationFailed { reason } => {
            ui.colored_label(
                egui::Color32::from_rgb(0xcc, 0x44, 0x44),
                format!("validation failed: {reason}"),
            );
        }
        DialogStatus::Cancelled => {
            ui.label("cancelled");
        }
    }
}
