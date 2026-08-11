//! Pattern catalogue for `devboy-tools` secrets.
//!
//! A *secret pattern* is everything we know about a *kind* of secret —
//! "what does a GitHub Personal Access Token look like, where does the
//! user obtain a fresh one, how often should it be rotated, how can we
//! tell whether a candidate value is still alive". One pattern, many
//! consumers:
//!
//! - The **secret store** (ADR-020 / ADR-023) reads `format_regex` for
//!   validation, `metadata().retrieval_url_template` for the
//!   `[Open URL]` button in the rotation flow, `rotation()` for the
//!   `doctor` cadence reminder, `liveness()` for the `secrets validate`
//!   probe.
//! - The **OTLP sanitizer** (#240) and **`otel scan`** auditor (#242)
//!   read `format_regex` and `severity` only — they don't care about
//!   retrieval or rotation, just "is this string a leaked secret?".
//!
//! The trait is **layered** so the two consumers can ignore the parts
//! they don't need without forcing the catalogue to fill in fields it
//! doesn't have. Mandatory: `id`, `display_name`, `format_regex`,
//! `severity`. Optional: `metadata`, `rotation`, `liveness`.
//!
//! See [ADR-023] §3.6 for the design rationale.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md
//!
//! # Example
//!
//! Implementing a minimal pattern (mandatory fields only) for a
//! made-up provider:
//!
//! ```
//! use devboy_secret_patterns::{SecretPattern, Severity};
//! use regex::Regex;
//! use std::sync::OnceLock;
//!
//! struct ExampleProviderToken;
//!
//! impl SecretPattern for ExampleProviderToken {
//!     fn id(&self) -> &str { "example-provider-token" }
//!     fn display_name(&self) -> &str { "Example Provider API Token" }
//!     fn severity(&self) -> Severity { Severity::High }
//!
//!     fn format_regex(&self) -> &Regex {
//!         static R: OnceLock<Regex> = OnceLock::new();
//!         R.get_or_init(|| Regex::new(r"^example_[A-Za-z0-9]{32}$").unwrap())
//!     }
//! }
//!
//! let p = ExampleProviderToken;
//! assert_eq!(p.id(), "example-provider-token");
//! assert_eq!(p.severity(), Severity::High);
//! assert!(p.format_regex().is_match("example_abcdefghijklmnopqrstuvwxyz012345"));
//! assert!(!p.format_regex().is_match("nope"));
//! // Optional layers default to None.
//! assert!(p.metadata().is_none());
//! assert!(p.rotation().is_none());
//! assert!(p.liveness().is_none());
//! ```

#![forbid(unsafe_code)]

pub mod builtin;
pub mod scrubber;
pub mod user;

pub use builtin::{BUILTINS, Builtin, builtins, find};
pub use user::{Catalogue, LoadError, LoadWarning, LoadWarningKind, UserPattern, UserPatternFile};

use std::borrow::Cow;
use std::fmt;

use regex::Regex;
use serde::{Deserialize, Serialize};

// =============================================================================
// Severity
// =============================================================================

/// How dangerous a leak of this kind of secret is.
///
/// Drives the colour/icon shown by `doctor` and the OTEL scan auditor.
/// `serde` produces lowercase tokens (`"low"`, `"medium"`, `"high"`) —
/// the catalogue is consumed by both internal Rust code and external
/// JSON tooling, so a stable wire shape matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Recommendation to redact, no immediate alarm. Examples:
    /// long-lived but read-only public-data tokens, hashed identifiers
    /// that still decode to PII through some lookup.
    Low,
    /// Should not appear in transcripts or logs. Examples: PII (email,
    /// phone), file paths revealing project structure, raw API
    /// payloads that may contain user data.
    Medium,
    /// Hard credential — API tokens, OAuth tokens, private keys,
    /// cloud-provider access keys, database connection strings with
    /// embedded passwords. A leak here is an incident.
    High,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        })
    }
}

// =============================================================================
// Optional metadata layer
// =============================================================================

