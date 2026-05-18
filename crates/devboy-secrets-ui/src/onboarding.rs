//! First-run onboarding wizard — view-model.
//!
//! The very first time `devboy secrets ui` starts with no
//! `sources.toml` and no vault file, it shows a wizard instead
//! of jumping straight to the keychain: pick where secrets
//! live, set a passphrase if a local vault is wanted, point at
//! a HashiCorp Vault if there's a remote one. Combinations are
//! allowed — keychain *and* local-vault *and* HCP Vault can
//! all be configured at once; one of them is marked
//! **primary** (the backend the UI works against until the
//! P11 multi-source router lands).
//!
//! Same shape as [`crate::vault_unlock`] / [`crate::provision_dialog`]:
//! a pure `OnboardingState` here, a thin egui widget in
//! [`crate::gui::onboarding`]. The binary owns the file write
//! + the `Vault::create` call; this module never touches disk.
//!
//! Secret fields (`local_passphrase`, `http_token`) live in
//! [`SecretString`]s — zeroized on drop, never rendered, the
//! same agent-never-sees-the-value contract the rest of the
//! UI follows.

use secrecy::{ExposeSecret, SecretString};

/// Which backend the UI works against once onboarding
/// finishes. Until the P11 multi-source router lands the UI
/// drives exactly one backend; the wizard records the rest in
/// `sources.toml` so they're ready when routing arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingProvider {
    /// OS keychain.
    Keychain,
    /// On-disk encrypted local vault.
    LocalVault,
    /// Remote HashiCorp / HCP Vault over HTTP KV v2.
    HttpVault,
}

impl OnboardingProvider {
    /// Stable lowercase id — used in `sources.toml` and tests.
    pub fn id(self) -> &'static str {
        match self {
            Self::Keychain => "keychain",
            Self::LocalVault => "local-vault",
            Self::HttpVault => "vault",
        }
    }

    /// Human label for the wizard's provider cards.
    pub fn label(self) -> &'static str {
        match self {
            Self::Keychain => "OS keychain",
            Self::LocalVault => "Encrypted local vault",
            Self::HttpVault => "HashiCorp Cloud Vault",
        }
    }
}

/// `read` / `readwrite` — mirrors `devboy_storage::SourceAccess`
/// but kept local so this crate stays decoupled from
/// `devboy-storage`. The binary maps it across.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnboardingAccess {
    /// Read-only.
    Read,
    /// Full access (default).
    #[default]
    ReadWrite,
}

impl OnboardingAccess {
    /// The `access = "..."` value as written to `sources.toml`.
    pub fn toml_value(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::ReadWrite => "readwrite",
        }
    }
}

/// Lifecycle status, driven by the binary as it writes the
/// config / creates the vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnboardingStatus {
    /// Awaiting input.
    Idle,
    /// Submit pressed, the binary is writing config / creating
    /// the vault.
    Working,
    /// Onboarding finished — the caller should close the modal.
    Done,
    /// The attempt failed (or local validation rejected the
    /// input). Reason is plain text rendered in red.
    Failed { reason: String },
}

impl OnboardingStatus {
    /// Lower-cased label for the footer / snapshot tests.
    pub fn label(&self) -> &str {
        match self {
            Self::Idle => "ready",
            Self::Working => "working…",
            Self::Done => "done",
            Self::Failed { .. } => "failed",
        }
    }
}

/// What the user did this frame.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct OnboardingFrameResult {
    /// "Finish setup" pressed and the local guards passed —
    /// the binary should write `sources.toml`, create the
    /// vault if asked, and resolve the backend.
    pub finish: bool,
    /// "Skip — use keychain" pressed — the binary should go
    /// straight to the keychain backend, writing nothing.
    pub skip: bool,
}

