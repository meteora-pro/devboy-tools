//! `SecretSource` trait + supporting types per [ADR-021] §1.
//!
//! A *source* is any backend able to answer questions about secrets:
//! the OS keychain, the encrypted local vault from ADR-023, the
//! 1Password CLI, a HashiCorp Vault server, an env-store mount, or a
//! community subprocess plugin. The router (epic phase P5.2 / P5.3)
//! maps an ADR-020 path to a `(source, reference)` pair and then
//! invokes the source's [`SecretSource::get`] / `list` / `validate`
//! through this trait.
//!
//! Two design choices worth knowing:
//!
//! 1. **Sources do not understand ADR-020 paths.** They take an
//!    opaque `reference: &str`. Mapping a path to a reference is the
//!    router's job. This separation lets a source plugin be written
//!    without any awareness of the manifest layer, which keeps the
//!    plugin protocol (P15) tiny.
//! 2. **Capabilities are explicit, not inferred.** A source whose
//!    only operation is `READ` declares exactly that. The router
//!    refuses to call `WRITE` on it without trying — the failure is
//!    structured (`SourceError::UnsupportedCapability`) rather than
//!    a network round-trip that returns "method not allowed".
//!
//! ## What this module does **not** define
//!
//! - The router itself (P5.2).
//! - Any specific source impl (`keychain`, `local-vault`,
//!   `1password`, …) — those land in P6.
//! - The recursion check that enforces the
//!   `__sources/<source>/<profile>` invariant — that's P5.5.
//! - The `Capabilities`-aware in-memory cache — P5.4.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md

use std::time::Duration;

use async_trait::async_trait;
use bitflags::bitflags;
use secrecy::SecretString;
use thiserror::Error;

use crate::SecretPath;

// =============================================================================
// Capabilities
// =============================================================================

bitflags! {
    /// What a source can do, plus two descriptive flags consumed by
    /// `doctor` and the agent provisioning surface.
    ///
    /// Per [ADR-021] §1.1. The first five bits are *operational* —
    /// the router refuses to dispatch an operation unless the
    /// matching bit is set. The last two bits are *descriptive* —
    /// they let agents and `doctor` reason about UX trade-offs
    /// without trying the operation:
    ///
    /// - [`BIOMETRIC_PROMPT`](Self::BIOMETRIC_PROMPT) — the source
    ///   *may* prompt for user-presence (TouchID, PIN, passphrase)
    ///   on at least one of its operations in its default
    ///   configuration. Single-bit flag on the source as a whole;
    ///   the router does not infer per-operation cost from it.
    /// - [`AUDIT_LOGGED`](Self::AUDIT_LOGGED) — every read is
    ///   durably logged on the upstream (Vault audit log, 1Password
    ///   account activity). Surfaced in `doctor` so the user knows
    ///   their reads are observable.
    ///
    /// Typical declarations:
    ///
    /// | Source            | Capabilities                                                   |
    /// |-------------------|----------------------------------------------------------------|
    /// | env-store         | `READ`                                                         |
    /// | keychain          | `READ \| LIST \| VALIDATE \| WRITE`                            |
    /// | local-vault       | `READ \| LIST \| VALIDATE \| WRITE \| ROTATE \| BIOMETRIC_PROMPT` |
    /// | 1password (cli)   | `READ \| LIST \| VALIDATE \| BIOMETRIC_PROMPT \| AUDIT_LOGGED` |
    /// | vault (kv v2)     | `READ \| LIST \| VALIDATE \| WRITE \| ROTATE \| AUDIT_LOGGED`   |
    ///
    /// [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md
    #[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Default)]
    pub struct Capabilities: u32 {
        /// The source can answer `get(reference)`.
        const READ              = 0b0000_0001;
        /// The source can enumerate its inventory via `list()`.
        const LIST              = 0b0000_0010;
        /// The source can validate that a reference is well-formed
        /// without round-tripping for the value.
        const VALIDATE          = 0b0000_0100;
        /// The source can write a new value at `reference`.
        const WRITE             = 0b0000_1000;
        /// The source can rotate (replace + invalidate prior) a
        /// value at `reference`.
        const ROTATE            = 0b0001_0000;
        /// Descriptive — the source may prompt the user for
        /// biometrics / a PIN / a passphrase on at least one of
        /// its operations.
        const BIOMETRIC_PROMPT  = 0b0010_0000;
        /// Descriptive — every read is observable in the upstream's
        /// audit log.
        const AUDIT_LOGGED      = 0b0100_0000;
    }
}

// =============================================================================
// Source-credential reference
// =============================================================================