/// Optional descriptive metadata for a pattern. Consumed by the secret
/// store's `pattern_id` inheritance (epic phase P2.4) and by the UI.
///
/// String fields are [`Cow<'static, str>`] so both built-in patterns
/// (which carry `&'static str` literals via `Cow::Borrowed`) and
/// user-loaded patterns (which carry owned `String`s deserialised from
/// `~/.devboy/secrets/patterns.d/*.toml`) can populate them without
/// either side leaking memory or leaning on `Box::leak`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternMetadata {
    /// Stable provider identifier, lowercase ASCII (`"github"`,
    /// `"gitlab"`, `"openai"`, …). Used as the second segment in
    /// suggested ADR-020 paths and as the routing key for liveness
    /// probes.
    pub provider_id: Cow<'static, str>,
    /// URL template the user opens to obtain a fresh value. May
    /// contain `{var}` placeholders the UI substitutes; if it has no
    /// placeholders, the UI opens it verbatim.
    pub retrieval_url_template: Cow<'static, str>,
    /// Default rotation cadence in days. Drives `doctor`'s default
    /// reminder when the per-secret entry doesn't override it.
    pub default_expiry_days: Option<u32>,
    /// Hint on the API scopes this token is typically created with.
    /// Surfaced in the metadata card; does **not** validate.
    pub scopes_hint: Vec<Cow<'static, str>>,
}

// =============================================================================
// Optional rotation layer
// =============================================================================

/// How a secret of this kind is rotated.
///
/// Mirrors the same idea as
/// [`devboy_storage::RotationMethod`](https://docs.rs/devboy-storage)
/// but stays in this crate to avoid a back-edge in the dep graph and
/// because the catalogue's variant carries an extra URL template that
/// the storage `RotationMethod` does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotationMethodSpec {
    /// User rotates the secret themselves through the upstream UI;
    /// `devboy-tools` records the new value and validates liveness.
    /// The first release ships **only** this method
    /// (ADR-023 §3.5 — provider-driven rotation is deferred).
    Manual,
    /// Reserved — `devboy-tools` opens the provider's UI at the
    /// templated URL and accepts the new value through the rotation
    /// flow. Not wired in v1.
    ProviderUi {
        /// URL template (may contain `{var}` placeholders).
        url_template: &'static str,
    },
    /// Reserved — `devboy-tools` calls the provider's rotation API
    /// directly. Not wired in v1.
    ProviderApi,
}

/// Optional rotation hint for a pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationSpec {
    /// How rotation happens.
    pub method: RotationMethodSpec,
}

// =============================================================================
// Optional liveness layer
// =============================================================================

/// HTTP method for liveness probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    /// HTTP `GET`.
    Get,
    /// HTTP `POST`.
    Post,
    /// HTTP `HEAD`.
    Head,
}

impl HttpMethod {
    /// Uppercase wire form (`"GET"`, `"POST"`, `"HEAD"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Head => "HEAD",
        }
    }
}

/// How to attach the candidate secret to the liveness HTTP request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LivenessAuth {
    /// `Authorization: Bearer <secret>` header.
    Bearer,
    /// `Authorization: Basic base64(<secret>:)` header (the secret is
    /// the username; the password is empty).
    BasicUser,
    /// HTTP Basic auth with empty username and `<secret>` as password.
    BasicPassword,
    /// Custom HTTP header name carrying the raw secret.
    Header {
        /// Header name (e.g. `"PRIVATE-TOKEN"`, `"X-API-Key"`).
        name: &'static str,
    },
}

/// Liveness probe shape — currently only HTTP, but the enum gives us
/// room to add subprocess- or socket-based probes later without
/// breaking the trait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LivenessKind {
    /// Make an HTTP request and check the response status code.
    Http {
        /// Endpoint URL (typically the provider's `/me` or `/user`
        /// endpoint).
        url: &'static str,
        /// HTTP method.
        method: HttpMethod,
        /// How the secret is attached to the request.
        auth: LivenessAuth,
        /// HTTP status that means "secret is valid".
        expect_status: u16,
    },
}

/// Optional liveness specification for a pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivenessSpec {
    /// Probe kind + parameters.
    pub kind: LivenessKind,
}

// =============================================================================
// SecretPattern trait
// =============================================================================

