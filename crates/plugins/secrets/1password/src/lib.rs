//! `devboy-secret-1password` — built-in `SecretSource` backed by
//! the 1Password `op` CLI per [ADR-021] §8.
//!
//! `reference` is an `op://` URL — e.g.
//! `op://Personal/GitHub PAT/credential` (`vault/item/field`,
//! optionally `vault/item/section/field`). The router (P5+) does
//! not generate these; users either supply them via
//! `[secret."<path>"]` overrides or by configuring a
//! `[[route]] prefix = ".../" source = "1p-personal" vault =
//! "Personal"` and letting the source build the URL from the path
//! tail.
//!
//! # Capabilities
//!
//! Per ADR-021 §8: `READ | LIST | VALIDATE | BIOMETRIC_PROMPT |
//! AUDIT_LOGGED`.
//!
//! - `BIOMETRIC_PROMPT` because in the typical configuration
//!   (Touch-ID-unlocked desktop app) `op` round-trips through the
//!   1Password app, which prompts for a biometric every read.
//!   `doctor` and the agent provisioning flow use this hint to warn
//!   about loops that would trigger N prompts.
//! - `AUDIT_LOGGED` because every read shows up in 1Password's
//!   account activity. Surfaced in `doctor` so the user knows
//!   reads are observable.
//!
//! Write capability is intentionally omitted from v1: `op item
//! create` is muddled enough that the ADR-020 bootstrap flow
//! opens the 1Password UI for new entries instead of writing
//! through the CLI.
//!
//! # Availability
//!
//! `is_available()` runs `op whoami` (with `--account <name>` if
//! the source is configured for a specific account).
//!
//! - exit 0 → [`SourceStatus::Available`]
//! - "not currently signed in" / "session has ended" →
//!   [`SourceStatus::Locked`] (`doctor` shows `op signin`)
//! - `op` binary missing → [`SourceStatus::NotInstalled`]
//! - anything else → [`SourceStatus::Error`]
//!
//! # Sub-process model
//!
//! Every operation forks `op` and waits for it. There's no
//! daemon, no persistent connection: `op` itself caches the
//! Touch-ID session inside the 1Password app, which is good
//! enough for the typical desktop workflow. The cache layer
//! (P5.4) absorbs latency for warm reads.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use devboy_storage::{
    Capabilities, GetOutcome, RemoteRef, SecretSource, SourceError, SourceStatus,
};
use secrecy::SecretString;
use serde::Deserialize;
use thiserror::Error;
use tokio::process::Command;

/// Default name of the 1Password CLI binary.
pub const DEFAULT_OP_BINARY: &str = "op";

/// Env var that overrides the discovered `op` binary path. Useful
/// for tests, CI, and side-by-side installations.
pub const OP_BIN_ENV: &str = "DEVBOY_OP_BIN";

/// Reference URL scheme for 1Password references.
pub const OP_URL_PREFIX: &str = "op://";

// =============================================================================
// Errors
// =============================================================================

/// Failure modes for [`OnePasswordSource::new`].
#[derive(Debug, Error)]
pub enum OpError {
    /// `op` binary could not be located in the PATH.
    #[error(
        "1Password `op` CLI not found in PATH; install it from https://developer.1password.com/docs/cli/get-started/ or set {OP_BIN_ENV}"
    )]
    BinaryNotFound,
}

// =============================================================================
// Source
// =============================================================================

/// `SecretSource` backed by the 1Password `op` CLI.
#[derive(Debug, Clone)]
pub struct OnePasswordSource {
    /// Logical name from `[[source]] name = "..."` in `sources.toml`.
    name: String,
    /// Optional 1Password account override; passed to `op` as
    /// `--account <name>` when set.
    account: Option<String>,
    /// Path to the `op` binary. Resolved at construction time so
    /// every operation does not re-walk PATH.
    op_binary: PathBuf,
}

impl OnePasswordSource {
    /// Build a source named `name` by locating `op` in the PATH
    /// (with [`OP_BIN_ENV`] as an explicit override).
    pub fn new(name: impl Into<String>) -> Result<Self, OpError> {
        let op_binary = find_op_binary().ok_or(OpError::BinaryNotFound)?;
        Ok(Self {
            name: name.into(),
            account: None,
            op_binary,
        })
    }

    /// Build a source with an explicit `op` binary path. Used by
    /// integration tests that point at a mock script in a tempdir.
    pub fn with_binary(name: impl Into<String>, op_binary: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            account: None,
            op_binary: op_binary.into(),
        }
    }

    /// Pin a specific 1Password account. Without this `op` uses
    /// the user's default-signed-in account.
    pub fn with_account(mut self, account: impl Into<String>) -> Self {
        self.account = Some(account.into());
        self
    }

    /// Borrow the resolved `op` binary path.
    pub fn op_binary(&self) -> &Path {
        &self.op_binary
    }

    /// Borrow the configured 1Password account, if any.
    pub fn account(&self) -> Option<&str> {
        self.account.as_deref()
    }

    fn build_command(&self) -> Command {
        let mut cmd = Command::new(&self.op_binary);
        cmd.kill_on_drop(true);
        if let Some(acct) = &self.account {
            cmd.args(["--account", acct]);
        }
        cmd
    }
}