/// Pure view-model for the onboarding wizard.
#[derive(Debug)]
pub struct OnboardingState {
    // -- provider toggles (combinations allowed) --------------
    keychain: bool,
    local_vault: bool,
    http_vault: bool,
    // -- local-vault sub-fields -------------------------------
    local_passphrase: SecretString,
    local_passphrase_len: usize,
    local_confirm: SecretString,
    local_confirm_len: usize,
    // -- http-vault sub-fields --------------------------------
    http_addr: String,
    http_namespace: String,
    http_mount: String,
    http_token: SecretString,
    http_token_len: usize,
    http_access: OnboardingAccess,
    // -- shared -----------------------------------------------
    /// Which selected provider the UI works against until P11.
    primary: OnboardingProvider,
    /// Whether the passphrase / token fields are unmasked.
    reveal: bool,
    status: OnboardingStatus,
}

impl Default for OnboardingState {
    fn default() -> Self {
        Self::new()
    }
}

impl OnboardingState {
    /// Fresh wizard. Keychain is pre-ticked (the zero-config
    /// default) and is the initial primary; the user opts into
    /// the vault backends.
    pub fn new() -> Self {
        Self {
            keychain: true,
            local_vault: false,
            http_vault: false,
            local_passphrase: SecretString::new(String::new().into()),
            local_passphrase_len: 0,
            local_confirm: SecretString::new(String::new().into()),
            local_confirm_len: 0,
            http_addr: String::new(),
            http_namespace: String::new(),
            http_mount: "secret/data".to_owned(),
            http_token: SecretString::new(String::new().into()),
            http_token_len: 0,
            http_access: OnboardingAccess::ReadWrite,
            primary: OnboardingProvider::Keychain,
            reveal: false,
            status: OnboardingStatus::Idle,
        }
    }

    // -- provider toggles -------------------------------------

    pub fn keychain_selected(&self) -> bool {
        self.keychain
    }
    pub fn local_vault_selected(&self) -> bool {
        self.local_vault
    }
    pub fn http_vault_selected(&self) -> bool {
        self.http_vault
    }

    /// Toggle a provider on / off. Turning off the current
    /// `primary` re-points `primary` at the first provider
    /// still selected (or keychain if somehow none are).
    pub fn toggle_provider(&mut self, provider: OnboardingProvider, on: bool) {
        match provider {
            OnboardingProvider::Keychain => self.keychain = on,
            OnboardingProvider::LocalVault => self.local_vault = on,
            OnboardingProvider::HttpVault => self.http_vault = on,
        }
        if !on && self.primary == provider {
            self.primary = self
                .selected_providers()
                .first()
                .copied()
                .unwrap_or(OnboardingProvider::Keychain);
        }
        // Picking the first provider when none was primary yet.
        if on && self.selected_providers().len() == 1 {
            self.primary = provider;
        }
    }

    /// Every provider currently ticked, in display order.
    pub fn selected_providers(&self) -> Vec<OnboardingProvider> {
        let mut v = Vec::with_capacity(3);
        if self.keychain {
            v.push(OnboardingProvider::Keychain);
        }
        if self.local_vault {
            v.push(OnboardingProvider::LocalVault);
        }
        if self.http_vault {
            v.push(OnboardingProvider::HttpVault);
        }
        v
    }

    pub fn primary(&self) -> OnboardingProvider {
        self.primary
    }

    /// Set the primary backend. Ignored if `provider` is not
    /// currently selected — the UI only offers ticked ones.
    pub fn set_primary(&mut self, provider: OnboardingProvider) {
        if self.selected_providers().contains(&provider) {
            self.primary = provider;
        }
    }

    // -- reveal toggle ----------------------------------------

    pub fn reveal(&self) -> bool {
        self.reveal
    }
    pub fn toggle_reveal(&mut self) {
        self.reveal = !self.reveal;
    }

    // -- local-vault fields -----------------------------------