/// One *kind* of secret in the catalogue.
///
/// Implementors are usually zero-sized types (one per pattern); the
/// catalogue (epic phase P2.2) holds them behind `&'static dyn
/// SecretPattern` references.
///
/// **Thread-safety.** The trait requires `Send + Sync` because the
/// secret store and the OTLP sanitizer both consume patterns from
/// concurrent contexts.
///
/// **Layering.** The mandatory accessors (`id`, `display_name`,
/// `format_regex`, `severity`) cover the OTLP-sanitizer / scan use
/// case. The three optional layers (`metadata`, `rotation`,
/// `liveness`) cover the secret-store use case; patterns may
/// implement all, some, or none of them. The default impl returns
/// `None` so a minimal pattern only has to write four method
/// bodies.
pub trait SecretPattern: Send + Sync {
    /// Stable identifier (lowercase, kebab-case). Used as a foreign
    /// key from the global index entry's `pattern_id` (ADR-020 §3)
    /// and as a join key with other tools that consume the
    /// catalogue.
    fn id(&self) -> &str;

    /// Human-readable name shown in `secrets describe` and in
    /// scan-tool reports.
    fn display_name(&self) -> &str;

    /// Regular expression matching valid values of this kind.
    ///
    /// Returned by reference so implementors can lazy-compile and
    /// cache the [`Regex`] (e.g. via `OnceLock`) without paying the
    /// cost on every match. The catalogue is hot-path: every secret
    /// resolution and every OTLP attribute walk hits this method.
    fn format_regex(&self) -> &Regex;

    /// Severity to attach to a leak finding for this pattern.
    fn severity(&self) -> Severity;

    /// Optional descriptive metadata (provider, retrieval URL,
    /// expiry, scopes).
    ///
    /// Default returns `None`; consumers that only need format/severity
    /// (the sanitizer and scan tools) ignore this layer.
    fn metadata(&self) -> Option<&PatternMetadata> {
        None
    }

    /// Optional rotation hint (manual vs provider-driven).
    ///
    /// Default returns `None`.
    fn rotation(&self) -> Option<&RotationSpec> {
        None
    }