#[async_trait]
impl SecretSource for OnePasswordSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::READ
            | Capabilities::LIST
            | Capabilities::VALIDATE
            | Capabilities::BIOMETRIC_PROMPT
            | Capabilities::AUDIT_LOGGED
    }

    async fn is_available(&self) -> SourceStatus {
        let mut cmd = self.build_command();
        cmd.arg("whoami");
        match cmd.output().await {
            Ok(out) if out.status.success() => SourceStatus::Available,
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                if is_not_signed_in(&stderr) {
                    SourceStatus::Locked
                } else {
                    SourceStatus::Error(format!("op whoami failed: {}", stderr.trim()))
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => SourceStatus::NotInstalled,
            Err(e) => SourceStatus::Error(format!("failed to invoke op: {e}")),
        }
    }

    async fn get(&self, reference: &str) -> Result<Option<GetOutcome>, SourceError> {
        self.validate(reference).await?;

        let mut cmd = self.build_command();
        cmd.args(["read", reference]);
        let out = cmd.output().await.map_err(|e| SourceError::Io {
            name: self.name.clone(),
            source: e,
        })?;
        if out.status.success() {
            let value = String::from_utf8_lossy(&out.stdout)
                .trim_end_matches(['\n', '\r'])
                .to_owned();
            // op never returns success with empty stdout for an
            // existing entry, but be defensive.
            if value.is_empty() {
                return Ok(None);
            }
            return Ok(Some(GetOutcome {
                value: SecretString::from(value),
                // 1Password has no native lease concept.
                lease_duration: None,
            }));
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if is_not_signed_in(&stderr) {
            return Err(SourceError::Locked {
                name: self.name.clone(),
            });
        }
        if is_not_found(&stderr) {
            return Ok(None);
        }
        Err(SourceError::Upstream {
            name: self.name.clone(),
            message: format!("op read failed: {}", stderr.trim()),
        })
    }

    async fn list(&self) -> Result<Vec<RemoteRef>, SourceError> {
        let mut cmd = self.build_command();
        cmd.args(["item", "list", "--format=json"]);
        let out = cmd.output().await.map_err(|e| SourceError::Io {
            name: self.name.clone(),
            source: e,
        })?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if is_not_signed_in(&stderr) {
                return Err(SourceError::Locked {
                    name: self.name.clone(),
                });
            }
            return Err(SourceError::Upstream {
                name: self.name.clone(),
                message: format!("op item list failed: {}", stderr.trim()),
            });
        }
        let items: Vec<OpItemSummary> =
            serde_json::from_slice(&out.stdout).map_err(|e| SourceError::Upstream {
                name: self.name.clone(),
                message: format!("op item list returned malformed JSON: {e}"),
            })?;
        Ok(items
            .into_iter()
            .map(|item| {
                let vault_label = item.vault.name.unwrap_or(item.vault.id);
                RemoteRef {
                    reference: format!("op://{vault_label}/{}", item.title),
                    display: Some(item.title),
                }
            })
            .collect())
    }

    async fn validate(&self, reference: &str) -> Result<(), SourceError> {
        if !reference.starts_with(OP_URL_PREFIX) {
            return Err(SourceError::BadReference {
                name: self.name.clone(),
                reference: reference.to_owned(),
                reason: format!("1Password reference must start with `{OP_URL_PREFIX}`"),
            });
        }
        let tail = &reference[OP_URL_PREFIX.len()..];
        let segments: Vec<&str> = tail.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() < 2 {
            return Err(SourceError::BadReference {
                name: self.name.clone(),
                reference: reference.to_owned(),
                reason: "op:// reference must include at least <vault>/<item>".to_owned(),
            });
        }
        Ok(())
    }
}

// =============================================================================
// Internals
// =============================================================================

fn find_op_binary() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var(OP_BIN_ENV)
        && !custom.is_empty()
    {
        return Some(PathBuf::from(custom));
    }
    let path_value = std::env::var("PATH").ok()?;
    for entry in std::env::split_paths(&path_value) {
        let candidate = entry.join(DEFAULT_OP_BINARY);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let with_ext = entry.join(format!("{DEFAULT_OP_BINARY}.exe"));
            if with_ext.is_file() {
                return Some(with_ext);
            }
        }
    }
    None
}

