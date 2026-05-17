//! Source-credential recursion check per [ADR-021] §4.
//!
//! A source `A` that declares
//! [`SecretSource::requires_credential`](crate::source::SecretSource::requires_credential)
//! `= Some(CredentialRef::Path(...))` must have its credential
//! resolved through a source `B` whose `requires_credential()` is
//! `None`. The router enforces this at *configuration load*, before
//! any `get()` is dispatched, so the user gets a structured error
//! rather than a runtime stack-overflow on the first secret read.
//!
//! ## Why one hop?
//!
//! The reasoning from ADR-021 §4: a Vault token cannot itself be
//! stored in Vault, because reading it would require Vault to
//! already be unlockable. The keychain (and the local-vault from
//! ADR-023, once unlocked) are the only sources that may hold
//! source-credentials, because they have no `requires_credential()`
//! of their own.
//!
//! Anything deeper than one hop is either a misconfiguration the
//! user did not realise, or a literal cycle. Both fail the load
//! with a typed error from this module.
//!
//! ## What the validator checks
//!
//! For every source `A` in [`RouterConfig::sources`] whose
//! `requires_credential()` is `Some(CredentialRef::Path(p))`:
//!
//! 1. `p` must live under the reserved `__sources/` namespace.
//!    Anything else is rejected with [`CredentialGraphError::BadCredentialPath`]
//!    so users can't accidentally route source-credentials through
//!    their normal manifest.
//! 2. `p` must resolve through the configured router rules to some
//!    source `B`.
//! 3. Walk the chain `A → B → C → …`. The first node whose
//!    `requires_credential()` is `None` (or
//!    `Some(CredentialRef::Sentinel)` — a sentinel means the
//!    source handles its own auth and is treated as terminal)
//!    closes the chain.
//! 4. If we revisit a node, the chain is a cycle —
//!    [`CredentialGraphError::Cycle`].
//! 5. Otherwise, if the chain is longer than one hop —
//!    [`CredentialGraphError::Deep`].
//!
//! `Sentinel`-typed credentials are *not* graph edges; the source
//! plugin interprets them natively (`biometric`,
//! `default-profile`). They terminate the walk with no traversal.
//!
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md
//! [`RouterConfig::sources`]: crate::router_config::RouterConfig::sources

use thiserror::Error;

use crate::router_config::RouterConfig;
use crate::router_resolve::{PathResolver, ResolveError};
use crate::secret_path::SecretPath;
use crate::source::CredentialRef;

/// Reserved prefix for source-authentication credential paths.
/// Anything outside this namespace is rejected as a credential
/// path; per ADR-021 §5 only `__sources/<source>/<profile>` paths
/// may carry source credentials.
pub const SOURCE_CREDENTIALS_PREFIX: &str = "__sources/";

// =============================================================================
// Errors
// =============================================================================

/// Failure modes for [`validate_source_credentials`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CredentialGraphError {
    /// A source declared a credential at `path`, but `path` is not
    /// under the reserved [`SOURCE_CREDENTIALS_PREFIX`]. Per
    /// ADR-021 §5: source credentials must live under `__sources/`.
    //
    // `source_name` is intentionally not named `source` — thiserror
    // would interpret a field named `source` as the `#[source]`
    // chain root (cf. `UnroutableCredential::source_error`).
    #[error(
        "source '{source_name}' declares its credential at '{path}', but credential paths must live under `{SOURCE_CREDENTIALS_PREFIX}`"
    )]
    BadCredentialPath {
        /// The source whose `requires_credential()` returned this
        /// path.
        source_name: String,
        /// The offending path.
        path: String,
    },

    /// The credential path failed to resolve through the router.
    /// Usually means the user did not configure routing for
    /// `__sources/<source>/...` and there is no `[default]`.
    #[error("source '{source_name}' credential at '{path}' is unroutable: {source_error}")]
    UnroutableCredential {
        /// The source whose credential is unroutable.
        source_name: String,
        /// The credential path.
        path: String,
        /// Underlying resolver error.
        #[source]
        source_error: ResolveError,
    },

    /// `E_SOURCE_CREDENTIAL_CYCLE` — the credential graph contains
    /// a cycle. Reading any secret would loop forever.
    #[error(
        "source credential cycle detected: {}",
        chain.join(" -> ")
    )]
    Cycle {
        /// Source names visited, in order, ending with the source
        /// that closed the cycle.
        chain: Vec<String>,
    },

    /// `E_SOURCE_CREDENTIAL_DEEP` — the credential chain is longer
    /// than one hop. Per ADR-021 §4, only one level of indirection
    /// is allowed (Vault tokens live in keychain, not in another
    /// Vault).
    #[error(
        "source credential chain too deep (>1 hop): {}",
        chain.join(" -> ")
    )]
    Deep {
        /// Source names visited, in order. The first is the source
        /// whose credential is mis-routed; the rest are downstream
        /// dependencies.
        chain: Vec<String>,
    },
}

