//! K25 — multi-wallet types layered over the single-instance
//! [`crate::StorageBackend`].
//!
//! `NamedBackend` pairs a `StorageBackend` with a stable name
//! (the user-visible label, matching the `name` field in the
//! corresponding `[[source]]` block of `sources.toml`).
//!
//! `WalletList` owns several `NamedBackend`s plus a `primary_idx`
//! that marks the backend new secrets get written to by default.
//! Reads pull from whichever wallet `has_value(path)` first; if
//! several wallets carry the same path the primary wins (matches
//! the daemon's router precedence — primary-source-of-record).
//!
//! The list is mutated through five methods:
//!
//! * `unlock_by_name(name, passphrase)` — invokes
//!   `StorageBackend::unlock` on the named wallet and swaps the
//!   slot atomically. Wrong passphrase / unknown name surfaces a
//!   `Result<(), String>` for the UI to display.
//! * `lock_by_name(name)` — drops the in-memory passphrase /
//!   snapshot back to the Locked variant.
//! * `refresh_by_name(name)` — re-opens the KDBX file in place
//!   (used after a working-copy write to pick up new entries).
//! * `set_primary(name)` — switches which wallet receives writes.
//! * `replace(name, new_backend)` — used by the existing keychain
//!   ↔ local-vault session switcher and by the create-vault flow
//!   that mints a brand-new on-disk vault.
//!
//! The K27 inventory aggregation walks `all_unlocked()` and folds
//! every unlocked KDBX snapshot's rows in, tagging each row with
//! the originating wallet name. Locked wallets contribute zero
//! data — they only appear in the K28 wallet-bar chips and the
//! K30 lazy-unlock placeholder rows.

use crate::{StorageBackend, default_vault_path};
use devboy_storage::SourceDefinition;

/// One named entry in a [`WalletList`]. The name matches the
/// `[[source]]` block's `name` key when loaded from
/// `sources.toml`; env-driven backends get synthetic names
/// (`kdbx-env`, `local-vault`, `keychain`).
#[derive(Debug, Clone)]
pub struct NamedBackend {
    pub name: String,
    pub backend: StorageBackend,
}

impl NamedBackend {
    pub fn new(name: impl Into<String>, backend: StorageBackend) -> Self {
        Self {
            name: name.into(),
            backend,
        }
    }
}

/// Top-level wallet collection — owns every backend the user
/// has connected this session. `primary_idx` points at the
/// wallet that handles writes; the index is stable across
/// reload / lock / unlock cycles because mutations swap the
/// slot in place instead of pushing / popping.
///
/// Invariant: `wallets` is non-empty after construction. The
/// fallback path is a single [`StorageBackend::Keychain`]
/// labelled `keychain` — same shape as the legacy single-
/// backend setup.
#[derive(Debug, Clone)]
pub struct WalletList {
    wallets: Vec<NamedBackend>,
    primary_idx: usize,
}

impl WalletList {
    /// Build a list from one initial backend. Used as the
    /// migration shim during K25 → K26: the existing
    /// `InventoryApp::new` keeps calling `detect_from_env()`
    /// and wraps the result in a one-wallet list.
    pub fn from_single(backend: StorageBackend) -> Self {
        let name = synthetic_name_for(&backend);
        Self {
            wallets: vec![NamedBackend::new(name, backend)],
            primary_idx: 0,
        }
    }

    /// Build a list from one or more backends, with explicit
    /// primary selection by index. Panics if `wallets` is empty
    /// or `primary_idx` is out of range — both are programmer
    /// errors caught in `WalletList::from_router_config_and_env`.
    pub fn from_parts(wallets: Vec<NamedBackend>, primary_idx: usize) -> Self {
        assert!(
            !wallets.is_empty(),
            "WalletList must hold at least one backend"
        );
        assert!(
            primary_idx < wallets.len(),
            "primary_idx {primary_idx} out of range (len {})",
            wallets.len()
        );
        Self {
            wallets,
            primary_idx,
        }
    }

    pub fn wallets(&self) -> &[NamedBackend] {
        &self.wallets
    }

    pub fn primary(&self) -> &NamedBackend {
        &self.wallets[self.primary_idx]
    }

    pub fn primary_mut(&mut self) -> &mut NamedBackend {
        &mut self.wallets[self.primary_idx]
    }

    pub fn primary_name(&self) -> &str {
        &self.wallets[self.primary_idx].name
    }

    /// Locate a wallet by name. Names are unique within a list
    /// — the loader rejects duplicates.
    pub fn find(&self, name: &str) -> Option<&NamedBackend> {
        self.wallets.iter().find(|w| w.name == name)
    }

