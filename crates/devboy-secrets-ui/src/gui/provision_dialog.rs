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
///
/// Note: there is no `open_url_clicked` field — the console /
/// docs / rotation-guide links are real `egui` hyperlinks that
/// hand the click straight to the OS browser via
/// `ui.ctx().open_url`. The caller does not have to broker the
/// browser launch.
#[derive(Debug, Default)]
pub struct DialogFrameResult {
    pub submission: Option<DialogSubmission>,
    pub cancelled: bool,
}

/// Render the provision / rotation dialog.
///
/// Information hierarchy, top to bottom — most-actionable
/// first:
///
/// 1. **heading** — what the dialog does.
/// 2. **metadata grid** — PATH / VIA / FORMAT for context.
///    ROTATION cadence shows only in Rotation mode (it's noise
///    when first provisioning).
/// 3. **description** — what this credential variant is.
/// 4. **links row** — "Open console" + "Provider docs", both
///    real hyperlinks, placed *above* the steps so the user
///    sees where to go before reading how.
/// 5. **how-to-obtain steps** — the numbered procedure.
/// 6. **note** — retrieval caveat.
/// 7. **rotation section** — Rotation mode ONLY. For a
///    first-time Provision the "how to rotate" guidance is
///    irrelevant clutter.
/// 8. **value input** — the actual action.
/// 9. **confirm checkbox** — Rotation mode only.
/// 10. **Validate & save / Cancel**.
/// 11. **status line**.
pub fn render(ui: &mut egui::Ui, state: &mut DialogState) -> DialogFrameResult {
    let mut out = DialogFrameResult::default();
    let mode = state.mode();
    let meta = state.metadata().clone();

    // 1. Heading.
    ui.heading(mode.title());

    // 2. Metadata grid — context, kept compact. The ROTATION
    //    cadence row is Rotation-mode only.
    egui::Grid::new("provision_meta")
        .num_columns(2)
        .show(ui, |ui| {
            ui.strong("PATH");
            ui.label(meta.path.as_str());
            ui.end_row();
            ui.strong("VIA");
            ui.label(meta.provider.as_str());
            ui.end_row();
            if mode == DialogMode::Rotation {
                ui.strong("ROTATION");
                ui.label(meta.rotation_method.as_str());
                ui.end_row();
            }
            if let Some(hint) = meta.format_hint.as_deref() {
                ui.strong("FORMAT");
                ui.label(hint);
                ui.end_row();
            }
        });

    // 3. Variant description — what the user is about to store.
    if let Some(desc) = meta.description.as_deref() {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(desc).italics());
    }

    // 4. Links row — actionable, so it sits above the steps.
    //    Both are real hyperlinks: egui hands the click to the
    //    OS browser directly (no caller round-trip). "Open
    //    console" is the creation page; "Provider docs" is the
    //    scopes / best-practices doc.
    if meta.provisioning_url.is_some() || meta.docs_url.is_some() {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if let Some(url) = meta.provisioning_url.as_deref() {
                ui.hyperlink_to("Open console ↗", url);
            }
            if let Some(url) = meta.docs_url.as_deref() {
                if meta.provisioning_url.is_some() {
                    ui.separator();
                }
                ui.hyperlink_to("Provider docs ↗", url);
            }
        });
    }

    // 5. Numbered retrieval procedure.
    if !meta.retrieval_steps.is_empty() {
        ui.add_space(4.0);
        ui.label(egui::RichText::new("How to obtain:").strong());
        for (i, step) in meta.retrieval_steps.iter().enumerate() {
            ui.label(format!("  {}. {step}", i + 1));
        }
    }

    // 6. Caveat / pro-tip — small weak text after the steps.
    if let Some(notes) = meta.retrieval_notes.as_deref() {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(format!("Note: {notes}")).small().weak());
    }

    // 7. Rotation section — Rotation mode ONLY. When first
    //    provisioning, "how to rotate later" is noise; it
    //    belongs on the rotation flow, not here.
    if mode == DialogMode::Rotation
        && (meta.rotation_notes.is_some() || meta.rotation_guide_url.is_some())
    {
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Rotating this secret:").strong());
        if let Some(notes) = meta.rotation_notes.as_deref() {
            ui.label(notes);
        }
        if let Some(url) = meta.rotation_guide_url.as_deref() {
            ui.hyperlink_to("Rotation guide ↗", url);
        }
    }

    ui.separator();

    // 8. Hidden value input — the actual action. egui's
    //    password mode masks it; the buffer round-trips through
    //    `value_clone_for_edit` / `replace_value_str` so the
    //    canonical store stays a `SecretString`.
    let mut buf = state.value_clone_for_edit();
    let resp = ui.add(
        egui::TextEdit::singleline(&mut buf)
            .password(true)
            .hint_text("paste the secret value"),
    );
    if resp.changed() {
        state.replace_value_str(buf);
    }
    ui.label(
        egui::RichText::new(format!("(hidden, {} chars)", state.value_len()))
            .small()
            .weak(),
    );

    // 9. Destructive-confirm checkbox — Rotation mode only.
    if mode == DialogMode::Rotation {
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

    // 10. Primary / secondary actions.
    ui.horizontal(|ui| {
        if ui.button("Validate & save").clicked() {
            out.submission = state.submit();
        }
        if ui.button("Cancel").clicked() {
            state.cancel();
            out.cancelled = true;
        }
    });

    // 11. Status line.
    render_status_line(ui, state.status());

    out
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // helpers stay below for readability
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
                ..DialogMetadata::default()
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
