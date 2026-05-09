//! egui Discovery-import view — see [`crate::discovery_import`]
//! for the shared view-model and [ADR-023] §3.4 for the
//! contract.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use crate::discovery_import::{ImportRowStatus, ImportState, ImportSubmission};

#[derive(Debug, Default)]
pub struct ImportFrameResult {
    /// Populated when the user clicked `Apply N confirmed`.
    pub batch: Option<Vec<ImportSubmission>>,
}

pub fn render(ui: &mut egui::Ui, state: &mut ImportState) -> ImportFrameResult {
    let mut out = ImportFrameResult::default();

    ui.heading(format!(
        "Discovery import — {} confirmed / {} total",
        state.confirmed_count(),
        state.rows().len()
    ));

    ui.separator();

    let snapshot: Vec<_> = state
        .rows()
        .iter()
        .enumerate()
        .map(|(i, r)| {
            (
                i,
                r.item.source_id.clone(),
                r.item.label.clone(),
                r.edited_path.clone(),
                r.status.clone(),
            )
        })
        .collect();

    let selected = state.selected();
    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            egui::Grid::new("import_table")
                .striped(true)
                .num_columns(4)
                .show(ui, |ui| {
                    ui.strong("source");
                    ui.strong("discovered");
                    ui.strong("registered path");
                    ui.strong("status");
                    ui.end_row();
                    for (i, source_id, label, edited_path, status) in &snapshot {
                        let row_selected = *i == selected;
                        if ui
                            .selectable_label(row_selected, source_id.as_str())
                            .clicked()
                        {
                            state.set_selected(*i);
                        }
                        ui.label(label.as_str());
                        ui.label(edited_path.as_str());
                        let (text, color) = status_visual(status);
                        ui.colored_label(color, text);
                        ui.end_row();
                    }
                });
        });

    ui.separator();

    // Edit-path field for the currently selected row. Bound to
    // a temporary buffer so the underlying state classifier
    // re-runs on each keystroke.
    let mut edit_buf = state
        .selected_row()
        .map(|r| r.edited_path.clone())
        .unwrap_or_default();
    let resp = ui.add(egui::TextEdit::singleline(&mut edit_buf).hint_text("registered path"));
    if resp.changed() {
        state.set_path(edit_buf);
    }

    ui.horizontal(|ui| {
        if ui.button("Confirm").clicked() {
            state.confirm_selected();
        }
        if ui.button("Skip").clicked() {
            state.skip_selected();
        }
        if ui.button("Unmark").clicked() {
            state.unmark_selected();
        }
        if ui
            .button(format!("Apply {} confirmed", state.confirmed_count()))
            .clicked()
        {
            out.batch = Some(state.submissions());
        }
    });

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery_import::{DiscoveryItem, ImportState};

    fn fixture() -> ImportState {
        let mut s = ImportState::new(Vec::new());
        s.replace_items(vec![DiscoveryItem {
            source_id: "1password".into(),
            source_path: "op://Private/jira-api-key".into(),
            label: "Jira API key".into(),
            hint: None,
        }]);
        s
    }

    #[test]
    fn render_does_not_panic_in_headless_egui_context() {
        let mut state = fixture();
        egui::__run_test_ui(|ui| {
            let _ = render(ui, &mut state);
        });
    }
}

fn status_visual(status: &ImportRowStatus) -> (String, egui::Color32) {
    match status {
        ImportRowStatus::Pending => ("pending".into(), egui::Color32::GRAY),
        ImportRowStatus::Confirmed => (
            "confirmed".into(),
            egui::Color32::from_rgb(0x55, 0xaa, 0x55),
        ),
        ImportRowStatus::Skipped => ("skipped".into(), egui::Color32::DARK_GRAY),
        ImportRowStatus::Conflict { reason } => (
            format!("conflict: {reason}"),
            egui::Color32::from_rgb(0xcc, 0x44, 0x44),
        ),
    }
}