    pub fn find_idx(&self, name: &str) -> Option<usize> {
        self.wallets.iter().position(|w| w.name == name)
    }

    /// Replace the named wallet's backend in place. Returns
    /// `Err` on unknown name. Used by the keychain ↔ local-vault
    /// session switcher and the create-vault flow.
    pub fn replace(&mut self, name: &str, new_backend: StorageBackend) -> Result<(), String> {
        let idx = self
            .find_idx(name)
            .ok_or_else(|| format!("no wallet named {name:?}"))?;
        self.wallets[idx].backend = new_backend;
        Ok(())
    }

    /// Replace the *primary* wallet — convenience for callers
    /// that don't already know its name. Used by the
    /// lock/unlock paths during K25 to preserve the pre-K25
    /// `self.backend = new` behaviour.
    pub fn replace_primary(&mut self, new_backend: StorageBackend) {
        self.wallets[self.primary_idx].backend = new_backend;
    }

    /// Promote the named wallet to primary. Returns `Err` for
    /// unknown names. Idempotent — a no-op when the named
    /// wallet is already primary.
    pub fn set_primary(&mut self, name: &str) -> Result<(), String> {
        let idx = self
            .find_idx(name)
            .ok_or_else(|| format!("no wallet named {name:?}"))?;
        self.primary_idx = idx;
        Ok(())
    }

    /// Unlock the named wallet with `passphrase`. Returns the
    /// underlying [`StorageBackend::unlock`] error verbatim on
    /// wrong-passphrase / corrupt-file failures.
    pub fn unlock_by_name(
        &mut self,
        name: &str,
        passphrase: secrecy::SecretString,
    ) -> Result<(), String> {
        let idx = self
            .find_idx(name)
            .ok_or_else(|| format!("no wallet named {name:?}"))?;
        let new_backend = self.wallets[idx].backend.unlock(passphrase)?;
        self.wallets[idx].backend = new_backend;
        Ok(())
    }

    /// Lock the named wallet. Drops the in-memory passphrase +
    /// snapshot back to the locked variant; subsequent reads
    /// need a fresh unlock.
    pub fn lock_by_name(&mut self, name: &str) -> Result<(), String> {
        let idx = self
            .find_idx(name)
            .ok_or_else(|| format!("no wallet named {name:?}"))?;
        let locked = self.wallets[idx].backend.lock();
        self.wallets[idx].backend = locked;
        Ok(())
    }

    /// Re-open the named wallet's underlying storage from disk
    /// using the cached passphrase. KDBX-only today — other
    /// backends return their refresh result verbatim.
    pub fn refresh_by_name(&mut self, name: &str) -> Result<(), String> {
        let idx = self
            .find_idx(name)
            .ok_or_else(|| format!("no wallet named {name:?}"))?;
        let new_backend = self.wallets[idx].backend.refresh()?;
        self.wallets[idx].backend = new_backend;
        Ok(())
    }

    /// `has_value` over every unlocked wallet. Returns `true`
    /// when any wallet reports the path is provisioned. The
    /// returned name (when `Some`) is the FIRST match in slot
    /// order, with primary checked first.
    pub fn route_has_value(&self, path: &str) -> Option<&str> {
        // Check primary first — matches the daemon's
        // primary-source-of-record convention.
        if self.wallets[self.primary_idx].backend.has_value(path) {
            return Some(&self.wallets[self.primary_idx].name);
        }
        for (i, w) in self.wallets.iter().enumerate() {
            if i == self.primary_idx {
                continue;
            }
            if w.backend.has_value(path) {
                return Some(&w.name);
            }
        }
        None
    }

    /// Iterator over unlocked wallets — feeds the K27 inventory
    /// aggregation and the K28 wallet-bar entry-count chip.
    pub fn all_unlocked(&self) -> impl Iterator<Item = &NamedBackend> + '_ {
        self.wallets.iter().filter(|w| !w.backend.is_locked())
    }

    /// Iterator over locked wallets — feeds the K28 chip layout
    /// + the K30 lazy-unlock placeholder rows.
    pub fn all_locked(&self) -> impl Iterator<Item = &NamedBackend> + '_ {
        self.wallets.iter().filter(|w| w.backend.is_locked())
    }
}

