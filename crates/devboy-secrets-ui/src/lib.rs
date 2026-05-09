//! Native UI for `devboy-tools` secrets — TUI on `ratatui`, GUI on `egui`.
//!
//! Two backends share a view-model layer; only the widget code differs.
//! See [ADR-023] §3.4 for the four MVP views (Inventory,
//! Provision/Rotation, Edit Metadata, Discovery Import).
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

pub mod inventory;

#[cfg(feature = "tui")]
pub use inventory::render as render_inventory;
pub use inventory::{
    DaemonStatus, Focus, InventoryFilters, InventoryRow, InventoryState, RowStatus, SortKey,
};
