//! `@secret:<path>` alias detection + resolver trait per
//! [ADR-020] §5.
//!
//! ADR-020 introduces an alias form so config files, command-line
//! argv, and HTTP request templates can reference a secret by its
//! ADR-020 path without ever storing the value alongside the
//! reference. The TOML on disk holds the alias verbatim:
//!
//! ```toml
//! [gitlab]
//! token = "@secret:team/gitlab/token-deploy"
//! ```
//!
//! This module is the *core* half of alias resolution:
//!
//! 1. [`parse_alias`] / [`is_alias`] / [`ALIAS_PREFIX`] —
//!    string-level detection. Whole-string match per ADR-020 §5;
//!    partial occurrences are not aliases.
//! 2. [`SecretResolver`] — abstract trait the config loader takes
//!    so it doesn't need to know whether the secret lives in the
//!    OS keychain, a Vault server, or the local-vault daemon.
//!    `devboy-storage` provides a concrete impl wired into the
//!    P5 router; tests can pass a `MemoryResolver`.
//!
//! Splitting detection (here) from resolution (storage) avoids a
//! circular dependency between `devboy-core` and `devboy-storage`.
//! The config loader stays free of credential-store / router
//! knowledge — it just calls `resolver.resolve(path)?` whenever
//! it sees an alias.
//!
//! # Round-trip preservation
//!
//! Aliases are *plain strings*. Serde does not magic-convert them
//! at deserialize time; the config struct sees `String` /
//! `Option<String>` and the alias stays put. Resolution happens
//! at use-site, never at load-site, so re-serializing the config
//! puts the alias back on disk unchanged. The
//! `roundtrip_preserves_alias` test pins this contract.
//!
//! [ADR-020]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md

use secrecy::SecretString;
use thiserror::Error;

/// Reserved prefix that flags an `@secret:<path>` alias. Per
/// ADR-020 §5: chosen so it cannot be accidentally interpreted
/// by a shell expansion or a templating engine.
pub const ALIAS_PREFIX: &str = "@secret:";

/// `true` iff `s` is an `@secret:<path>` alias with a non-empty
/// path. Whole-string match; partial occurrences inside a longer
/// value are not aliases per ADR-020 §5.
pub fn is_alias(s: &str) -> bool {
    parse_alias(s).is_some()
}

/// Extract the path portion of an `@secret:<path>` alias.
/// Returns `Some(path)` only when:
///
/// - the string starts with [`ALIAS_PREFIX`], and
/// - the suffix after the prefix is non-empty.
///
/// `None` otherwise — including for the bare prefix (`"@secret:"`)
/// and for strings where the prefix appears mid-value.
pub fn parse_alias(s: &str) -> Option<&str> {
    s.strip_prefix(ALIAS_PREFIX).filter(|p| !p.is_empty())
}

// =============================================================================
// Resolver trait
// =============================================================================

/// Failure modes for [`SecretResolver::resolve`].
///
/// Concrete impls in `devboy-storage` translate router/storage
/// errors into one of these variants. The config loader only
/// needs to handle the variant set, not the underlying backend
/// errors.
#[derive(Debug, Error)]
pub enum AliasResolverError {
    /// No value at `path`.
    #[error("no value for alias path '{path}'")]
    NotFound {
        /// The unresolved alias path.
        path: String,
    },

    /// Path syntax doesn't pass ADR-020 validation.
    #[error("alias path '{path}' is malformed: {reason}")]
    BadPath {
        /// The offending path.
        path: String,
        /// Human-readable detail.
        reason: String,
    },

    /// Backend (keychain, router, source plugin) errored.
    #[error("secret backend error resolving '{path}': {message}")]
    Backend {
        /// The path being resolved.
        path: String,
        /// Backend-supplied error message.
        message: String,
    },
}

/// Resolves an `@secret:<path>` alias to its current value.
///
/// `devboy-core` defines this trait so the config loader takes a
/// resolver by trait object without pulling in `devboy-storage`'s
/// router. Production wiring lives in
/// [`devboy_storage`](https://docs.rs/devboy-storage); tests
/// implement it inline against a `HashMap`.
pub trait SecretResolver: Send + Sync {
    /// Resolve `path` (the portion after `@secret:`) to its
    /// current value. Implementations consume the path verbatim;
    /// path validation belongs in the credential layer per
    /// ADR-020.
    fn resolve(&self, path: &str) -> Result<SecretString, AliasResolverError>;
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
    use std::collections::HashMap;
    use std::sync::Mutex;

    // -- parse_alias / is_alias ----------------------------------

    #[test]
    fn parse_alias_extracts_path() {
        assert_eq!(
            parse_alias("@secret:team/gitlab/token-deploy"),
            Some("team/gitlab/token-deploy")
        );
    }

    #[test]
    fn parse_alias_rejects_bare_prefix() {
        assert_eq!(parse_alias("@secret:"), None);
    }