    /// Optional liveness probe specification.
    ///
    /// Default returns `None`. Patterns that ship a probe let the
    /// `secrets validate` flow check whether a candidate value is
    /// currently accepted by the upstream.
    fn liveness(&self) -> Option<&LivenessSpec> {
        None
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    /// Minimal pattern — only the four mandatory methods. Used to
    /// exercise the trait's default impls.
    struct Minimal;
    impl SecretPattern for Minimal {
        fn id(&self) -> &str {
            "minimal"
        }
        fn display_name(&self) -> &str {
            "Minimal Test Pattern"
        }
        fn severity(&self) -> Severity {
            Severity::Low
        }
        fn format_regex(&self) -> &Regex {
            static R: OnceLock<Regex> = OnceLock::new();
            R.get_or_init(|| Regex::new(r"^min_[a-z0-9]{8}$").unwrap())
        }
    }

    /// Pattern that fills in every optional layer. Used to verify the
    /// optional accessors deliver what the impl supplies.
    struct Full;
    impl SecretPattern for Full {
        fn id(&self) -> &str {
            "full"
        }
        fn display_name(&self) -> &str {
            "Full Test Pattern"
        }
        fn severity(&self) -> Severity {
            Severity::High
        }
        fn format_regex(&self) -> &Regex {
            static R: OnceLock<Regex> = OnceLock::new();
            R.get_or_init(|| Regex::new(r"^full_.+$").unwrap())
        }
        fn metadata(&self) -> Option<&PatternMetadata> {
            static M: OnceLock<PatternMetadata> = OnceLock::new();
            Some(M.get_or_init(|| PatternMetadata {
                provider_id: Cow::Borrowed("test"),
                retrieval_url_template: Cow::Borrowed("https://test.example/tokens"),
                default_expiry_days: Some(90),
                scopes_hint: vec![Cow::Borrowed("read"), Cow::Borrowed("write")],
            }))
        }
        fn rotation(&self) -> Option<&RotationSpec> {
            static R: OnceLock<RotationSpec> = OnceLock::new();
            Some(R.get_or_init(|| RotationSpec {
                method: RotationMethodSpec::Manual,
            }))
        }
        fn liveness(&self) -> Option<&LivenessSpec> {
            static L: OnceLock<LivenessSpec> = OnceLock::new();
            Some(L.get_or_init(|| LivenessSpec {
                kind: LivenessKind::Http {
                    url: "https://test.example/api/me",
                    method: HttpMethod::Get,
                    auth: LivenessAuth::Bearer,
                    expect_status: 200,
                },
            }))
        }
    }

    // -- Severity --------------------------------------------------------------

    #[test]
    fn severity_serializes_lowercase() {
        // Stable wire shape — sanitizer (#240) and scan (#242) parse this.
        assert_eq!(serde_json::to_string(&Severity::Low).unwrap(), "\"low\"");
        assert_eq!(
            serde_json::to_string(&Severity::Medium).unwrap(),
            "\"medium\""
        );
        assert_eq!(serde_json::to_string(&Severity::High).unwrap(), "\"high\"");
    }

    #[test]
    fn severity_deserializes_lowercase() {
        let s: Severity = serde_json::from_str("\"high\"").unwrap();
        assert_eq!(s, Severity::High);
    }

    #[test]
    fn severity_display_matches_serde() {
        for s in [Severity::Low, Severity::Medium, Severity::High] {
            let displayed = format!("{s}");
            let serded: String = serde_json::from_value(serde_json::to_value(s).unwrap()).unwrap();
            assert_eq!(displayed, serded);
        }
    }

    #[test]
    fn severity_orders_low_below_high() {
        assert!(Severity::Low < Severity::Medium);
        assert!(Severity::Medium < Severity::High);
    }

    // -- HttpMethod ------------------------------------------------------------

    #[test]
    fn http_method_as_str_matches_uppercase_wire_form() {
        assert_eq!(HttpMethod::Get.as_str(), "GET");
        assert_eq!(HttpMethod::Post.as_str(), "POST");
        assert_eq!(HttpMethod::Head.as_str(), "HEAD");
    }

    // -- Trait: mandatory accessors ------------------------------------------

    #[test]
    fn minimal_pattern_exposes_mandatory_accessors() {
        let p = Minimal;
        assert_eq!(p.id(), "minimal");
        assert_eq!(p.display_name(), "Minimal Test Pattern");
        assert_eq!(p.severity(), Severity::Low);
        assert!(p.format_regex().is_match("min_abc12345"));
        assert!(!p.format_regex().is_match("nope"));
    }

    #[test]
    fn minimal_pattern_optional_accessors_default_to_none() {
        let p = Minimal;
        assert!(p.metadata().is_none());
        assert!(p.rotation().is_none());
        assert!(p.liveness().is_none());
    }

    // -- Trait: optional accessors -------------------------------------------

    #[test]
    fn full_pattern_exposes_metadata_layer() {
        let p = Full;
        let m = p.metadata().expect("Full provides metadata");
        assert_eq!(m.provider_id, "test");
        assert_eq!(m.retrieval_url_template, "https://test.example/tokens");
        assert_eq!(m.default_expiry_days, Some(90));
        assert_eq!(
            m.scopes_hint,
            vec![Cow::Borrowed("read"), Cow::Borrowed("write")]
        );
    }

    #[test]
    fn full_pattern_exposes_rotation_layer() {
        let p = Full;
        let r = p.rotation().expect("Full provides rotation");
        assert_eq!(r.method, RotationMethodSpec::Manual);
    }

    #[test]
    fn full_pattern_exposes_liveness_layer() {
        let p = Full;
        let l = p.liveness().expect("Full provides liveness");
        match &l.kind {
            LivenessKind::Http {
                url,
                method,
                auth,
                expect_status,
            } => {
                assert_eq!(*url, "https://test.example/api/me");
                assert_eq!(*method, HttpMethod::Get);
                assert_eq!(*auth, LivenessAuth::Bearer);
                assert_eq!(*expect_status, 200);
            }
        }
    }

    // -- Trait object --------------------------------------------------------

    #[test]
    fn trait_is_object_safe_and_send_sync() {
        // The trait must be usable behind `&dyn SecretPattern` for the
        // catalogue (P2.2) and `Box<dyn SecretPattern>` for user-loaded
        // patterns (P2.3). Send+Sync because both the secret store and
        // the OTLP sanitizer hold patterns concurrently.
        let patterns: Vec<&dyn SecretPattern> = vec![&Minimal, &Full];
        assert_eq!(patterns.len(), 2);

        fn assert_send_sync<T: Send + Sync + ?Sized>() {}
        assert_send_sync::<dyn SecretPattern>();
    }

    // -- LivenessAuth coverage ------------------------------------------------

    #[test]
    fn liveness_auth_header_variant_carries_name() {
        let a = LivenessAuth::Header {
            name: "PRIVATE-TOKEN",
        };
        match a {
            LivenessAuth::Header { name } => assert_eq!(name, "PRIVATE-TOKEN"),
            _ => panic!("expected Header variant"),
        }
    }
}
