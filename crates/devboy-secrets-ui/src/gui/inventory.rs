//! egui Inventory view — see [`crate::inventory`] for the
//! shared view-model and [ADR-023] §3.4 for the contract.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use crate::inventory::{InventoryRow, InventoryState, RowStatus, SortKey, TreeNode, ViewMode};

/// Render the inventory into the supplied [`egui::Ui`]. Mutates
/// the state directly via clicks AND keyboard input (row
/// selection, sort key changes, filter changes, search query,
/// group expansion).
///
/// Keyboard shortcuts (when no text-input has focus — typing
/// inside the search bar passes the keys through to the
/// TextEdit instead):
///
/// - `↑` / `↓` — move the selection up / down the visible
///   row list. Works in both Flat and Tree modes.
/// - `Home` / `End` — jump to the first / last visible row.
/// - `/` — focus the search field (same effect as Cmd+F).
/// - `Esc` — clear the search query when focus is anywhere;
///   otherwise the host modal handler runs.
pub fn render(ui: &mut egui::Ui, state: &mut InventoryState) {
    apply_keyboard_navigation(ui, state);

    ui.heading("Secrets — inventory");
    ui.label(state.daemon_status().label());

    ui.separator();

    render_search_bar(ui, state);
    render_filters(ui, state);
    render_view_mode_toggle(ui, state);

    ui.separator();

    match state.view_mode() {
        ViewMode::Flat => render_table(ui, state),
        ViewMode::Tree => render_tree(ui, state),
    }
}

/// Translate non-text key presses into `InventoryState`
/// mutations. Runs at the top of the render so the resulting
/// selection / query change shows up the same frame.
///
/// We deliberately do NOT call `consume_key` — egui's
/// TextEdit widgets need to keep receiving `↑↓` for their own
/// cursor movement, and we don't want to fight them. The
/// `wants_keyboard_input` check ensures we don't steal keys
/// when the user is typing in the search bar.
fn apply_keyboard_navigation(ui: &mut egui::Ui, state: &mut InventoryState) {
    let ctx = ui.ctx().clone();
    let text_input_focused = ctx.egui_wants_keyboard_input();

    // `/` focuses search even when a row label is focused —
    // standard "find" affordance. Skipped while text input
    // already has focus (the slash should land in the field).
    if !text_input_focused
        && ctx.input(|i| {
            i.events
                .iter()
                .any(|e| matches!(e, egui::Event::Text(t) if t == "/"))
        })
    {
        ctx.memory_mut(|m| {
            m.request_focus(egui::Id::new("inventory_search_field"));
        });
    }

    if text_input_focused {
        return;
    }

    // Arrow / Home / End drive the inventory cursor when
    // nothing else is focused.
    ctx.input(|i| {
        if i.key_pressed(egui::Key::ArrowDown) {
            state.move_down();
        }
        if i.key_pressed(egui::Key::ArrowUp) {
            state.move_up();
        }
        if i.key_pressed(egui::Key::Home) {
            state.move_to_top();
        }
        if i.key_pressed(egui::Key::End) {
            state.move_to_bottom();
        }
        if i.key_pressed(egui::Key::Escape) && !state.query().is_empty() {
            state.set_query(String::new());
        }
    });
}

/// Toggle between flat-list and tree-view. The default is
/// picked from row-count in `InventoryState::new`; the user
/// can override at any time.
fn render_view_mode_toggle(ui: &mut egui::Ui, state: &mut InventoryState) {
    ui.horizontal(|ui| {
        ui.label("View:");
        let current = state.view_mode();
        for mode in [ViewMode::Flat, ViewMode::Tree] {
            if ui.selectable_label(current == mode, mode.label()).clicked() {
                state.set_view_mode(mode);
            }
        }
    });
}

/// Top-bar search field. Drives `InventoryState.query`; the
/// real filter lives in `visible_rows`. Counter under the bar
/// shows "N of M shown" so the user can tell what was hidden.
/// `Cmd+F` / `Ctrl+F` focuses the field (mirrors the platform
/// convention for "find").
fn render_search_bar(ui: &mut egui::Ui, state: &mut InventoryState) {
    let total = state.rows().len();
    let visible = state.visible_rows().len();
    let mut buf = state.query().to_owned();
    ui.horizontal(|ui| {
        ui.label("🔍");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut buf)
                .id(egui::Id::new("inventory_search_field"))
                .hint_text("search by path / scope / provider …  (Cmd+F or /)")
                .desired_width(360.0),
        );
        // Cmd+F (macOS) / Ctrl+F (other) focuses the search
        // field. The `/` shortcut is handled at the top-level
        // render via `apply_keyboard_navigation`, which also
        // targets this same egui Id.
        let ctx = ui.ctx().clone();
        let want_focus = ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F));
        if want_focus {
            resp.request_focus();
        }
        if resp.changed() {
            state.set_query(buf.clone());
        }
        if !buf.is_empty() && ui.small_button("✕").on_hover_text("clear search").clicked() {
            state.set_query(String::new());
        }
    });
    // Counter — visible / total. Only show when the user has
    // a query or active filters; otherwise the row count is
    // self-evident from the table.
    let has_filter = !state.query().trim().is_empty() || !state.filters().is_empty();
    if has_filter {
        ui.label(
            egui::RichText::new(format!("{visible} of {total} shown"))
                .small()
                .weak(),
        );
    }
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
            state.set_query(String::new());
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