    #[test]
    fn parse_alias_rejects_strings_without_prefix() {
        assert_eq!(parse_alias("team/gitlab/token-deploy"), None);
        assert_eq!(parse_alias(""), None);
        assert_eq!(parse_alias("not-an-alias"), None);
    }

    #[test]
    fn parse_alias_does_not_match_partial_occurrence() {
        // ADR-020 §5: alias replaces the whole field value;
        // mid-string occurrences are NOT aliases.
        assert_eq!(parse_alias("Bearer @secret:foo/bar/baz"), None);
        assert_eq!(parse_alias("foo @secret:bar/baz"), None);
    }

    #[test]
    fn is_alias_mirrors_parse_alias() {
        assert!(is_alias("@secret:foo/bar/baz"));
        assert!(!is_alias("not-an-alias"));
        assert!(!is_alias("@secret:"));
    }

    #[test]
    fn alias_prefix_constant_matches_adr_020() {
        // The exact constant is part of the user-visible contract;
        // pin it so a refactor doesn't accidentally change the
        // alias scheme.
        assert_eq!(ALIAS_PREFIX, "@secret:");
    }

    // -- Trait usage --------------------------------------------

    /// Tiny in-memory resolver for tests. Maps path → plaintext.
    struct MapResolver {
        entries: Mutex<HashMap<String, String>>,
    }

    impl MapResolver {
        fn new(entries: impl IntoIterator<Item = (&'static str, &'static str)>) -> Self {
            Self {
                entries: Mutex::new(
                    entries
                        .into_iter()
                        .map(|(k, v)| (k.to_owned(), v.to_owned()))
                        .collect(),
                ),
            }
        }
    }

    impl SecretResolver for MapResolver {
        fn resolve(&self, path: &str) -> Result<SecretString, AliasResolverError> {
            let map = self.entries.lock().expect("MapResolver mutex poisoned");
            match map.get(path) {
                Some(v) => Ok(SecretString::from(v.clone())),
                None => Err(AliasResolverError::NotFound {
                    path: path.to_owned(),
                }),
            }
        }
    }

    #[test]
    fn resolver_trait_works_through_dyn_box() {
        let resolver: Box<dyn SecretResolver> = Box::new(MapResolver::new([(
            "team/gitlab/token-deploy",
            "ghp_fixture",
        )]));
        let value = resolver.resolve("team/gitlab/token-deploy").unwrap();
        assert_eq!(value.expose_secret(), "ghp_fixture");
    }

    #[test]
    fn resolver_returns_not_found_for_missing_path() {
        let resolver = MapResolver::new([("a/b/c", "v")]);
        let err = resolver.resolve("does/not/exist").unwrap_err();
        match err {
            AliasResolverError::NotFound { path } => assert_eq!(path, "does/not/exist"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    // -- Round-trip preservation --------------------------------

    /// Smoke test: a plain `String`-typed config field with an
    /// `@secret:` value round-trips through TOML serde without
    /// being magic-converted. This pins ADR-020's "TOML on disk
    /// holds the alias, never the value" contract — the config
    /// loader must NOT eagerly resolve aliases at deserialize.
    #[test]
    fn roundtrip_preserves_alias_in_string_field() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Cfg {
            token: String,
        }
        let original = Cfg {
            token: "@secret:team/gitlab/token-deploy".to_owned(),
        };
        let toml_text = toml::to_string(&original).unwrap();
        assert!(toml_text.contains("@secret:team/gitlab/token-deploy"));
        let back: Cfg = toml::from_str(&toml_text).unwrap();
        assert_eq!(back, original);
        assert_eq!(back.token, "@secret:team/gitlab/token-deploy");
    }

    #[test]
    fn roundtrip_preserves_alias_in_optional_field() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct Cfg {
            token: Option<String>,
        }
        let original = Cfg {
            token: Some("@secret:personal/github/pat".to_owned()),
        };
        let toml_text = toml::to_string(&original).unwrap();
        let back: Cfg = toml::from_str(&toml_text).unwrap();
        assert_eq!(back.token.as_deref(), Some("@secret:personal/github/pat"));
    }

    // -- Display of the error variants --------------------------

    #[test]
    fn error_display_includes_path_for_not_found() {
        let e = AliasResolverError::NotFound {
            path: "team/x/y".into(),
        };
        let s = format!("{e}");
        assert!(s.contains("team/x/y"));
    }

    #[test]
    fn error_display_includes_reason_for_bad_path() {
        let e = AliasResolverError::BadPath {
            path: "BAD".into(),
            reason: "uppercase letter".into(),
        };
        let s = format!("{e}");
        assert!(s.contains("BAD"));
        assert!(s.contains("uppercase letter"));
    }

    #[test]
    fn error_display_includes_backend_message() {
        let e = AliasResolverError::Backend {
            path: "team/x/y".into(),
            message: "vault unsealed but token expired".into(),
        };
        let s = format!("{e}");
        assert!(s.contains("vault unsealed but token expired"));
    }
}