    pub fn local_passphrase_len(&self) -> usize {
        self.local_passphrase_len
    }
    pub fn local_confirm_len(&self) -> usize {
        self.local_confirm_len
    }
    pub fn local_passphrase_clone_for_edit(&self) -> String {
        self.local_passphrase.expose_secret().to_string()
    }
    pub fn local_confirm_clone_for_edit(&self) -> String {
        self.local_confirm.expose_secret().to_string()
    }
    pub fn replace_local_passphrase_str(&mut self, s: String) {
        self.local_passphrase_len = s.chars().count();
        self.local_passphrase = SecretString::new(s.into());
    }
    pub fn replace_local_confirm_str(&mut self, s: String) {
        self.local_confirm_len = s.chars().count();
        self.local_confirm = SecretString::new(s.into());
    }
    /// The entered local-vault passphrase — for the binary's
    /// `Vault::create` call.
    pub fn local_passphrase(&self) -> &SecretString {
        &self.local_passphrase
    }

    // -- http-vault fields ------------------------------------

    pub fn http_addr(&self) -> &str {
        &self.http_addr
    }
    pub fn set_http_addr(&mut self, s: String) {
        self.http_addr = s;
    }
    pub fn http_namespace(&self) -> &str {
        &self.http_namespace
    }
    pub fn set_http_namespace(&mut self, s: String) {
        self.http_namespace = s;
    }
    pub fn http_mount(&self) -> &str {
        &self.http_mount
    }
    pub fn set_http_mount(&mut self, s: String) {
        self.http_mount = s;
    }
    pub fn http_token_len(&self) -> usize {
        self.http_token_len
    }
    pub fn http_token_clone_for_edit(&self) -> String {
        self.http_token.expose_secret().to_string()
    }
    pub fn replace_http_token_str(&mut self, s: String) {
        self.http_token_len = s.chars().count();
        self.http_token = SecretString::new(s.into());
    }
    /// The entered Vault token — for the binary's
    /// `StorageBackend::HttpVault`.
    pub fn http_token(&self) -> &SecretString {
        &self.http_token
    }
    pub fn http_access(&self) -> OnboardingAccess {
        self.http_access
    }
    pub fn set_http_access(&mut self, access: OnboardingAccess) {
        self.http_access = access;
    }

    // -- status -----------------------------------------------

    pub fn status(&self) -> &OnboardingStatus {
        &self.status
    }
    pub fn apply_status(&mut self, status: OnboardingStatus) {
        self.status = status;
    }

    // -- validation -------------------------------------------

    /// Local pre-submit guard. `Err(reason)` when the wizard
    /// should NOT hand off to the binary yet:
    ///
    /// - no provider selected at all;
    /// - local-vault ticked but the passphrase is empty or the
    ///   confirmation doesn't match;
    /// - http-vault ticked but the address or token is empty.
    pub fn validate(&self) -> Result<(), String> {
        if self.selected_providers().is_empty() {
            return Err("pick at least one backend".to_owned());
        }
        if self.local_vault {
            if self.local_passphrase.expose_secret().is_empty() {
                return Err("local-vault passphrase must not be empty".to_owned());
            }
            if self.local_passphrase.expose_secret() != self.local_confirm.expose_secret() {
                return Err("local-vault passphrase and confirmation do not match".to_owned());
            }
        }
        if self.http_vault {
            if self.http_addr.trim().is_empty() {
                return Err("HashiCorp Vault address must not be empty".to_owned());
            }
            if self.http_token.expose_secret().is_empty() {
                return Err("HashiCorp Vault token must not be empty".to_owned());
            }
        }
        Ok(())
    }

