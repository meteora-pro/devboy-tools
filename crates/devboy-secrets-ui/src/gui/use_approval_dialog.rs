//! egui Use-approval dialog — see
//! [`crate::use_approval_dialog`] for the shared view-model and
//! [ADR-023] §3.7 for the contract (P25).
//!
//! The dialog is a modeless `egui::Window` so the user can
//! still interact with the inventory while deciding — the
//! request lifecycle is enforced by the daemon-side TTL, not by
//! a hard-modal lock.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use crate::use_approval_dialog::{UseApprovalDecision, UseApprovalState};

/// Render result of one frame. `decision` is `Some` exactly on
/// the frame the user clicked one of the three buttons; the
/// caller observes it once and then closes the window.
#[derive(Debug, Default)]
pub struct UseApprovalFrameResult {
    pub decision: Option<UseApprovalDecision>,
}

/// Render the use-approval dialog as a modeless `egui::Window`.
/// Re-renders are idempotent: once `state` is `Decided`, all
/// further frames are no-ops on the state but the window keeps
/// drawing until the orchestration layer dismisses it.
pub fn render(ctx: &egui::Context, state: &mut UseApprovalState) -> UseApprovalFrameResult {
    let mut out = UseApprovalFrameResult::default();

    egui::Window::new("Approve secret use")
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(format!("Path: {}", state.path()));
            ui.separator();
            ui.label("Reason from agent:");
            // Quote the agent-supplied reason verbatim. It is
            // strictly read-only: rendered as a label, never as
            // an edit field, so it cannot influence anything
            // else on the surface.
            ui.label(format!("\u{201c}{}\u{201d}", state.reason()));
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Allow always (this session)").clicked() {
                    state.record_decision(UseApprovalDecision::AlwaysSession);
                    out.decision = Some(UseApprovalDecision::AlwaysSession);
                }
                if ui.button("Allow once").clicked() {
                    state.record_decision(UseApprovalDecision::Once);
                    out.decision = Some(UseApprovalDecision::Once);
                }
                if ui.button("Deny").clicked() {
                    state.record_decision(UseApprovalDecision::Deny);
                    out.decision = Some(UseApprovalDecision::Deny);
                }
            });
        });

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_approval_dialog::UseApprovalStatus;

    fn fixture() -> UseApprovalState {
        UseApprovalState::new("team/jira/api-key", "pushing image to staging")
    }

    #[test]
    fn render_does_not_panic_in_headless_egui_context() {
        let mut state = fixture();
        egui::__run_test_ui(|ui| {
            let _ = render(ui.ctx(), &mut state);
        });
    }

    #[test]
    fn render_keeps_state_awaiting_when_no_button_clicked() {
        let mut state = fixture();
        egui::__run_test_ui(|ui| {
            let result = render(ui.ctx(), &mut state);
            // No button was clicked in the headless harness.
            assert!(result.decision.is_none());
        });
        assert_eq!(state.status(), UseApprovalStatus::Awaiting);
    }
}