fn is_not_signed_in(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("not currently signed in")
        || s.contains("session has ended")
        || s.contains("you are not signed in")
}

fn is_not_found(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("isn't an item")
        || s.contains("not found")
        || s.contains("doesn't exist")
        || s.contains("could not find")
}

#[derive(Debug, Deserialize)]
struct OpItemSummary {
    #[allow(dead_code)] // forward-compat: id may be useful later
    id: String,
    title: String,
    vault: OpVaultRef,
}

#[derive(Debug, Deserialize)]
struct OpVaultRef {
    id: String,
    name: Option<String>,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- Pure-data trait surface -----------------------------------

    #[test]
    fn name_returns_constructor_value() {
        let s = OnePasswordSource::with_binary("1p-personal", "/bin/false");
        assert_eq!(s.name(), "1p-personal");
    }

    #[test]
    fn account_starts_unset_and_with_account_pins_it() {
        let s = OnePasswordSource::with_binary("1p", "/bin/false");
        assert!(s.account().is_none());
        let s = s.with_account("personal.example.1password.com");
        assert_eq!(s.account(), Some("personal.example.1password.com"));
    }

    #[test]
    fn op_binary_returns_constructor_value() {
        let s = OnePasswordSource::with_binary("1p", "/usr/local/bin/op");
        assert_eq!(s.op_binary(), Path::new("/usr/local/bin/op"));
    }

    #[test]
    fn capabilities_match_adr_021_section_8_one_password() {
        let s = OnePasswordSource::with_binary("1p", "/bin/false");
        let caps = s.capabilities();
        assert!(caps.contains(Capabilities::READ));
        assert!(caps.contains(Capabilities::LIST));
        assert!(caps.contains(Capabilities::VALIDATE));
        assert!(caps.contains(Capabilities::BIOMETRIC_PROMPT));
        assert!(caps.contains(Capabilities::AUDIT_LOGGED));
        // Write is intentionally omitted in v1 (per ADR-021 §8).
        assert!(!caps.contains(Capabilities::WRITE));
        assert!(!caps.contains(Capabilities::ROTATE));
    }

    #[test]
    fn requires_credential_returns_none_handled_via_native_signin() {
        // 1Password's native auth (op signin / Touch-ID session)
        // is what the router treats as a Sentinel — the trait
        // default `None` is correct here because the *default*
        // configuration handles auth natively. A user who wires
        // an explicit token would override with `Some(...)`.
        let s = OnePasswordSource::with_binary("1p", "/bin/false");
        assert!(s.requires_credential().is_none());
    }

    // -- Reference validation --------------------------------------

    #[tokio::test]
    async fn validate_accepts_well_formed_op_url() {
        let s = OnePasswordSource::with_binary("1p", "/bin/false");
        s.validate("op://Personal/GitHub PAT/credential")
            .await
            .unwrap();
        s.validate("op://Personal/GitHub").await.unwrap();
    }

    #[tokio::test]
    async fn validate_rejects_non_op_url() {
        let s = OnePasswordSource::with_binary("1p", "/bin/false");
        let err = s.validate("vault://something").await.unwrap_err();
        match err {
            SourceError::BadReference {
                reference, reason, ..
            } => {
                assert_eq!(reference, "vault://something");
                assert!(reason.contains("op://"));
            }
            other => panic!("expected BadReference, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn validate_rejects_op_url_without_item_segment() {
        let s = OnePasswordSource::with_binary("1p", "/bin/false");
        let err = s.validate("op://OnlyVault").await.unwrap_err();
        assert!(matches!(err, SourceError::BadReference { .. }));
    }

    #[tokio::test]
    async fn get_rejects_non_op_url_before_spawning_op() {
        let s = OnePasswordSource::with_binary("1p", "/bin/false");
        let err = s.get("not-an-op-url").await.unwrap_err();
        assert!(matches!(err, SourceError::BadReference { .. }));
    }

    // -- Stderr-classification helpers -----------------------------

    #[test]
    fn is_not_signed_in_recognises_op_phrases() {
        assert!(is_not_signed_in("[ERROR] You are not currently signed in"));
        assert!(is_not_signed_in(
            "Your session has ended; please sign in again"
        ));
        assert!(!is_not_signed_in("[ERROR] item not found"));
    }

    #[test]
    fn is_not_found_recognises_op_phrases() {
        assert!(is_not_found("[ERROR] \"foo\" isn't an item"));
        assert!(is_not_found("not found"));
        assert!(!is_not_found("[ERROR] not currently signed in"));
    }

    // -- Mock-binary integration -----------------------------------

    #[cfg(unix)]
    fn write_executable_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    /// Mock `op` that pretends to be signed in and returns canned
    /// responses for whoami/read/item-list.
    #[cfg(unix)]
    fn mock_op_signed_in(dir: &Path) -> PathBuf {
        let body = r##"#!/bin/sh
# Strip --account NAME if present so the rest of the args line up.
while [ "$1" = "--account" ]; do shift 2; done
case "$1" in
  whoami)
    echo 'URL: https://test.1password.com'
    exit 0
    ;;
  read)
    case "$2" in
      "op://Personal/GitHub/credential") echo "ghp-fixture-value"; exit 0 ;;
      *) echo "[ERROR] \"$2\" isn't an item" >&2; exit 1 ;;
    esac
    ;;
  item)
    if [ "$2" = "list" ]; then
      cat <<JSON