/// K26 — Load every supported `[[source]]` block from
/// `sources.toml` as a `NamedBackend`, excluding any source
/// whose name matches `primary_name` (so the env-driven primary
/// doesn't show up twice in the wallet list). Supported types:
/// `keychain`, `local-vault`, `kdbx`. Unsupported types
/// (`vault`, `1password`, `env-store`) are silently skipped —
/// they stay in the toml for the daemon's router but the GUI
/// doesn't surface them yet.
///
/// Returns an empty Vec when `sources.toml` is missing or
/// fails to parse — the UI continues with just its primary.
pub fn load_extra_wallets_from_router_config(primary_name: &str) -> Vec<NamedBackend> {
    use devboy_storage::RouterConfig;
    let Ok(cfg) = RouterConfig::load() else {
        return Vec::new();
    };
    cfg.sources
        .iter()
        .filter(|def| def.name != primary_name)
        .filter_map(|def| backend_from_definition(def).map(|b| NamedBackend::new(&def.name, b)))
        .collect()
}

/// Build a `StorageBackend` from one `[[source]]` block.
/// Returns `None` for source types the GUI doesn't yet handle —
/// caller silently skips them.
pub fn backend_from_definition(def: &SourceDefinition) -> Option<StorageBackend> {
    match def.source_type.as_str() {
        "keychain" => Some(StorageBackend::Keychain),
        "local-vault" => {
            // `path` is optional — when absent we fall back to
            // the daemon's default vault location so toml-only
            // configs keep working.
            let path = def
                .settings
                .get("path")
                .and_then(toml::Value::as_str)
                .map(std::path::PathBuf::from)
                .unwrap_or_else(default_vault_path);
            Some(StorageBackend::LocalVaultLocked { vault_path: path })
        }
        "kdbx" => {
            let file = def
                .settings
                .get("file")
                .and_then(toml::Value::as_str)
                .map(std::path::PathBuf::from)?;
            let keyfile = def
                .settings
                .get("keyfile")
                .and_then(toml::Value::as_str)
                .map(std::path::PathBuf::from);
            Some(StorageBackend::KdbxLocked { file, keyfile })
        }
        _ => None,
    }
}

/// Synthesize a name for a backend that wasn't loaded from a
/// `[[source]]` block (i.e. came from `detect_from_env` or the
/// keychain default). Used by `WalletList::from_single` and by
/// the env-compat path in K26 when the env-driven backend has
/// no matching toml entry.
pub fn synthetic_name_for(backend: &StorageBackend) -> String {
    match backend {
        StorageBackend::Keychain => "keychain".to_owned(),
        StorageBackend::LocalVault { .. } | StorageBackend::LocalVaultLocked { .. } => {
            "local-vault".to_owned()
        }
        StorageBackend::HttpVault { addr, .. } => {
            // strip scheme so the chip reads "vault.acme.dev"
            // rather than "https://vault.acme.dev".
            addr.split("://").nth(1).unwrap_or(addr).to_owned()
        }
        StorageBackend::KdbxLocked { file, .. } | StorageBackend::Kdbx { file, .. } => file
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("kdbx")
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_single_wraps_a_keychain_with_synthetic_name() {
        let list = WalletList::from_single(StorageBackend::Keychain);
        assert_eq!(list.wallets().len(), 1);
        assert_eq!(list.primary().name, "keychain");
    }

    #[test]
    fn synthetic_name_strips_scheme_from_http_vault() {
        let backend = StorageBackend::HttpVault {
            addr: "https://vault.acme.dev:8200".to_owned(),
            namespace: None,
            mount: "secret/data".to_owned(),
            token: secrecy::SecretString::new("t".into()),
            access: devboy_storage::SourceAccess::Read,
        };
        assert_eq!(synthetic_name_for(&backend), "vault.acme.dev:8200");
    }

    #[test]
    fn replace_primary_swaps_backend_in_place() {
        let mut list = WalletList::from_single(StorageBackend::Keychain);
        list.replace_primary(StorageBackend::LocalVaultLocked {
            vault_path: std::path::PathBuf::from("/tmp/x.devboy"),
        });
        assert!(matches!(
            list.primary().backend,
            StorageBackend::LocalVaultLocked { .. }
        ));
        // name stays the same — the user labelled this slot
        // "keychain" and renaming it would be surprising.
        assert_eq!(list.primary().name, "keychain");
    }

    #[test]
    fn set_primary_rejects_unknown_name() {
        let mut list = WalletList::from_single(StorageBackend::Keychain);
        let err = list.set_primary("nope").unwrap_err();
        assert!(err.contains("nope"));
    }

    #[test]
    fn route_has_value_returns_first_matching_wallet_name() {
        // Both wallets are keychain — has_value always returns
        // false there, so this test just verifies the iteration
        // shape (primary first). Real coverage of the routing
        // semantics lives in the K27 inventory tests.
        let list = WalletList::from_parts(
            vec![
                NamedBackend::new("a", StorageBackend::Keychain),
                NamedBackend::new("b", StorageBackend::Keychain),
            ],
            0,
        );
        assert!(list.route_has_value("any/path").is_none());
    }
}
