//! Native UI for `devboy-tools` secrets — TUI on `ratatui`, GUI on `egui`.
//!
//! Two backends share a view-model layer; only the widget code differs.
//! See [ADR-023] §3.4 for the four MVP views (Inventory,
//! Provision/Rotation, Edit Metadata, Discovery Import).
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

pub mod discovery_import;
#[cfg(feature = "gui")]
pub mod gui;
pub mod inventory;
pub mod metadata_editor;
pub mod provision_dialog;

#[cfg(feature = "tui")]
pub use inventory::render as render_inventory;
pub use inventory::{
    DaemonStatus, Focus, InventoryFilters, InventoryRow, InventoryState, RowStatus, SortKey,
};

#[cfg(feature = "tui")]
pub use provision_dialog::render as render_provision_dialog;
pub use provision_dialog::{
    DialogFocus, DialogMetadata, DialogMode, DialogState, DialogStatus, DialogSubmission,
};

#[cfg(feature = "tui")]
pub use metadata_editor::render as render_metadata_editor;
pub use metadata_editor::{
    EditorFocus, EditorMode, EditorState, EditorStatus, EditorSubmission, FieldDiff, MetadataDraft,
    MetadataField,
};

#[cfg(feature = "tui")]
pub use discovery_import::render as render_discovery_import;
pub use discovery_import::{
    DiscoveryItem, ImportFocus, ImportRow, ImportRowStatus, ImportState, ImportSubmission,
    suggest_path,
};
