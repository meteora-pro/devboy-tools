//! egui Inventory view — see [`crate::inventory`] for the
//! shared view-model and [ADR-023] §3.4 for the contract.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use crate::inventory::{InventoryState, RowStatus, SortKey};

/// Render the inventory into the supplied [`egui::Ui`]. Mutates
/// the state directly via clicks (row selection, sort key
/// changes, filter changes).
pub fn render(ui: &mut egui::Ui, state: &mut InventoryState) {
    ui.heading("Secrets — inventory");
    ui.label(state.daemon_status().label());

    ui.separator();

    render_filters(ui, state);

    ui.separator();

    render_table(ui, state);
}

fn render_filters(ui: &mut egui::Ui, state: &mut InventoryState) {
    ui.horizontal(|ui| {
        ui.label("Sort:");
        let mut current = state.sort_key();
        for (key, label) in [
            (SortKey::ExpiresAt, "expires_at"),
            (SortKey::Path, "path"),
            (SortKey::Status, "status"),
        ] {
            if ui.selectable_label(current == key, label).clicked() {
                state.set_sort_key(key);
                current = key;
            }
        }
    });

    ui.horizontal(|ui| {
        ui.label("Filters:");
        if ui.button("clear all").clicked() {
            state.clear_filters();
        }
    });
}

fn render_table(ui: &mut egui::Ui, state: &mut InventoryState) {
    let rows: Vec<_> = state
        .visible_rows()
        .iter()
        .map(|r| {
            (
                r.path.clone(),
                r.status,
                r.routed_source.clone(),
                r.expires_at.clone(),
                r.provider.clone(),
                r.scope.clone(),
                r.catalog_override.clone(),
            )
        })
        .collect();

    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            egui::Grid::new("inventory_table")
                .striped(true)
                .num_columns(6)
                .show(ui, |ui| {
                    ui.strong("path");
                    ui.strong("status");
                    ui.strong("via");
                    ui.strong("expires_at");
                    ui.strong("provider");
                    ui.strong("scope");
                    ui.end_row();

                    let selected = state.selected();
                    for (i, (path, status, routed, expires, provider, scope, catalog_override)) in
                        rows.iter().enumerate()
                    {
                        let row_selected = i == selected;
                        // Path cell: selectable label + optional
                        // catalog-override chip rendered inline so
                        // a team-pinned override is visible without
                        // opening the row's dialog (P22.2).
                        ui.horizontal(|ui| {
                            let label = ui.selectable_label(row_selected, path.as_str());
                            if label.clicked() {
                                state.set_selected(i);
                            }
                            if let Some(badge) = catalog_override.as_deref() {
                                ui.label(catalog_override_chip(badge));
                            }
                        });
                        ui.colored_label(status_color(*status), status.label());
                        ui.label(routed.as_deref().unwrap_or("—"));
                        ui.label(expires.as_deref().unwrap_or("—"));
                        ui.label(provider.as_deref().unwrap_or("—"));
                        ui.label(scope.as_str());
                        ui.end_row();
                    }
                });
        });
}

/// Map a free-form catalog-override badge (`"user"`, `"project"`,
/// `"url:host"`) to a coloured `RichText` chip. Mirrors the
/// dialog-side `catalog_source_chip` palette (P20.5 / P23.1) so
/// the visual language is consistent across both views.
fn catalog_override_chip(badge: &str) -> egui::RichText {
    let kind = badge.split(':').next().unwrap_or(badge);
    let color = match kind {
        "user" => egui::Color32::from_rgb(0x55, 0xa0, 0xcc),
        "project" => egui::Color32::from_rgb(0x55, 0xaa, 0x55),
        "url" => egui::Color32::from_rgb(0xdd, 0x88, 0x33),
        _ => egui::Color32::from_rgb(0x99, 0x99, 0x99),
    };
    egui::RichText::new(format!("[{badge}]"))
        .small()
        .color(color)
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // status_color / catalog_override_chip stay below for readability
mod tests {
    use super::*;
    use crate::inventory::{DaemonStatus, InventoryRow, InventoryState, RowStatus};

    fn fixture_state() -> InventoryState {
        let mut s = InventoryState::new(Vec::new());
        s.replace_rows(vec![InventoryRow {
            path: "team/jira/api-key".into(),
            status: RowStatus::Provisioned,
            routed_source: Some("1password".into()),
            expires_at: Some("2026-12-01".into()),
            provider: Some("1password".into()),
            scope: "team".into(),
            catalog_override: None,
        }]);
        s.apply_daemon_status(DaemonStatus::Unlocked);
        s
    }

    #[test]
    fn render_does_not_panic_in_headless_egui_context() {
        let mut state = fixture_state();
        egui::__run_test_ui(|ui| {
            render(ui, &mut state);
        });
    }
}

fn status_color(status: RowStatus) -> egui::Color32 {
    match status {
        RowStatus::Provisioned => egui::Color32::from_rgb(0x55, 0xaa, 0x55),
        RowStatus::Expiring => egui::Color32::from_rgb(0xcc, 0xa0, 0x33),
        RowStatus::Missing => egui::Color32::from_rgb(0xcc, 0x44, 0x44),
        RowStatus::FormatInvalid => egui::Color32::from_rgb(0xcc, 0x66, 0x33),
        RowStatus::Unknown => egui::Color32::GRAY,
    }
}