/// What a source needs to be operational.
///
/// Per [ADR-021] §4: a Vault source needs a Vault token, a 1Password
/// source needs a signed-in account session, etc. The credential
/// reference is *either*:
///
/// - A path under the reserved `__sources/` namespace pointing at
///   the credential value in the credential store, or
/// - A source-defined *sentinel string* (`biometric`,
///   `default-profile`, `oauth`) that the source plugin interprets
///   itself.
///
/// The router uses this on configuration load to enforce the
/// recursion invariant (P5.5): a source-credential must resolve
/// through a source whose own [`SecretSource::requires_credential`]
/// is `None`. That's why this enum exists — sources need a way to
/// say "my credential lives at this path" *or* "my credential lives
/// inside me; ask my native unlock". Without the distinction the
/// recursion check would reject 1Password's biometric session as a
/// missing credential.
///
/// [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialRef {
    /// A path under the reserved `__sources/` namespace. The router
    /// fetches the value from the credential store and hands it to
    /// the source's init step.
    Path(SecretPath),
    /// A source-specific sentinel like `biometric` or
    /// `default-profile`. Opaque to the router; meaningful only to
    /// the source plugin.
    Sentinel(String),
}

// =============================================================================
// Source status
// =============================================================================

/// Snapshot of a source's connectivity at the moment of the call.
///
/// Returned by [`SecretSource::is_available`]. The router uses it
/// to decide whether to retry the default route against a fallback
/// (per ADR-021 §2 step 3) and `doctor` uses it to render the
/// per-source health check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceStatus {
    /// Backend is reachable and ready to answer queries.
    Available,
    /// Backend is reachable but needs an unlock step before use
    /// (e.g. local-vault is locked, 1Password account is signed
    /// out).
    Locked,
    /// The CLI / SDK / system service this source depends on is
    /// not present at all (e.g. `op` not installed).
    NotInstalled,
    /// Anything else — surfaced verbatim to `doctor`.
    Error(String),
}

impl SourceStatus {
    /// Convenience — `true` only when the source is fully
    /// operational. Treats `Locked` / `NotInstalled` / `Error` as
    /// not-available.
    pub fn is_available(&self) -> bool {
        matches!(self, SourceStatus::Available)
    }
}

// =============================================================================
// Get outcome
// =============================================================================

/// Successful payload of [`SecretSource::get`].
///
/// `value` is wrapped in [`SecretString`] so the plaintext doesn't
/// accidentally land in a `Debug` print or a log line. Only the
/// final consumer (an HTTP header builder, an SDK call site)
/// should call `expose_secret()`.
///
/// `lease_duration` lets the router's adaptive-TTL cache (P5.4)
/// shrink its TTL to fit short-lived dynamic secrets — Vault's
/// dynamic database creds, AWS STS tokens, etc. Sources that don't
/// have a lease concept return `None`, in which case the cache uses
/// its default TTL.
#[derive(Debug)]
pub struct GetOutcome {
    /// The secret value.
    pub value: SecretString,
    /// Upstream-reported lease duration, if any. The router caps
    /// the cache TTL at this value (per ADR-021 §7).
    pub lease_duration: Option<Duration>,
}

// =============================================================================
// List entry
// =============================================================================

/// One entry returned by [`SecretSource::list`].
///
/// `reference` is the backend-specific opaque string the router
/// would feed back into `get()`. `display` is the human-readable
/// label the source can offer when one is available — TUI
/// discovery (P11.4) renders it in the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRef {
    /// Backend-specific reference string.
    pub reference: String,
    /// Optional human-readable label (1Password item title, Vault
    /// path, env-var name, etc.). When absent, callers fall back to
    /// `reference`.
    pub display: Option<String>,
}

// =============================================================================
// Errors
// =============================================================================

/// All the ways a source operation can fail.
#[derive(Debug, Error)]
pub enum SourceError {
    /// The source's [`SecretSource::is_available`] check returned
    /// something other than `Available`. Caller may retry against a
    /// fallback or surface the underlying status.
    #[error("source `{name}` is not available: {reason}")]
    Unavailable {
        /// Source name (matches [`SecretSource::name`]).
        name: String,
        /// Human-readable detail — typically the `Display` of the
        /// underlying [`SourceStatus`] variant.
        reason: String,
    },

    /// The router asked the source to do something its
    /// [`SecretSource::capabilities`] said it could not.
    #[error("source `{name}` does not support {capability:?}")]
    UnsupportedCapability {
        /// Source name.
        name: String,
        /// The capability that was missing.
        capability: Capabilities,
    },

    /// The reference string isn't well-formed for this backend
    /// (e.g. malformed `op://` URL, invalid Vault path).
    #[error("source `{name}` rejected reference `{reference}`: {reason}")]
    BadReference {
        /// Source name.
        name: String,
        /// The reference the source rejected.
        reference: String,
        /// Human-readable detail.
        reason: String,
    },

