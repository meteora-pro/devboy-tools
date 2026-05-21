//! Vault unlock / create modal — view-model.
//!
//! When `devboy-secrets-ui` starts against a `local-vault`
//! whose passphrase is not already in the environment, the UI
//! must prompt for it before the inventory is usable. This is
//! the pure view-model behind that modal:
//!
//! - [`VaultUnlockState`] holds the entered passphrase (in a
//!   [`SecretString`], zeroized on drop), an optional confirm
//!   field for the create flow, the masked / revealed toggle,
//!   and the lifecycle [`VaultUnlockStatus`].
//! - The egui widget ([`crate::gui::vault_unlock`]) is a thin
//!   render over this; the binary crate owns the actual
//!   `Vault::open` / `Vault::create` calls and feeds the
//!   outcome back via [`VaultUnlockState::apply_status`].
//!
//! Two modes, same view-model:
//!
//! - [`VaultUnlockMode::Unlock`] — an existing `.dvb` file;
//!   one passphrase field, "Unlock" submits.
//! - [`VaultUnlockMode::Create`] — no `.dvb` file yet; a
//!   passphrase + a confirm field that must match, "Create"
//!   submits. The binary then surfaces the recovery phrase.
//!
//! The agent never sees the passphrase: same contract as the
//! provision dialog — the value lives in a `SecretString` and
//! the render only ever emits its length.

use secrecy::{ExposeSecret, SecretString};

/// Whether the modal is unlocking an existing vault or
/// creating a new one. Only the confirm field and the submit
/// label differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VaultUnlockMode {
    /// Existing `.dvb` file — single passphrase field.
    Unlock,
    /// No vault file yet — passphrase + confirm, must match.
    Create,
}

impl VaultUnlockMode {
    /// Modal title.
    pub fn title(self) -> &'static str {
        match self {
            Self::Unlock => "Unlock local vault",
            Self::Create => "Create encrypted local vault",
        }
    }

    /// Primary-button label.
    pub fn submit_label(self) -> &'static str {
        match self {
            Self::Unlock => "Unlock",
            Self::Create => "Create vault",
        }
    }

    /// Whether this mode requires the confirm-passphrase field.
    pub fn needs_confirm(self) -> bool {
        matches!(self, Self::Create)
    }
}

/// Lifecycle status, updated by the binary as the
/// `Vault::open` / `Vault::create` call progresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultUnlockStatus {
    /// Awaiting input — the default.
    Idle,
    /// Submit pressed, the binary is opening / creating the
    /// vault.
    Working,
    /// The vault opened / was created. The caller should close
    /// the modal and swap the backend to the unlocked variant.
    Unlocked,
    /// The attempt failed — wrong passphrase, corrupt file,
    /// mismatched confirm, … Reason is plain text rendered in
    /// red below the input.
    Failed { reason: String },
}

impl VaultUnlockStatus {
    /// Lower-cased label for the footer / snapshot tests.
    pub fn label(&self) -> &str {
        match self {
            Self::Idle => "ready",
            Self::Working => "working…",
            Self::Unlocked => "unlocked",
            Self::Failed { .. } => "failed",
        }
    }
}

/// What the user can do besides typing — produced by the
/// render function each frame.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct VaultUnlockFrameResult {
    /// User pressed the primary button and the local guards
    /// (non-empty passphrase, confirm match) passed — the
    /// binary should attempt the actual unlock / create.
    pub submit: bool,
    /// User chose the "Use keychain instead" escape hatch —
    /// the binary should switch the backend to keychain for
    /// the session.
    pub use_keychain: bool,
}

/// Which field has keyboard focus. The unlock mode has no
/// confirm field, so `Confirm` is unreachable there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultUnlockFocus {
    Passphrase,
    Confirm,
}

/// Pure view-model for the unlock / create modal.
#[derive(Debug)]
pub struct VaultUnlockState {
    mode: VaultUnlockMode,
    passphrase: SecretString,
    passphrase_len: usize,
    /// Confirm field — only meaningful in `Create` mode.
    confirm: SecretString,
    confirm_len: usize,
    focus: VaultUnlockFocus,
    status: VaultUnlockStatus,
    /// Whether the passphrase fields are unmasked. Same
    /// eye-toggle affordance as the provision dialog.
    reveal: bool,
    /// Optional override for the modal heading. When `None` the
    /// render uses `mode.title()`. Set when the modal is being
    /// reused for a non-local-vault backend (e.g. KDBX 4 file
    /// unlock) so the heading matches what's actually being
    /// opened.
    title_override: Option<String>,
    /// Optional override for the explanatory blurb under the
    /// heading. Falls back to the mode-derived default when
    /// `None`.
    blurb_override: Option<String>,
}

impl VaultUnlockState {
    /// Fresh modal. Focus starts on the passphrase field — the
    /// only thing the user does first.
    pub fn new(mode: VaultUnlockMode) -> Self {
        Self {
            mode,
            passphrase: SecretString::new(String::new().into()),
            passphrase_len: 0,
            confirm: SecretString::new(String::new().into()),
            confirm_len: 0,
            focus: VaultUnlockFocus::Passphrase,
            status: VaultUnlockStatus::Idle,
            reveal: false,
            title_override: None,
            blurb_override: None,
        }
    }