    /// Render the selected providers into a `sources.toml`
    /// body. Secrets are NOT written here — the local-vault
    /// passphrase never lands in the config (it unlocks the
    /// `.dvb` file); the Vault token IS written because that's
    /// where a `type = "vault"` source reads it from.
    ///
    /// `[default].source` is set to the `primary` provider so
    /// the future router knows the working backend.
    pub fn to_sources_toml(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        out.push_str("# Generated by the devboy secrets onboarding wizard.\n");
        out.push_str("# Edit by hand to add routes, change access, or add sources.\n\n");

        if self.keychain {
            out.push_str("[[source]]\n");
            out.push_str("name = \"default-keychain\"\n");
            out.push_str("type = \"keychain\"\n\n");
        }
        if self.local_vault {
            out.push_str("[[source]]\n");
            out.push_str("name = \"local-vault\"\n");
            out.push_str("type = \"local-vault\"\n\n");
        }
        if self.http_vault {
            out.push_str("[[source]]\n");
            out.push_str("name = \"hcp-vault\"\n");
            out.push_str("type = \"vault\"\n");
            let _ = writeln!(out, "access = \"{}\"", self.http_access.toml_value());
            let _ = writeln!(out, "addr = \"{}\"", self.http_addr.trim());
            if !self.http_namespace.trim().is_empty() {
                let _ = writeln!(out, "namespace = \"{}\"", self.http_namespace.trim());
            }
            let _ = writeln!(out, "mount = \"{}\"", self.http_mount.trim());
            let _ = writeln!(out, "token = \"{}\"", self.http_token.expose_secret());
            out.push('\n');
        }

        let primary_name = match self.primary {
            OnboardingProvider::Keychain => "default-keychain",
            OnboardingProvider::LocalVault => "local-vault",
            OnboardingProvider::HttpVault => "hcp-vault",
        };
        let _ = writeln!(out, "[default]\nsource = \"{primary_name}\"");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_state_has_keychain_preselected_as_primary() {
        let s = OnboardingState::new();
        assert!(s.keychain_selected());
        assert!(!s.local_vault_selected());
        assert!(!s.http_vault_selected());
        assert_eq!(s.primary(), OnboardingProvider::Keychain);
        assert_eq!(s.status(), &OnboardingStatus::Idle);
    }

    #[test]
    fn toggling_a_provider_off_repoints_primary() {
        let mut s = OnboardingState::new();
        s.toggle_provider(OnboardingProvider::LocalVault, true);
        s.set_primary(OnboardingProvider::LocalVault);
        assert_eq!(s.primary(), OnboardingProvider::LocalVault);
        // Turning local-vault off must move primary back to a
        // still-selected provider (keychain).
        s.toggle_provider(OnboardingProvider::LocalVault, false);
        assert_eq!(s.primary(), OnboardingProvider::Keychain);
    }

    #[test]
    fn set_primary_ignores_an_unselected_provider() {
        let mut s = OnboardingState::new();
        // http-vault is not ticked → can't be primary.
        s.set_primary(OnboardingProvider::HttpVault);
        assert_eq!(s.primary(), OnboardingProvider::Keychain);
    }

    #[test]
    fn selected_providers_lists_every_ticked_backend_in_order() {
        let mut s = OnboardingState::new();
        s.toggle_provider(OnboardingProvider::LocalVault, true);
        s.toggle_provider(OnboardingProvider::HttpVault, true);
        assert_eq!(
            s.selected_providers(),
            vec![
                OnboardingProvider::Keychain,
                OnboardingProvider::LocalVault,
                OnboardingProvider::HttpVault,
            ]
        );
    }

    #[test]
    fn validate_rejects_zero_providers() {
        let mut s = OnboardingState::new();
        s.toggle_provider(OnboardingProvider::Keychain, false);
        assert!(s.validate().unwrap_err().contains("at least one"));
    }

    #[test]
    fn validate_rejects_local_vault_passphrase_mismatch() {
        let mut s = OnboardingState::new();
        s.toggle_provider(OnboardingProvider::LocalVault, true);
        s.replace_local_passphrase_str("hunter2".into());
        s.replace_local_confirm_str("hunter3".into());
        assert!(s.validate().unwrap_err().contains("do not match"));
    }

    #[test]
    fn validate_rejects_local_vault_empty_passphrase() {
        let mut s = OnboardingState::new();
        s.toggle_provider(OnboardingProvider::LocalVault, true);
        assert!(s.validate().unwrap_err().contains("must not be empty"));
    }

    #[test]
    fn validate_rejects_http_vault_without_addr_or_token() {
        let mut s = OnboardingState::new();
        s.toggle_provider(OnboardingProvider::HttpVault, true);
        assert!(s.validate().unwrap_err().contains("address"));
        s.set_http_addr("https://vault.example.invalid".into());
        assert!(s.validate().unwrap_err().contains("token"));
        s.replace_http_token_str("hvs.sometoken".into());
        assert!(s.validate().is_ok());
    }

    #[test]
    fn validate_passes_for_keychain_only() {
        let s = OnboardingState::new();
        assert!(s.validate().is_ok());
    }

    #[test]
    fn validate_passes_for_a_matching_local_vault_pair() {
        let mut s = OnboardingState::new();
        s.toggle_provider(OnboardingProvider::LocalVault, true);
        s.replace_local_passphrase_str("matching-pass".into());
        s.replace_local_confirm_str("matching-pass".into());
        assert!(s.validate().is_ok());
    }

    #[test]
    fn to_sources_toml_keychain_only() {
        let s = OnboardingState::new();
        let toml = s.to_sources_toml();
        assert!(toml.contains("name = \"default-keychain\""));
        assert!(toml.contains("type = \"keychain\""));
        assert!(toml.contains("[default]\nsource = \"default-keychain\""));
        // No vault sections.
        assert!(!toml.contains("type = \"local-vault\""));
        assert!(!toml.contains("type = \"vault\""));
    }

    #[test]
    fn to_sources_toml_all_three_with_http_vault_primary() {
        let mut s = OnboardingState::new();
        s.toggle_provider(OnboardingProvider::LocalVault, true);
        s.toggle_provider(OnboardingProvider::HttpVault, true);
        s.set_http_addr("https://vault.hashicorp.cloud:8200".into());
        s.set_http_namespace("admin".into());
        s.replace_http_token_str("hvs.tok".into());
        s.set_http_access(OnboardingAccess::Read);
        s.set_primary(OnboardingProvider::HttpVault);

        let toml = s.to_sources_toml();
        // All three sources present.
        assert!(toml.contains("type = \"keychain\""));
        assert!(toml.contains("type = \"local-vault\""));
        assert!(toml.contains("type = \"vault\""));
        // HCP Vault fields.
        assert!(toml.contains("addr = \"https://vault.hashicorp.cloud:8200\""));
        assert!(toml.contains("namespace = \"admin\""));
        assert!(toml.contains("mount = \"secret/data\""));
        assert!(toml.contains("access = \"read\""));
        assert!(toml.contains("token = \"hvs.tok\""));
        // Primary routed to the HCP Vault.
        assert!(toml.contains("[default]\nsource = \"hcp-vault\""));
    }

    #[test]
    fn to_sources_toml_omits_namespace_when_blank() {
        let mut s = OnboardingState::new();
        s.toggle_provider(OnboardingProvider::HttpVault, true);
        s.set_http_addr("https://vault.example.invalid".into());
        s.replace_http_token_str("hvs.tok".into());
        // namespace left blank
        let toml = s.to_sources_toml();
        assert!(toml.contains("type = \"vault\""));
        assert!(!toml.contains("namespace ="));
    }

    #[test]
    fn toggle_reveal_flips_both_ways() {
        let mut s = OnboardingState::new();
        s.toggle_reveal();
        assert!(s.reveal());
        s.toggle_reveal();
        assert!(!s.reveal());
    }

    #[test]
    fn status_label_covers_every_variant() {
        assert_eq!(OnboardingStatus::Idle.label(), "ready");
        assert_eq!(OnboardingStatus::Working.label(), "working…");
        assert_eq!(OnboardingStatus::Done.label(), "done");
        assert_eq!(
            OnboardingStatus::Failed { reason: "x".into() }.label(),
            "failed"
        );
    }
}
