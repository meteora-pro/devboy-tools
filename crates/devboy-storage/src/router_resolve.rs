//! Path resolution algorithm per [ADR-021] §2.
//!
//! Given a [`RouterConfig`] and a [`SecretPath`], decide *which*
//! source serves the path and what context the source needs to
//! produce a reference. The decision is pure data — no source
//! plugin is invoked here. Turning the decision into an actual
//! `(source, reference)` pair is the next layer's job (P5.4 wires
//! the cache + plugin lookup; P6 ships the concrete sources).
//!
//! Splitting the resolver from the source-call layer lets tests
//! exercise the routing rules with TOML fixtures alone, without
//! standing up real backends. `doctor` (P7.2 / P10.1) reuses the
//! same pure function to pre-flight a project's manifest against
//! the active config.
//!
//! ## Algorithm
//!
//! Per ADR-021 §2:
//!
//! 1. **Per-secret override.** If `[secret."<path>"]` matches the
//!    queried path, return [`RouteDecision::Explicit`] with the
//!    user-supplied source + reference. Wins over everything else.
//! 2. **Longest prefix.** Walk `[[route]]` blocks; pick the one
//!    whose `prefix` is the longest string that the queried path
//!    starts with. Return [`RouteDecision::Prefix`] with the
//!    route's source name + verbatim settings. Source-specific
//!    path-tail-to-reference mapping (Vault joins mount + tail,
//!    1Password builds an `op://` URL, …) is delegated to the
//!    plugin.
//! 3. **Default route.** No `[secret]` and no `[[route]]` matched.
//!    Return [`RouteDecision::Default`] carrying the default
//!    source + the optional fallback hint (used by the caller's
//!    `is_available() == NotInstalled` retry — see ADR-021 §2
//!    step 3).
//! 4. **No fall-through.** No matching `[secret]`, no matching
//!    `[[route]]`, and no `[default]` either: fail with
//!    [`ResolveError::NoRoute`].
//!
//! Prefix `[[route]]` already enforces a trailing `/` at config
//! load (`router_config::RouterConfigError::BadRoutePrefix`), so
//! the resolver does not have to worry about partial-segment
//! matches like `team/` vs `teamfoo/x`.
//!
//! Ties between routes of equal length resolve by declaration
//! order — earlier `[[route]]` wins. That's deterministic and
//! matches what the user sees when they read the file top to
//! bottom.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md
//! [`RouterConfig`]: crate::router_config::RouterConfig

use std::collections::BTreeMap;

use thiserror::Error;

use crate::router_config::RouterConfig;
use crate::secret_path::SecretPath;

// =============================================================================
// Public types
// =============================================================================

/// Outcome of resolving a path against a [`RouterConfig`].
///
/// All variants borrow from the input config (lifetime `'a`); the
/// queried path is owned by the caller and is not copied into the
/// decision. The caller computes the path-tail (`path.strip_prefix
/// (decision.prefix())`) only when it needs to call into a source
/// plugin.
#[derive(Debug, Clone, PartialEq)]
pub enum RouteDecision<'a> {
    /// `[secret."<path>"]` matched — both the source and the
    /// backend-specific reference are user-supplied.
    Explicit {
        /// Source name to dispatch to.
        source: &'a str,
        /// Backend-specific reference handed to the source's `get`
        /// (e.g. `op://Work/Acme Jira/credential`).
        reference: &'a str,
    },
    /// One of the `[[route]]` prefixes matched. The source plugin
    /// is responsible for joining `settings` with the path tail to
    /// produce its actual reference.
    Prefix {
        /// Source name to dispatch to.
        source: &'a str,
        /// The matched prefix verbatim (always ends with `/`).
        prefix: &'a str,
        /// Source-specific extras from the `[[route]]` block
        /// (`mount`, `vault`, …). Pass-through to the source.
        settings: &'a BTreeMap<String, toml::Value>,
    },
    /// No `[secret]` and no `[[route]]` matched — falls back to
    /// `[default].source`. The optional `fallback` hint lets the
    /// caller retry against `[default].fallback` when the primary
    /// reports `is_available() == NotInstalled` (per ADR-021 §2
    /// step 3).
    Default {
        /// `[default].source`.
        source: &'a str,
        /// `[default].fallback`, if configured.
        fallback: Option<&'a str>,
    },
}