    /// Builder — override the modal heading + blurb. Both
    /// strings end up in the rendered modal verbatim; pass
    /// short, plain text. Used to reuse the unlock view-model
    /// for KDBX files where "Unlock local vault" would be
    /// misleading.
    pub fn with_copy(mut self, title: impl Into<String>, blurb: impl Into<String>) -> Self {
        self.title_override = Some(title.into());
        self.blurb_override = Some(blurb.into());
        self
    }

    /// Heading the render should display. Override wins; falls
    /// back to the mode-derived default when no override is set.
    pub fn effective_title(&self) -> &str {
        match self.title_override.as_deref() {
            Some(t) => t,
            None => self.mode.title(),
        }
    }

    /// Blurb the render should display under the heading.
    /// Override wins; the render is responsible for picking a
    /// mode-derived default when this returns `None`.
    pub fn effective_blurb(&self) -> Option<&str> {
        self.blurb_override.as_deref()
    }

    pub fn mode(&self) -> VaultUnlockMode {
        self.mode
    }

    pub fn status(&self) -> &VaultUnlockStatus {
        &self.status
    }

    pub fn focus(&self) -> VaultUnlockFocus {
        self.focus
    }

    pub fn reveal(&self) -> bool {
        self.reveal
    }

    /// Flip the masked / unmasked state of both passphrase
    /// fields.
    pub fn toggle_reveal(&mut self) {
        self.reveal = !self.reveal;
    }

    /// Code-point length of the passphrase — the render uses
    /// this for the masked-char count; tests assert on it.
    pub fn passphrase_len(&self) -> usize {
        self.passphrase_len
    }

    /// Code-point length of the confirm field.
    pub fn confirm_len(&self) -> usize {
        self.confirm_len
    }

    /// Move focus to the named field. A no-op request for the
    /// `Confirm` field in `Unlock` mode is ignored.
    pub fn focus_field(&mut self, field: VaultUnlockFocus) {
        if field == VaultUnlockFocus::Confirm && !self.mode.needs_confirm() {
            return;
        }
        self.focus = field;
    }

    /// Clone the focused field's value into a `String` for a
    /// GUI text-edit binding. The caller feeds it back through
    /// [`Self::replace_focused_str`].
    pub fn focused_clone_for_edit(&self) -> String {
        match self.focus {
            VaultUnlockFocus::Passphrase => self.passphrase.expose_secret().to_string(),
            VaultUnlockFocus::Confirm => self.confirm.expose_secret().to_string(),
        }
    }

    /// Clone the passphrase field specifically (the GUI binds
    /// each field separately, not just the focused one).
    pub fn passphrase_clone_for_edit(&self) -> String {
        self.passphrase.expose_secret().to_string()
    }

    /// Clone the confirm field specifically.
    pub fn confirm_clone_for_edit(&self) -> String {
        self.confirm.expose_secret().to_string()
    }

    /// Replace the passphrase field from a GUI buffer. The
    /// previous `SecretString` is dropped here (zeroized).
    pub fn replace_passphrase_str(&mut self, s: String) {
        self.passphrase_len = s.chars().count();
        self.passphrase = SecretString::new(s.into());
    }

    /// Replace the confirm field from a GUI buffer.
    pub fn replace_confirm_str(&mut self, s: String) {
        self.confirm_len = s.chars().count();
        self.confirm = SecretString::new(s.into());
    }

    /// Replace whichever field currently has focus.
    pub fn replace_focused_str(&mut self, s: String) {
        match self.focus {
            VaultUnlockFocus::Passphrase => self.replace_passphrase_str(s),
            VaultUnlockFocus::Confirm => self.replace_confirm_str(s),
        }
    }

    /// The binary calls this with the outcome of its
    /// `Vault::open` / `Vault::create` attempt.
    pub fn apply_status(&mut self, status: VaultUnlockStatus) {
        self.status = status;
    }

    /// Expose the entered passphrase for the binary's unlock /
    /// create call. Kept deliberately explicit (not a `Deref`)
    /// so every call site is grep-able.
    pub fn passphrase(&self) -> &SecretString {
        &self.passphrase
    }