// =============================================================================
// Validator
// =============================================================================

/// Validate the source-credential graph defined by `config` and the
/// `requires_credential` lookup for each defined source.
///
/// `requires_credential` maps a source name to its
/// [`SecretSource::requires_credential`](crate::source::SecretSource::requires_credential)
/// return value. The router constructs sources first, then passes
/// `|name| sources[name].requires_credential()` here. Tests stub
/// the function with a static map.
///
/// Returns `Ok(())` when every source either has no credential, is
/// satisfied by a sentinel, or routes through exactly one
/// credential-free source. Otherwise returns the appropriate
/// [`CredentialGraphError`].
pub fn validate_source_credentials<F>(
    config: &RouterConfig,
    mut requires_credential: F,
) -> Result<(), CredentialGraphError>
where
    F: FnMut(&str) -> Option<CredentialRef>,
{
    let resolver = PathResolver::new(config);

    for src in &config.sources {
        let name = &src.name;
        let cred = requires_credential(name);
        match cred {
            None => continue,
            Some(CredentialRef::Sentinel(_)) => continue,
            Some(CredentialRef::Path(p)) => {
                ensure_credential_path_namespace(name, &p)?;
                walk_chain(name, &p, &resolver, &mut requires_credential)?;
            }
        }
    }

    Ok(())
}

fn ensure_credential_path_namespace(
    source_name: &str,
    path: &SecretPath,
) -> Result<(), CredentialGraphError> {
    if !path.as_str().starts_with(SOURCE_CREDENTIALS_PREFIX) {
        return Err(CredentialGraphError::BadCredentialPath {
            source_name: source_name.to_owned(),
            path: path.to_string(),
        });
    }
    Ok(())
}