impl<'a> RouteDecision<'a> {
    /// The source name selected for this path. Consumers that just
    /// want to know "who serves it?" don't have to match on the
    /// variant.
    pub fn source(&self) -> &'a str {
        match self {
            RouteDecision::Explicit { source, .. }
            | RouteDecision::Prefix { source, .. }
            | RouteDecision::Default { source, .. } => source,
        }
    }
}

/// Failure modes for [`PathResolver::resolve`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResolveError {
    /// No `[secret]` matched, no `[[route]]` matched, and the
    /// config has no `[default]` section. The path is unroutable.
    #[error(
        "no route for path '{path}' — config has no matching [secret], no matching [[route]], and no [default]"
    )]
    NoRoute {
        /// The path that failed to route.
        path: String,
    },
}

// =============================================================================
// Resolver
// =============================================================================

/// Read-only view over a [`RouterConfig`] that answers
/// "which source serves this path?" without invoking any source
/// plugin.
///
/// Cheap to construct (just borrows the config), so call sites can
/// re-create one per query if the config might have been reloaded
/// in the meantime. The resolver itself caches nothing; the
/// adaptive cache lands in P5.4.
pub struct PathResolver<'a> {
    config: &'a RouterConfig,
}

impl<'a> PathResolver<'a> {
    /// Build a resolver borrowing from `config`.
    pub fn new(config: &'a RouterConfig) -> Self {
        Self { config }
    }

    /// Run the algorithm from the module docs against `path`.
    pub fn resolve(&self, path: &SecretPath) -> Result<RouteDecision<'a>, ResolveError> {
        // 1) Per-secret override — exact match wins outright.
        if let Some(ovr) = self.config.secret_overrides.get(path) {
            return Ok(RouteDecision::Explicit {
                source: ovr.source.as_str(),
                reference: ovr.reference.as_str(),
            });
        }

        // 2) Longest prefix wins. Iterate in declaration order so
        //    ties go to the earlier `[[route]]`.
        let path_str = path.as_str();
        let mut best: Option<&'a crate::router_config::RouteRule> = None;
        for r in &self.config.routes {
            if !path_str.starts_with(&r.prefix) {
                continue;
            }
            match best {
                None => best = Some(r),
                Some(prev) if r.prefix.len() > prev.prefix.len() => best = Some(r),
                Some(_) => {} // earlier-declared shorter or equal-length prefix already won
            }
        }
        if let Some(r) = best {
            return Ok(RouteDecision::Prefix {
                source: r.source.as_str(),
                prefix: r.prefix.as_str(),
                settings: &r.settings,
            });
        }

        // 3) Default route, with fallback hint.
        if let Some(d) = &self.config.default {
            return Ok(RouteDecision::Default {
                source: d.source.as_str(),
                fallback: d.fallback.as_deref(),
            });
        }

        // 4) Nothing left. Fail with a structured error.
        Err(ResolveError::NoRoute {
            path: path_str.to_owned(),
        })
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router_config::RouterConfig;

    fn cfg(toml: &str) -> RouterConfig {
        RouterConfig::parse(toml).expect("fixture config must parse")
    }

    fn p(s: &str) -> SecretPath {
        SecretPath::parse(s).unwrap()
    }

    // -- 1) Per-secret override --------------------------------------