/// Render the inventory as a collapsible tree built from path
/// segments. The visible-rows + sort + filter + query stack
/// already applies in `state.build_tree()`, so this render
/// just walks the resulting `TreeNode`s.
///
/// Click handling: clicking a leaf row sets it as the
/// selection; clicking a group header toggles
/// `state.expanded`. The `force_open` field on TreeNode wins
/// over the user's expanded-set during active search so the
/// user doesn't have to click through.
fn render_tree(ui: &mut egui::Ui, state: &mut InventoryState) {
    let nodes = state.build_tree();
    // Action queue — we can't borrow `state` mutably while
    // iterating nodes (the node tree was built from a borrow).
    // Collect intents, apply after the walk.
    let mut to_select: Option<String> = None;
    let mut to_toggle: Option<String> = None;
    let selected_path = state.selected_row().map(|r| r.path);
    egui::ScrollArea::vertical()
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            for node in &nodes {
                render_tree_node(
                    ui,
                    node,
                    state,
                    selected_path.as_deref(),
                    &mut to_select,
                    &mut to_toggle,
                );
            }
        });
    if let Some(path) = to_select {
        // Map the path back to its index in visible_rows so
        // `set_selected` lands the right place.
        if let Some(idx) = state.visible_rows().iter().position(|r| r.path == path) {
            state.set_selected(idx);
        }
    }
    if let Some(prefix) = to_toggle {
        state.toggle_expanded(&prefix);
    }
}

/// Recursive group renderer. Emits a `CollapsingHeader` per
/// group, then leaves inside, then descends into children.
fn render_tree_node(
    ui: &mut egui::Ui,
    node: &TreeNode,
    state: &InventoryState,
    selected_path: Option<&str>,
    to_select: &mut Option<String>,
    to_toggle: &mut Option<String>,
) {
    let leaf_count = count_leaves(node);
    let chip = format!("{} ({leaf_count})", node.label);
    // Sticky-expand logic:
    //   - `force_open` (search auto-expand) wins absolutely.
    //   - Otherwise user's expanded-set decides.
    let user_open = state.is_expanded(&node.prefix);
    let initially_open = node.force_open || user_open;
    let header = egui::CollapsingHeader::new(chip)
        .id_salt(&node.prefix)
        .default_open(initially_open);
    let resp = header.show(ui, |ui| {
        for leaf in &node.leaves {
            render_leaf_row(ui, leaf, selected_path, to_select);
        }
        for child in &node.children {
            render_tree_node(ui, child, state, selected_path, to_select, to_toggle);
        }
    });
    // Track manual toggles — only when force_open is not set
    // (otherwise the user can't collapse search-expanded
    // groups, and that's intentional).
    if !node.force_open && resp.header_response.clicked() {
        *to_toggle = Some(node.prefix.clone());
    }
}

/// Render one leaf row (a single `InventoryRow`) inside a
/// tree group. The displayed label is just the LAST path
/// segment — the parent `CollapsingHeader` chain already
/// shows the breadcrumb context so duplicating the full
/// path is redundant + crowded for KeePass DBs with deep
/// group hierarchies. The full path stays in `row.path`
/// for selection tracking and click routing.
fn render_leaf_row(
    ui: &mut egui::Ui,
    leaf: &InventoryRow,
    selected_path: Option<&str>,
    to_select: &mut Option<String>,
) {
    let is_selected = selected_path == Some(leaf.path.as_str());
    let display = leaf.path.rsplit('/').next().unwrap_or(leaf.path.as_str());
    ui.horizontal(|ui| {
        let label = ui
            .selectable_label(is_selected, display)
            .on_hover_text(leaf.path.as_str());
        if label.clicked() {
            *to_select = Some(leaf.path.clone());
        }
        if let Some(badge) = leaf.catalog_override.as_deref() {
            ui.label(catalog_override_chip(badge));
        }
        ui.colored_label(status_color(leaf.status), leaf.status.label());
        if let Some(expires) = leaf.expires_at.as_deref() {
            ui.label(egui::RichText::new(format!("· {expires}")).small().weak());
        }
    });
}

/// Total leaf count under `node` (recursive). Cheap — used
/// to render the per-group "N entries" chip in the header.
fn count_leaves(node: &TreeNode) -> usize {
    node.leaves.len() + node.children.iter().map(count_leaves).sum::<usize>()
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