    /// The source itself errored (network failure, upstream
    /// returned 5xx, plugin subprocess crashed, …).
    #[error("source `{name}` upstream error: {message}")]
    Upstream {
        /// Source name.
        name: String,
        /// Human-readable detail.
        message: String,
    },

    /// The source needs an unlock step before it can answer this
    /// operation. Typically maps to [`SourceStatus::Locked`].
    #[error("source `{name}` requires unlock")]
    Locked {
        /// Source name.
        name: String,
    },

    /// I/O error talking to the source (mostly subprocess stdio /
    /// socket I/O for plugin-style sources).
    #[error("I/O error talking to source `{name}`")]
    Io {
        /// Source name.
        name: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

// =============================================================================
// SecretSource trait
// =============================================================================

/// Backend-agnostic interface for any "place secrets live."
///
/// Per [ADR-021] §1. Implementations are sync-Sendable so they can
/// be stored in a `Box<dyn SecretSource>` and shared across tasks.
/// Operations are `async` so HTTP-backed sources (Vault, future
/// cloud KMS) don't have to block; the local sources
/// (`keychain`, `local-vault`, `env-store`) await trivially.
///
/// ## Default-method behaviour
///
/// `list` and `validate` default to returning
/// [`SourceError::UnsupportedCapability`]. A source that declares
/// the matching capability bit *must* override the corresponding
/// method, or the router's invariant ("declared cap → call works")
/// breaks. (A debug-build assertion in P5.3 will catch the
/// mismatch.)
///
/// `requires_credential` defaults to `None`, which is what most
/// sources want — only Vault, 1Password, AWS, and friends override.
///
/// [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md
#[async_trait]
pub trait SecretSource: Send + Sync {
    /// Stable identifier of this source instance. Matches the
    /// `name` field of `[[source]]` in `sources.toml`.
    fn name(&self) -> &str;

    /// Static capability bitset for this source. Cheap; called by
    /// the router on every dispatch so do not allocate.
    fn capabilities(&self) -> Capabilities;

    /// What the source itself needs to be unlocked / operational.
    /// `None` for self-contained sources (keychain, env-store).
    /// `Some(Path(_))` or `Some(Sentinel(_))` for sources that
    /// depend on something the framework has to provide first
    /// (Vault token, 1Password account session).
    fn requires_credential(&self) -> Option<CredentialRef> {
        None
    }

    /// Probe the backend. Cheap when the source caches the answer;
    /// otherwise this might shell out (`op account list`) or open a
    /// short-lived connection. Used by the router's fallback logic
    /// (ADR-021 §2 step 3) and by `doctor`.
    async fn is_available(&self) -> SourceStatus;

    /// Fetch the value at `reference`. `Ok(None)` means "the source
    /// is operational but has no entry there"; an error means the
    /// source itself failed to talk to its upstream.
    async fn get(&self, reference: &str) -> Result<Option<GetOutcome>, SourceError>;

    /// Enumerate the source's inventory. Default implementation
    /// reports unsupported. A source that declares
    /// [`Capabilities::LIST`] must override.
    async fn list(&self) -> Result<Vec<RemoteRef>, SourceError> {
        Err(SourceError::UnsupportedCapability {
            name: self.name().to_owned(),
            capability: Capabilities::LIST,
        })
    }

    /// Confirm `reference` is well-formed for this backend without
    /// round-tripping for the value (e.g. parse the `op://` URL,
    /// check the Vault path syntax). Default implementation reports
    /// unsupported.
    async fn validate(&self, reference: &str) -> Result<(), SourceError> {
        let _ = reference;
        Err(SourceError::UnsupportedCapability {
            name: self.name().to_owned(),
            capability: Capabilities::VALIDATE,
        })
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    // -- Capabilities -----------------------------------------------------

    #[test]
    fn capabilities_bit_values_match_adr_021() {
        // The exact bit positions are part of the on-the-wire
        // contract for the future plugin protocol (P15). Pin them
        // so a refactor doesn't accidentally renumber.
        assert_eq!(Capabilities::READ.bits(), 0b0000_0001);
        assert_eq!(Capabilities::LIST.bits(), 0b0000_0010);
        assert_eq!(Capabilities::VALIDATE.bits(), 0b0000_0100);
        assert_eq!(Capabilities::WRITE.bits(), 0b0000_1000);
        assert_eq!(Capabilities::ROTATE.bits(), 0b0001_0000);
        assert_eq!(Capabilities::BIOMETRIC_PROMPT.bits(), 0b0010_0000);
        assert_eq!(Capabilities::AUDIT_LOGGED.bits(), 0b0100_0000);
    }

    #[test]
    fn capabilities_compose_via_bitor() {
        let read_only = Capabilities::READ | Capabilities::LIST | Capabilities::VALIDATE;
        assert!(read_only.contains(Capabilities::READ));
        assert!(read_only.contains(Capabilities::LIST));
        assert!(read_only.contains(Capabilities::VALIDATE));
        assert!(!read_only.contains(Capabilities::WRITE));
        assert!(!read_only.contains(Capabilities::ROTATE));
    }

    #[test]
    fn capabilities_default_is_empty() {
        let empty: Capabilities = Capabilities::default();
        assert_eq!(empty.bits(), 0);
        assert!(empty.is_empty());
    }

    // -- SourceStatus -----------------------------------------------------

    #[test]
    fn source_status_is_available_only_for_available_variant() {
        assert!(SourceStatus::Available.is_available());
        assert!(!SourceStatus::Locked.is_available());
        assert!(!SourceStatus::NotInstalled.is_available());
        assert!(!SourceStatus::Error("disk full".into()).is_available());
    }

    // -- CredentialRef ----------------------------------------------------

    #[test]
    fn credential_ref_can_be_a_sources_path() {
        let path = SecretPath::parse_internal("__sources/vault-team/deploy").unwrap();
        let cr = CredentialRef::Path(path.clone());
        match cr {
            CredentialRef::Path(p) => assert_eq!(p, path),
            CredentialRef::Sentinel(_) => panic!("wrong variant"),
        }
    }

    #[test]
    fn credential_ref_can_be_a_sentinel() {
        let cr = CredentialRef::Sentinel("biometric".into());
        match cr {
            CredentialRef::Sentinel(s) => assert_eq!(s, "biometric"),
            CredentialRef::Path(_) => panic!("wrong variant"),
        }
    }

    // -- Default trait methods -------------------------------------------

    /// Bare-minimum source: only declares READ. Used to verify that
    /// the default `list` / `validate` impls bail with a structured
    /// error and that `requires_credential` defaults to `None`.
    struct ReadOnlySource;

    #[async_trait]
    impl SecretSource for ReadOnlySource {
        fn name(&self) -> &str {
            "read-only-fixture"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::READ
        }
        async fn is_available(&self) -> SourceStatus {
            SourceStatus::Available
        }
        async fn get(&self, _reference: &str) -> Result<Option<GetOutcome>, SourceError> {
            Ok(Some(GetOutcome {
                value: SecretString::from("hunter2"),
                lease_duration: None,
            }))
        }
    }

    #[tokio::test]
    async fn default_list_reports_unsupported_capability() {
        let s = ReadOnlySource;
        let err = s.list().await.unwrap_err();
        match err {
            SourceError::UnsupportedCapability { name, capability } => {
                assert_eq!(name, "read-only-fixture");
                assert_eq!(capability, Capabilities::LIST);
            }
            other => panic!("expected UnsupportedCapability, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn default_validate_reports_unsupported_capability() {
        let s = ReadOnlySource;
        let err = s.validate("anything").await.unwrap_err();
        match err {
            SourceError::UnsupportedCapability { name, capability } => {
                assert_eq!(name, "read-only-fixture");
                assert_eq!(capability, Capabilities::VALIDATE);
            }
            other => panic!("expected UnsupportedCapability, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn default_requires_credential_returns_none() {
        let s = ReadOnlySource;
        assert!(s.requires_credential().is_none());
    }

    // -- dyn-trait usage --------------------------------------------------

    #[tokio::test]
    async fn source_works_through_dyn_box() {
        // The router (P5.2) will store sources in a
        // `HashMap<String, Box<dyn SecretSource>>`, so the trait
        // must be dyn-compatible. Smoke-test that here.
        let s: Box<dyn SecretSource> = Box::new(ReadOnlySource);
        assert_eq!(s.name(), "read-only-fixture");
        assert_eq!(s.capabilities(), Capabilities::READ);
        let status = s.is_available().await;
        assert!(status.is_available());
        let out = s.get("ignored").await.unwrap().unwrap();
        assert_eq!(out.value.expose_secret(), "hunter2");
        assert!(out.lease_duration.is_none());
    }

    // -- SourceError display ---------------------------------------------

    #[test]
    fn source_error_display_includes_name_and_context() {
        let e = SourceError::UnsupportedCapability {
            name: "vault-team".into(),
            capability: Capabilities::WRITE,
        };
        let s = format!("{e}");
        assert!(s.contains("vault-team"));
        assert!(s.contains("WRITE"));
    }

    #[test]
    fn source_error_io_preserves_underlying() {
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "ECONNREFUSED");
        let e = SourceError::Io {
            name: "plugin".into(),
            source: io,
        };
        // The std::io::Error message is exposed via the `#[source]`
        // chain rather than the top-level Display, so verify both
        // pieces.
        assert!(format!("{e}").contains("plugin"));
        let chained = std::error::Error::source(&e).unwrap().to_string();
        assert!(chained.contains("ECONNREFUSED"));
    }
}