/// Walk the chain starting from `start` whose credential lives at
/// `cred_path`. The chain length is counted in **edges** — a valid
/// one-hop chain has length 1.
fn walk_chain<F>(
    start: &str,
    cred_path: &SecretPath,
    resolver: &PathResolver<'_>,
    requires_credential: &mut F,
) -> Result<(), CredentialGraphError>
where
    F: FnMut(&str) -> Option<CredentialRef>,
{
    // `chain` records every source name we've visited so we can
    // (a) detect cycles and (b) include a useful trace in error
    // messages.
    let mut chain: Vec<String> = vec![start.to_owned()];
    let mut current_path = cred_path.clone();
    let mut hop = 0usize;

    loop {
        let decision = resolver.resolve(&current_path).map_err(|e| {
            CredentialGraphError::UnroutableCredential {
                source_name: start.to_owned(),
                path: current_path.to_string(),
                source_error: e,
            }
        })?;
        let next = decision.source().to_owned();
        hop += 1;

        // Cycle: revisiting a source we already saw — including
        // `start` itself if hop == 1 closes back to it.
        if chain.iter().any(|s| s == &next) {
            chain.push(next);
            return Err(CredentialGraphError::Cycle { chain });
        }
        chain.push(next.clone());

        let next_cred = requires_credential(&next);
        match next_cred {
            None | Some(CredentialRef::Sentinel(_)) => {
                // Terminal. Valid only if hop == 1; otherwise the
                // chain went too deep before closing.
                if hop > 1 {
                    return Err(CredentialGraphError::Deep { chain });
                }
                return Ok(());
            }
            Some(CredentialRef::Path(p)) => {
                if hop >= 1 {
                    // We already consumed the one allowed hop and
                    // the next node still needs a credential —
                    // either we eventually cycle back (caught on
                    // the next iteration) or we walk forever
                    // through credential-bearing sources, which is
                    // DEEP. Continue the walk so the loop can tell
                    // them apart.
                    ensure_credential_path_namespace(&next, &p)?;
                    current_path = p;
                    continue;
                }
            }
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router_config::RouterConfig;
    use std::collections::HashMap;

    fn cfg(toml: &str) -> RouterConfig {
        RouterConfig::parse(toml).expect("fixture config must parse")
    }

    fn p_internal(s: &str) -> SecretPath {
        SecretPath::parse_internal(s).expect("internal path must parse")
    }

    /// Convenience — build a `requires_credential` function over a
    /// static map. `None` entries mean credential-free.
    fn req_map(
        m: HashMap<String, Option<CredentialRef>>,
    ) -> impl FnMut(&str) -> Option<CredentialRef> {
        move |name| m.get(name).cloned().unwrap_or(None)
    }

    // -- 1) Valid one-hop chain -----------------------------------

    #[test]
    fn one_hop_chain_through_keychain_is_valid() {
        // vault-team needs a credential; it routes to keychain
        // (credential-free) via a `[[route]]` for the `__sources/`
        // namespace.
        let c = cfg(r#"
            [[source]]
            name = "vault-team"
            type = "vault"

            [[source]]
            name = "keychain"
            type = "keychain"

            [[route]]
            prefix = "__sources/"
            source = "keychain"
            "#);
        let req = req_map(
            [
                (
                    "vault-team".to_owned(),
                    Some(CredentialRef::Path(p_internal(
                        "__sources/vault-team/deploy",
                    ))),
                ),
                ("keychain".to_owned(), None),
            ]
            .into_iter()
            .collect(),
        );
        validate_source_credentials(&c, req).expect("one-hop chain should be valid");
    }

    // -- 2) Self-cycle -------------------------------------------

    #[test]
    fn self_loop_is_a_cycle() {
        // vault-team requires a credential whose path resolves
        // back to vault-team itself (via [secret] override).
        let c = cfg(r#"
            [[source]]
            name = "vault-team"
            type = "vault"

            [secret."__sources/vault-team/deploy"]
            source = "vault-team"
            reference = "secret/data/__sources/vault-team/deploy"
            "#);
        let req = req_map(
            [(
                "vault-team".to_owned(),
                Some(CredentialRef::Path(p_internal(
                    "__sources/vault-team/deploy",
                ))),
            )]
            .into_iter()
            .collect(),
        );
        let err = validate_source_credentials(&c, req).unwrap_err();
        match err {
            CredentialGraphError::Cycle { chain } => {
                assert_eq!(
                    chain,
                    vec!["vault-team".to_owned(), "vault-team".to_owned()]
                );
            }
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    // -- 3) Two-node cycle ---------------------------------------

    #[test]
    fn two_node_cycle_detected() {
        // A → B (via [secret] override of __sources/A/...)
        // B → A (via [secret] override of __sources/B/...)
        let c = cfg(r#"
            [[source]]
            name = "vault-a"
            type = "vault"

            [[source]]
            name = "vault-b"
            type = "vault"

            [secret."__sources/vault-a/x"]
            source = "vault-b"
            reference = "secret/a/x"

            [secret."__sources/vault-b/x"]
            source = "vault-a"
            reference = "secret/b/x"
            "#);
        let req = req_map(
            [
                (
                    "vault-a".to_owned(),
                    Some(CredentialRef::Path(p_internal("__sources/vault-a/x"))),
                ),
                (
                    "vault-b".to_owned(),
                    Some(CredentialRef::Path(p_internal("__sources/vault-b/x"))),
                ),
            ]
            .into_iter()
            .collect(),
        );
        let err = validate_source_credentials(&c, req).unwrap_err();
        match err {
            CredentialGraphError::Cycle { chain } => {
                // Either A->B->A or B->A->B depending on iteration
                // order over config.sources (Vec, declaration
                // order). With the config above we hit vault-a
                // first.
                assert_eq!(chain.first().unwrap(), "vault-a");
                assert_eq!(chain.last().unwrap(), "vault-a");
                assert!(chain.iter().any(|n| n == "vault-b"));
            }
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    // -- 4) Deep chain (no cycle) --------------------------------

    #[test]
    fn three_hop_chain_without_cycle_is_deep() {
        // A requires __sources/A/x → routes to B (B requires
        // __sources/B/y → routes to C (C credential-free)).
        let c = cfg(r#"
            [[source]]
            name = "vault-a"
            type = "vault"

            [[source]]
            name = "vault-b"
            type = "vault"

            [[source]]
            name = "keychain"
            type = "keychain"

            [secret."__sources/vault-a/x"]
            source = "vault-b"
            reference = "x"

            [secret."__sources/vault-b/y"]
            source = "keychain"
            reference = "y"
            "#);
        let req = req_map(
            [
                (
                    "vault-a".to_owned(),
                    Some(CredentialRef::Path(p_internal("__sources/vault-a/x"))),
                ),
                (
                    "vault-b".to_owned(),
                    Some(CredentialRef::Path(p_internal("__sources/vault-b/y"))),
                ),
                ("keychain".to_owned(), None),
            ]
            .into_iter()
            .collect(),
        );
        let err = validate_source_credentials(&c, req).unwrap_err();
        match err {
            CredentialGraphError::Deep { chain } => {
                assert_eq!(
                    chain,
                    vec![
                        "vault-a".to_owned(),
                        "vault-b".to_owned(),
                        "keychain".to_owned()
                    ]
                );
            }
            other => panic!("expected Deep, got {other:?}"),
        }
    }

    // -- 5) Nothing to check --------------------------------------

    #[test]
    fn no_source_with_credential_is_ok() {
        let c = cfg(r#"
            [[source]]
            name = "keychain"
            type = "keychain"

            [[source]]
            name = "env-store"
            type = "env-store"
            "#);
        let req = req_map(HashMap::new());
        validate_source_credentials(&c, req).unwrap();
    }

    // -- 6) Sentinel terminates without traversal -----------------

    #[test]
    fn sentinel_credential_is_terminal() {
        // 1Password's biometric session is a Sentinel — the source
        // handles its own auth, so the validator should not try to
        // route the credential.
        let c = cfg(r#"
            [[source]]
            name = "1p-personal"
            type = "1password"
            "#);
        let req = req_map(
            [(
                "1p-personal".to_owned(),
                Some(CredentialRef::Sentinel("biometric".to_owned())),
            )]
            .into_iter()
            .collect(),
        );
        validate_source_credentials(&c, req).unwrap();
    }

    // -- 7) Path outside __sources/ -------------------------------

    #[test]
    fn credential_path_outside_internal_namespace_rejected() {
        // ADR-020 path validation forbids `__*` outside
        // `parse_internal`, so we have to use a regular path here
        // (which is exactly the case we want to reject).
        let c = cfg(r#"
            [[source]]
            name = "vault-team"
            type = "vault"
            [[source]]
            name = "keychain"
            type = "keychain"

            [default]
            source = "keychain"
            "#);
        let req = req_map(
            [(
                "vault-team".to_owned(),
                Some(CredentialRef::Path(
                    SecretPath::parse("team/secret/token").unwrap(),
                )),
            )]
            .into_iter()
            .collect(),
        );
        let err = validate_source_credentials(&c, req).unwrap_err();
        match err {
            CredentialGraphError::BadCredentialPath { source_name, path } => {
                assert_eq!(source_name, "vault-team");
                assert_eq!(path, "team/secret/token");
            }
            other => panic!("expected BadCredentialPath, got {other:?}"),
        }
    }

    // -- 8) Unroutable credential ---------------------------------

    #[test]
    fn unroutable_credential_path_surfaces_resolve_error() {
        // No [default], no [[route]] for `__sources/`, no
        // [secret]. The path is unroutable.
        let c = cfg(r#"
            [[source]]
            name = "vault-team"
            type = "vault"
            [[source]]
            name = "keychain"
            type = "keychain"
            "#);
        let req = req_map(
            [(
                "vault-team".to_owned(),
                Some(CredentialRef::Path(p_internal(
                    "__sources/vault-team/deploy",
                ))),
            )]
            .into_iter()
            .collect(),
        );
        let err = validate_source_credentials(&c, req).unwrap_err();
        match err {
            CredentialGraphError::UnroutableCredential {
                source_name,
                path,
                source_error,
            } => {
                assert_eq!(source_name, "vault-team");
                assert_eq!(path, "__sources/vault-team/deploy");
                assert!(matches!(source_error, ResolveError::NoRoute { .. }));
            }
            other => panic!("expected UnroutableCredential, got {other:?}"),
        }
    }

    // -- 9) Default-routed __sources/ ------------------------------

    #[test]
    fn one_hop_chain_via_default_route_is_valid() {
        // No explicit __sources/ route; the default catches it.
        let c = cfg(r#"
            [[source]]
            name = "vault-team"
            type = "vault"

            [[source]]
            name = "keychain"
            type = "keychain"

            [default]
            source = "keychain"
            "#);
        let req = req_map(
            [
                (
                    "vault-team".to_owned(),
                    Some(CredentialRef::Path(p_internal(
                        "__sources/vault-team/deploy",
                    ))),
                ),
                ("keychain".to_owned(), None),
            ]
            .into_iter()
            .collect(),
        );
        validate_source_credentials(&c, req).unwrap();
    }
}
