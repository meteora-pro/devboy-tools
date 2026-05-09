//! Native UI for `devboy-tools` secrets — TUI on `ratatui`, GUI on `egui`.
//!
//! Two backends share a view-model layer; only the widget code differs.
//! See [ADR-023] §3.4 for the four MVP views (Inventory,
//! Provision/Rotation, Edit Metadata, Discovery Import).
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md
//!
//! Status: scaffolding — implementation lands in epic #247 phases
//! P11 (TUI) and P12 (GUI).