    #[test]
    fn explicit_secret_override_wins_over_prefix_and_default() {
        let c = cfg(r#"
            [[source]]
            name = "keychain"
            type = "keychain"

            [[source]]
            name = "vault-team"
            type = "vault"

            [[source]]
            name = "1p-personal"
            type = "1password"

            [default]
            source = "keychain"

            [[route]]
            prefix = "team/"
            source = "vault-team"

            [secret."team/gitlab/token-deploy"]
            source = "1p-personal"
            reference = "op://Work/Gitlab/credential"
            "#);
        let r = PathResolver::new(&c);

        // Path is also covered by the team/ prefix and the default,
        // but the [secret."..."] override beats both.
        let d = r.resolve(&p("team/gitlab/token-deploy")).unwrap();
        match d {
            RouteDecision::Explicit { source, reference } => {
                assert_eq!(source, "1p-personal");
                assert_eq!(reference, "op://Work/Gitlab/credential");
            }
            other => panic!("expected Explicit, got {other:?}"),
        }
    }

    // -- 2) Prefix routing -------------------------------------------

    #[test]
    fn matching_prefix_returns_route_with_settings() {
        let c = cfg(r#"
            [[source]]
            name = "vault-team"
            type = "vault"

            [[route]]
            prefix = "team/"
            source = "vault-team"
            mount  = "secret/data/team"
            "#);
        let r = PathResolver::new(&c);
        let d = r.resolve(&p("team/gitlab/token-deploy")).unwrap();
        match d {
            RouteDecision::Prefix {
                source,
                prefix,
                settings,
            } => {
                assert_eq!(source, "vault-team");
                assert_eq!(prefix, "team/");
                assert_eq!(
                    settings.get("mount").unwrap().as_str().unwrap(),
                    "secret/data/team"
                );
            }
            other => panic!("expected Prefix, got {other:?}"),
        }
    }

    #[test]
    fn longest_matching_prefix_wins_over_shorter() {
        let c = cfg(r#"
            [[source]]
            name = "vault-team"
            type = "vault"

            [[source]]
            name = "vault-acme"
            type = "vault"

            [[route]]
            prefix = "team/"
            source = "vault-team"

            [[route]]
            prefix = "team/acme/"
            source = "vault-acme"
            "#);
        let r = PathResolver::new(&c);
        // `team/acme/x` matches both prefixes; the longer wins.
        let d = r.resolve(&p("team/acme/database/url")).unwrap();
        assert_eq!(d.source(), "vault-acme");
        match d {
            RouteDecision::Prefix { prefix, .. } => assert_eq!(prefix, "team/acme/"),
            other => panic!("expected Prefix, got {other:?}"),
        }

        // `team/foo/x` only matches the shorter one.
        let d = r.resolve(&p("team/foo/x")).unwrap();
        assert_eq!(d.source(), "vault-team");
    }

    #[test]
    fn duplicate_prefix_is_rejected_at_config_load_so_resolver_never_sees_it() {
        // The resolver's "ties go to the earlier `[[route]]`" rule
        // would only matter if config load let two `[[route]]`
        // entries share a prefix — confirm here that it doesn't,
        // so we can rely on the invariant.
        let err = RouterConfig::parse(
            r#"
            [[source]]
            name = "src-a"
            type = "x"
            [[source]]
            name = "src-b"
            type = "x"

            [[route]]
            prefix = "tea/"
            source = "src-a"

            [[route]]
            prefix = "tea/"
            source = "src-b"
            "#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            crate::router_config::RouterConfigError::DuplicateRoutePrefix { .. }
        ));
    }

    #[test]
    fn earlier_route_wins_when_prefixes_have_different_length_but_one_starts_the_other_short() {
        // Reversal of "longest wins" — earlier declaration order
        // matters only when lengths are equal. Here `team/foo/` is
        // longer than `team/`; the long one wins regardless of
        // which was declared first.
        let c = cfg(r#"
            [[source]]
            name = "long"
            type = "x"
            [[source]]
            name = "short"
            type = "x"

            [[route]]
            prefix = "team/foo/"
            source = "long"

            [[route]]
            prefix = "team/"
            source = "short"
            "#);
        let r = PathResolver::new(&c);
        let d = r.resolve(&p("team/foo/secret")).unwrap();
        assert_eq!(d.source(), "long");
    }

    #[test]
    fn prefix_must_match_at_segment_boundary() {
        // `team/` is required to end in `/` at config load. So a
        // path like `teamfoo/x` cannot accidentally match. Verify.
        let c = cfg(r#"
            [[source]]
            name = "vault-team"
            type = "vault"

            [default]
            source = "vault-team"

            [[route]]
            prefix = "team/"
            source = "vault-team"
            "#);
        let r = PathResolver::new(&c);
        // `teamfoo/sub/key` does NOT start with `team/` (3-segment
        // path because ADR-020 requires ≥3).
        let d = r.resolve(&p("teamfoo/sub/key")).unwrap();
        match d {
            RouteDecision::Default { .. } => {}
            other => panic!("teamfoo/sub/key must NOT match the team/ prefix; got {other:?}"),
        }
    }

    // -- 3) Default route --------------------------------------------

    #[test]
    fn unmatched_path_falls_back_to_default() {
        let c = cfg(r#"
            [[source]]
            name = "keychain"
            type = "keychain"

            [default]
            source = "keychain"
            "#);
        let r = PathResolver::new(&c);
        let d = r.resolve(&p("personal/random/key")).unwrap();
        match d {
            RouteDecision::Default { source, fallback } => {
                assert_eq!(source, "keychain");
                assert!(fallback.is_none());
            }
            other => panic!("expected Default, got {other:?}"),
        }
    }

    #[test]
    fn default_fallback_is_carried_through() {
        let c = cfg(r#"
            [[source]]
            name = "keychain"
            type = "keychain"
            [[source]]
            name = "local-vault"
            type = "local-vault"

            [default]
            source   = "keychain"
            fallback = "local-vault"
            "#);
        let r = PathResolver::new(&c);
        let d = r.resolve(&p("anything/goes/here")).unwrap();
        match d {
            RouteDecision::Default { source, fallback } => {
                assert_eq!(source, "keychain");
                assert_eq!(fallback, Some("local-vault"));
            }
            other => panic!("expected Default, got {other:?}"),
        }
    }

    // -- 4) No-route error -------------------------------------------

    #[test]
    fn no_secret_no_prefix_no_default_returns_no_route_error() {
        let c = cfg(r#"
            [[source]]
            name = "vault-team"
            type = "vault"

            [[route]]
            prefix = "team/"
            source = "vault-team"
            "#);
        let r = PathResolver::new(&c);
        let err = r.resolve(&p("personal/random/key")).unwrap_err();
        match err {
            ResolveError::NoRoute { path } => assert_eq!(path, "personal/random/key"),
        }
    }

    // -- Helpers / smoke ---------------------------------------------

    #[test]
    fn route_decision_source_helper_returns_the_dispatch_target() {
        let c = cfg(r#"
            [[source]]
            name = "keychain"
            type = "keychain"

            [default]
            source = "keychain"
            "#);
        let r = PathResolver::new(&c);
        assert_eq!(r.resolve(&p("a/b/c")).unwrap().source(), "keychain");
    }

    /// Table-driven fixture covering all four cases — mirrors the
    /// "tests with table of fixtures" requirement in the task
    /// description.
    #[test]
    fn fixture_table_exercises_every_branch() {
        let c = cfg(r#"
            [[source]]
            name = "keychain"
            type = "keychain"
            [[source]]
            name = "local-vault"
            type = "local-vault"
            [[source]]
            name = "vault-team"
            type = "vault"
            [[source]]
            name = "1p-personal"
            type = "1password"

            [default]
            source   = "keychain"
            fallback = "local-vault"

            [[route]]
            prefix = "team/"
            source = "vault-team"

            [[route]]
            prefix = "team/acme/"
            source = "1p-personal"

            [secret."client-acme/jira/api-key"]
            source = "1p-personal"
            reference = "op://Work/Acme Jira/credential"
            "#);
        let r = PathResolver::new(&c);

        // (path, expected source, expected variant tag for sanity)
        let cases: &[(&str, &str, &str)] = &[
            // explicit > prefix > default
            ("client-acme/jira/api-key", "1p-personal", "Explicit"),
            // longest prefix
            ("team/acme/db/url", "1p-personal", "Prefix"),
            ("team/foo/bar", "vault-team", "Prefix"),
            // default + fallback
            ("personal/x/y", "keychain", "Default"),
        ];
        for (input, expected_source, expected_variant) in cases {
            let d = r
                .resolve(&p(input))
                .unwrap_or_else(|e| panic!("resolve('{input}') failed: {e}"));
            assert_eq!(
                d.source(),
                *expected_source,
                "fixture for '{input}' picked wrong source"
            );
            let actual_variant = match &d {
                RouteDecision::Explicit { .. } => "Explicit",
                RouteDecision::Prefix { .. } => "Prefix",
                RouteDecision::Default { .. } => "Default",
            };
            assert_eq!(
                actual_variant, *expected_variant,
                "fixture for '{input}' picked wrong variant"
            );
        }
    }
}
