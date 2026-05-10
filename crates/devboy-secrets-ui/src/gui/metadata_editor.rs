//! egui Edit-metadata view — see [`crate::metadata_editor`] for
//! the shared view-model and [ADR-023] §3.4 for the contract.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use crate::metadata_editor::{
    EditorMode, EditorState, EditorStatus, EditorSubmission, MetadataField,
};

#[derive(Debug, Default)]
pub struct EditorFrameResult {
    pub submission: Option<EditorSubmission>,
    pub cancelled: bool,
    pub suggestion_accepted: bool,
    pub suggestion_rejected: bool,
}

pub fn render(ui: &mut egui::Ui, state: &mut EditorState) -> EditorFrameResult {
    let mut out = EditorFrameResult::default();

    ui.heading(format!("Edit metadata — {}", state.path()));

    egui::Grid::new("metadata_form")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            for field in [
                MetadataField::Description,
                MetadataField::RetrievalUrl,
                MetadataField::RotateEveryDays,
                MetadataField::ExpiresAt,
                MetadataField::PatternId,
            ] {
                ui.strong(field.label());
                let mut buf = state.draft_field_clone(field);
                let resp = ui.add(egui::TextEdit::singleline(&mut buf));
                if resp.changed() {
                    state.set_field(field, buf);
                }
                ui.end_row();
            }
        });

    if state.mode() == EditorMode::ApplySuggestion {
        ui.separator();
        ui.label("Suggestion diff");
        egui::Grid::new("metadata_diff")
            .num_columns(3)
            .striped(true)
            .show(ui, |ui| {
                ui.strong("FIELD");
                ui.strong("CURRENT");
                ui.strong("PROPOSED");
                ui.end_row();
                for d in state.diff() {
                    let style = if d.changed {
                        egui::Color32::from_rgb(0xcc, 0xa0, 0x33)
                    } else {
                        egui::Color32::GRAY
                    };
                    ui.colored_label(style, d.field.label());
                    ui.colored_label(
                        style,
                        if d.current.is_empty() {
                            "—"
                        } else {
                            &d.current
                        },
                    );
                    ui.colored_label(
                        style,
                        if d.proposed.is_empty() {
                            "—"
                        } else {
                            &d.proposed
                        },
                    );
                    ui.end_row();
                }
            });
        ui.horizontal(|ui| {
            if ui.button("Accept suggestion").clicked() {
                state.accept_suggestion();
                out.suggestion_accepted = true;
            }
            if ui.button("Reject suggestion").clicked() {
                state.reject_suggestion();
                out.suggestion_rejected = true;
            }
        });
    }

    ui.separator();

    ui.horizontal(|ui| {
        if ui.button("Save").clicked() {
            out.submission = state.save();
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
#[allow(clippy::items_after_test_module)] // helpers stay below for readability
mod tests {
    use super::*;
    use crate::metadata_editor::{EditorState, MetadataDraft};

    fn fixture() -> EditorState {
        EditorState::new(
            "team/jira/api-key",
            MetadataDraft {
                description: Some("Jira API key".into()),
                retrieval_url: Some("https://example.invalid/x".into()),
                rotate_every_days: Some("90".into()),
                expires_at: Some("2026-12-01".into()),
                pattern_id: Some("jira-api-token".into()),
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

fn render_status_line(ui: &mut egui::Ui, status: &EditorStatus) {
    match status {
        EditorStatus::Idle => {}
        EditorStatus::Saving => {
            ui.label("saving…");
        }
        EditorStatus::Saved => {
            ui.colored_label(egui::Color32::from_rgb(0x55, 0xaa, 0x55), "saved");
        }
        EditorStatus::ValidationFailed { field, reason } => {
            ui.colored_label(
                egui::Color32::from_rgb(0xcc, 0x44, 0x44),
                format!("validation failed [{}]: {reason}", field.label()),
            );
        }
        EditorStatus::Cancelled => {
            ui.label("cancelled");
        }
    }
}