[{"id":"abc","title":"GitHub","vault":{"id":"v1","name":"Personal"}},
 {"id":"def","title":"AWS","vault":{"id":"v1","name":"Personal"}}]
JSON
      exit 0
    fi
    ;;
esac
exit 2
"##;
        write_executable_script(dir, "op", body)
    }

    /// Mock `op` that always reports "not signed in".
    #[cfg(unix)]
    fn mock_op_signed_out(dir: &Path) -> PathBuf {
        let body = r#"#!/bin/sh
echo '[ERROR] You are not currently signed in' >&2
exit 1
"#;
        write_executable_script(dir, "op", body)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn is_available_returns_available_when_op_whoami_succeeds() {
        let dir = tempfile::TempDir::new().unwrap();
        let bin = mock_op_signed_in(dir.path());
        let s = OnePasswordSource::with_binary("1p-test", bin);
        assert_eq!(s.is_available().await, SourceStatus::Available);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn is_available_returns_locked_when_signed_out() {
        let dir = tempfile::TempDir::new().unwrap();
        let bin = mock_op_signed_out(dir.path());
        let s = OnePasswordSource::with_binary("1p-test", bin);
        assert_eq!(s.is_available().await, SourceStatus::Locked);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn is_available_returns_not_installed_when_binary_missing() {
        let s =
            OnePasswordSource::with_binary("1p-test", "/totally-nonexistent/devboy-test/op-binary");
        assert_eq!(s.is_available().await, SourceStatus::NotInstalled);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn get_returns_value_for_known_op_url() {
        use secrecy::ExposeSecret;
        let dir = tempfile::TempDir::new().unwrap();
        let bin = mock_op_signed_in(dir.path());
        let s = OnePasswordSource::with_binary("1p-test", bin);
        let outcome = s
            .get("op://Personal/GitHub/credential")
            .await
            .unwrap()
            .expect("expected Some for the canned reference");
        assert_eq!(outcome.value.expose_secret(), "ghp-fixture-value");
        assert!(outcome.lease_duration.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn get_returns_none_when_op_says_not_an_item() {
        let dir = tempfile::TempDir::new().unwrap();
        let bin = mock_op_signed_in(dir.path());
        let s = OnePasswordSource::with_binary("1p-test", bin);
        let outcome = s.get("op://Personal/Unknown/credential").await.unwrap();
        assert!(outcome.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn get_returns_locked_when_signed_out() {
        let dir = tempfile::TempDir::new().unwrap();
        let bin = mock_op_signed_out(dir.path());
        let s = OnePasswordSource::with_binary("1p-test", bin);
        let err = s.get("op://Personal/x/y").await.unwrap_err();
        assert!(matches!(err, SourceError::Locked { .. }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn list_parses_op_item_list_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let bin = mock_op_signed_in(dir.path());
        let s = OnePasswordSource::with_binary("1p-test", bin);
        let entries = s.list().await.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].reference, "op://Personal/GitHub");
        assert_eq!(entries[0].display.as_deref(), Some("GitHub"));
        assert_eq!(entries[1].reference, "op://Personal/AWS");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn account_arg_is_passed_through_to_op() {
        // The mock script strips `--account <name>` if present;
        // both with-account and without-account configurations
        // reach the same canned response, so this test verifies
        // we don't break the dispatch when --account is set.
        let dir = tempfile::TempDir::new().unwrap();
        let bin = mock_op_signed_in(dir.path());
        let s = OnePasswordSource::with_binary("1p-test", bin)
            .with_account("personal.example.1password.com");
        assert_eq!(s.is_available().await, SourceStatus::Available);
        let outcome = s
            .get("op://Personal/GitHub/credential")
            .await
            .unwrap()
            .unwrap();
        use secrecy::ExposeSecret;
        assert_eq!(outcome.value.expose_secret(), "ghp-fixture-value");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dyn_compat_smoke() {
        let s: Box<dyn SecretSource> = Box::new(OnePasswordSource::with_binary("1p", "/bin/false"));
        assert_eq!(s.name(), "1p");
        assert!(s.capabilities().contains(Capabilities::AUDIT_LOGGED));
    }
}
