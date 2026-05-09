//! egui-based GUI backend per [ADR-023] §3.4.
//!
//! Parallel implementation of the four MVP views — the same
//! view-models the TUI consumes ([`crate::inventory`],
//! [`crate::provision_dialog`], [`crate::metadata_editor`],
//! [`crate::discovery_import`]) drive these widgets.
//!
//! ## Why a parallel renderer rather than a shared abstraction
//!
//! The TUI is a flat character buffer; egui is an immediate-mode
//! widget tree. Anything that abstracts over both ends up either
//! over-fitting one or losing the platform-native affordances of
//! the other. ADR-023 §3.4's "shared view-model, separate
//! widget code" boundary is what we already have — these
//! modules just supply the egui half.
//!
//! ## Embedding
//!
//! Each `render(ui, &mut state)` is intended to be called from
//! within an egui [`egui::Ui`] — typically a `CentralPanel`,
//! `Window`, or `SidePanel`. The orchestration crate (P12.2 and
//! later) chooses the surface; this crate is windowing-system
//! agnostic.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

pub mod discovery_import;
pub mod inventory;
pub mod metadata_editor;
pub mod provision_dialog;