    /// Local pre-submit guard. Returns `Err(reason)` when the
    /// modal should NOT hand off to the binary yet:
    ///
    /// - empty passphrase, or
    /// - `Create` mode with a confirm field that doesn't match.
    ///
    /// `Ok(())` means the binary can attempt the real
    /// open / create.
    pub fn validate(&self) -> Result<(), String> {
        if self.passphrase.expose_secret().is_empty() {
            return Err("passphrase must not be empty".to_owned());
        }
        if self.mode.needs_confirm()
            && self.passphrase.expose_secret() != self.confirm.expose_secret()
        {
            return Err("passphrase and confirmation do not match".to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_state_starts_idle_masked_focused_on_passphrase() {
        let s = VaultUnlockState::new(VaultUnlockMode::Unlock);
        assert_eq!(s.status(), &VaultUnlockStatus::Idle);
        assert!(!s.reveal());
        assert_eq!(s.focus(), VaultUnlockFocus::Passphrase);
        assert_eq!(s.passphrase_len(), 0);
    }

    #[test]
    fn mode_titles_and_labels_are_distinct() {
        assert_eq!(VaultUnlockMode::Unlock.submit_label(), "Unlock");
        assert_eq!(VaultUnlockMode::Create.submit_label(), "Create vault");
        assert!(!VaultUnlockMode::Unlock.needs_confirm());
        assert!(VaultUnlockMode::Create.needs_confirm());
    }

    #[test]
    fn validate_rejects_an_empty_passphrase() {
        let s = VaultUnlockState::new(VaultUnlockMode::Unlock);
        let err = s.validate().unwrap_err();
        assert!(err.contains("empty"), "got: {err}");
    }

    #[test]
    fn validate_passes_for_a_non_empty_passphrase_in_unlock_mode() {
        let mut s = VaultUnlockState::new(VaultUnlockMode::Unlock);
        s.replace_passphrase_str("hunter2".into());
        assert!(s.validate().is_ok());
    }

    #[test]
    fn validate_rejects_a_create_confirm_mismatch() {
        let mut s = VaultUnlockState::new(VaultUnlockMode::Create);
        s.replace_passphrase_str("hunter2".into());
        s.replace_confirm_str("hunter3".into());
        let err = s.validate().unwrap_err();
        assert!(err.contains("do not match"), "got: {err}");
    }

    #[test]
    fn validate_passes_for_a_matching_create_pair() {
        let mut s = VaultUnlockState::new(VaultUnlockMode::Create);
        s.replace_passphrase_str("matching-pass".into());
        s.replace_confirm_str("matching-pass".into());
        assert!(s.validate().is_ok());
    }

    #[test]
    fn focus_confirm_is_a_no_op_in_unlock_mode() {
        let mut s = VaultUnlockState::new(VaultUnlockMode::Unlock);
        s.focus_field(VaultUnlockFocus::Confirm);
        assert_eq!(
            s.focus(),
            VaultUnlockFocus::Passphrase,
            "Unlock mode has no confirm field — focus request must be ignored"
        );
    }

    #[test]
    fn focus_confirm_works_in_create_mode() {
        let mut s = VaultUnlockState::new(VaultUnlockMode::Create);
        s.focus_field(VaultUnlockFocus::Confirm);
        assert_eq!(s.focus(), VaultUnlockFocus::Confirm);
    }

    #[test]
    fn replace_focused_targets_the_focused_field() {
        let mut s = VaultUnlockState::new(VaultUnlockMode::Create);
        s.replace_focused_str("pass-1".into());
        assert_eq!(s.passphrase_len(), 6);
        assert_eq!(s.confirm_len(), 0);
        s.focus_field(VaultUnlockFocus::Confirm);
        s.replace_focused_str("pass-1".into());
        assert_eq!(s.confirm_len(), 6);
    }

    #[test]
    fn toggle_reveal_flips_both_ways() {
        let mut s = VaultUnlockState::new(VaultUnlockMode::Unlock);
        s.toggle_reveal();
        assert!(s.reveal());
        s.toggle_reveal();
        assert!(!s.reveal());
    }

    #[test]
    fn apply_status_drives_the_lifecycle() {
        let mut s = VaultUnlockState::new(VaultUnlockMode::Unlock);
        s.apply_status(VaultUnlockStatus::Working);
        assert_eq!(s.status().label(), "working…");
        s.apply_status(VaultUnlockStatus::Failed {
            reason: "wrong passphrase".into(),
        });
        assert_eq!(s.status().label(), "failed");
        s.apply_status(VaultUnlockStatus::Unlocked);
        assert_eq!(s.status().label(), "unlocked");
    }

    #[test]
    fn passphrase_accessor_exposes_the_entered_value() {
        let mut s = VaultUnlockState::new(VaultUnlockMode::Unlock);
        s.replace_passphrase_str("the-passphrase".into());
        assert_eq!(s.passphrase().expose_secret(), "the-passphrase");
    }

    #[test]
    fn effective_title_falls_back_to_mode_when_not_overridden() {
        let s = VaultUnlockState::new(VaultUnlockMode::Unlock);
        assert_eq!(s.effective_title(), "Unlock local vault");
        assert!(s.effective_blurb().is_none());
    }

    #[test]
    fn with_copy_overrides_title_and_blurb_for_kdbx_use_case() {
        let s = VaultUnlockState::new(VaultUnlockMode::Unlock).with_copy(
            "Unlock KeePass database",
            "Enter the passphrase that protects `/tmp/x.kdbx`.",
        );
        assert_eq!(s.effective_title(), "Unlock KeePass database");
        assert_eq!(
            s.effective_blurb(),
            Some("Enter the passphrase that protects `/tmp/x.kdbx`.")
        );
    }
}
