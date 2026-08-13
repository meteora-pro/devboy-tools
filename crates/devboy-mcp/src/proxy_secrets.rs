//! `@secret:<path>` header-rewriting for the MCP proxy per
//! [ADR-020] §5 third bullet:
//!
//! > **HTTP through the local proxy.** The MCP proxy already
//! > mediates outgoing HTTP for some flows. When it sees an
//! > outgoing `Authorization: Bearer @secret:<path>` (or another
//! > whitelisted header pattern), it rewrites the value to the
//! > resolved secret before forwarding. The agent that constructed
//! > the request never saw the value; the request as logged
//! > through transcript shows the alias.
//!
//! This module is the planning + rewrite step. It operates on a
//! [`reqwest::header::HeaderMap`] and a [`SecretResolver`]. The
//! returned [`RewriteOutcome`] carries the *original* aliases per
//! header so the proxy's transcript layer can log what the agent
//! saw, not what the upstream saw.
//!
//! ## Recognised patterns
//!
//! For each header in [`WHITELISTED_HEADERS`] (matched
//! case-insensitively):
//!
//! - `Bearer @secret:<path>` → resolve, rewrite to
//!   `Bearer <value>`. Most common shape; covers Authorization
//!   for OAuth bearer flows.
//! - `Token @secret:<path>` → resolve, rewrite to
//!   `Token <value>`. GitLab's `PRIVATE-TOKEN` is sometimes
//!   wrapped this way.
//! - `@secret:<path>` (whole header value) → resolve, rewrite
//!   to the bare value. Covers `X-API-Key`, `X-Vault-Token`,
//!   etc.
//!
//! Headers outside the whitelist are *not* inspected — even if
//! they contain a valid-looking alias. This is by design: the
//! whitelist is the security boundary, not the alias syntax.
//!
//! ## Nothing calls this yet, and nothing can
//!
//! Stated at the top rather than buried, because a security helper
//! that looks live and is not is worse than one that is absent.
//!
//! No shipped tool accepts HTTP headers from an agent. The proxy
//! builds its own `Authorization` from a token the CLI resolved
//! before connecting, so no `@secret:` alias ever reaches a header
//! for this code to rewrite. There is no missing wire here — there is
//! a missing *consumer*, and inventing one (a tool that forwards
//! caller-supplied headers) is a feature decision with its own risks,
//! not a loose end to tidy.
//!
//! What that means in practice:
//!
//! - Do not count this bullet of [ADR-020] §5 when reasoning about
//!   what an agent can currently reach. The ADR says so too.
//! - The day a tool does accept caller-supplied headers, it must
//!   route through [`rewrite_outgoing_headers`] rather than around
//!   it. That is the whole reason this is kept rather than deleted:
//!   the rule is written down and tested, so the future tool inherits
//!   it instead of re-deciding it.
//!
//! ## Also not done here
//!
//! - Resolving aliases against a router. The
//!   [`SecretResolver`] trait is the boundary; tests pass a
//!   `HashMap`-backed mock.
//!
//! [ADR-020]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md
//! [`SecretResolver`]: devboy_core::alias::SecretResolver

use devboy_core::alias::{ALIAS_PREFIX, AliasResolverError, SecretResolver, parse_alias};
use devboy_core::secret_approval::ApprovalGatedResolver;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, InvalidHeaderValue};
use secrecy::ExposeSecret;
use thiserror::Error;
use tracing::debug;

/// Header names the rewriter inspects. Case-insensitive match.
/// Adding a new header here is a deliberate widening of the
/// rewrite surface.
pub const WHITELISTED_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "x-vault-token",
    "x-auth-token",
    "private-token",
    "x-private-token",
];

/// Recognised auth schemes that prefix the alias.
const RECOGNISED_SCHEMES: &[&str] = &["Bearer", "Token"];

// =============================================================================
// Errors
// =============================================================================

/// Failure modes for [`rewrite_outgoing_headers`].
#[derive(Debug, Error)]
pub enum ProxyHeaderError {
    /// Resolver failed to produce a value for an alias.
    #[error("failed to resolve alias `@secret:{path}` for header `{header}`: {source_error}")]
    Resolve {
        /// Header whose value held the alias.
        header: String,
        /// Path portion of the alias.
        path: String,
        /// Underlying resolver error.
        #[source]
        source_error: AliasResolverError,
    },

    /// Resolved value contains bytes that aren't valid in an HTTP
    /// header (e.g. a newline injected via a malicious upstream).
    /// Surfaces as `Err` rather than silently truncating because
    /// the alternative is a request-smuggling vector.
    #[error(
        "resolved value for header `{header}` contains characters that are not valid in an HTTP header"
    )]
    InvalidHeaderValue {
        /// Header that failed to validate.
        header: String,
        /// Underlying reqwest error.
        #[source]
        source_error: InvalidHeaderValue,
    },
}

// =============================================================================
// Rewrite outcome (transcript-friendly)
// =============================================================================

/// Audit entry for one rewritten header. The `original_alias`
/// portion is what the agent / transcript log will display; the
/// resolved value is in the [`HeaderMap`] the rewriter mutated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderRewrite {
    /// Header name (lowercase, as `HeaderName::as_str` returns).
    pub header_name: String,
    /// Path portion of the original `@secret:<path>` alias.
    pub original_alias: String,
    /// Auth scheme prefix, when present (`"Bearer"`, `"Token"`).
    /// `None` for whole-value aliases (e.g. `X-API-Key`).
    pub scheme: Option<String>,
}

impl HeaderRewrite {
    /// Reproduce the alias the agent originally constructed —
    /// useful for transcript logs that want to show
    /// `Authorization: Bearer @secret:team/x/y` rather than the
    /// resolved value.
    pub fn transcript_value(&self) -> String {
        match &self.scheme {
            Some(scheme) => format!("{scheme} {ALIAS_PREFIX}{}", self.original_alias),
            None => format!("{ALIAS_PREFIX}{}", self.original_alias),
        }
    }
}

/// What [`rewrite_outgoing_headers`] did. Empty `rewrites` means
/// the headers were a pass-through.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RewriteOutcome {
    /// One entry per header that was rewritten, in iteration
    /// order over the whitelist.
    pub rewrites: Vec<HeaderRewrite>,
}

impl RewriteOutcome {
    /// `true` when at least one header was rewritten.
    pub fn changed(&self) -> bool {
        !self.rewrites.is_empty()
    }
}

// =============================================================================
// Rewriter
// =============================================================================

/// Walk `headers`, find aliased values in [`WHITELISTED_HEADERS`],
/// resolve via an [`ApprovalGatedResolver`], and replace each
/// value with the resolved form. Returns a [`RewriteOutcome`]
/// capturing what changed for transcript logging.
///
/// The gated resolver enforces the approve-on-use protocol
/// (`approve_on_use = session | per-call`) before dispatching
/// to the underlying source. A `PromptRequired` gate surfaces
/// as [`ProxyHeaderError::Resolve`] so the proxy can return
/// the error to the agent and the orchestration layer can open
/// the use-approval dialog.
///
/// Non-whitelisted headers — and whitelisted headers that don't
/// match the recognised alias patterns — are left untouched.
pub fn rewrite_outgoing_headers<R, F>(
    headers: &mut HeaderMap,
    resolver: &ApprovalGatedResolver<R, F>,
) -> Result<RewriteOutcome, ProxyHeaderError>
where
    R: SecretResolver,
    F: Fn(&str) -> devboy_core::secret_approval::ApproveOnUsePolicy + Send + Sync,
{
    let mut outcome = RewriteOutcome::default();

    for whitelisted in WHITELISTED_HEADERS {
        let header_name = match HeaderName::from_bytes(whitelisted.as_bytes()) {
            Ok(n) => n,
            // Whitelisted constants are valid header names; this
            // branch is defensive and unreachable in practice.
            Err(_) => continue,
        };

        let Some(existing) = headers.get(&header_name) else {
            continue;
        };
        let Ok(value_str) = existing.to_str() else {
            // Non-ASCII / opaque header value — can't be an
            // `@secret:` alias, leave it.
            continue;
        };

        let Some(rewrite) = parse_aliased_value(value_str) else {
            continue;
        };

        let resolved =
            resolver
                .resolve(&rewrite.path)
                .map_err(|source_error| ProxyHeaderError::Resolve {
                    header: header_name.as_str().to_owned(),
                    path: rewrite.path.clone(),
                    source_error,
                })?;

        let new_value = match &rewrite.scheme {
            Some(scheme) => format!("{scheme} {}", resolved.expose_secret()),
            None => resolved.expose_secret().to_owned(),
        };
        let header_value = HeaderValue::from_str(&new_value).map_err(|source_error| {
            ProxyHeaderError::InvalidHeaderValue {
                header: header_name.as_str().to_owned(),
                source_error,
            }
        })?;
        // `insert` replaces — exactly what we want.
        headers.insert(&header_name, header_value);

        outcome.rewrites.push(HeaderRewrite {
            header_name: header_name.as_str().to_owned(),
            original_alias: rewrite.path,
            scheme: rewrite.scheme,
        });
    }

    if outcome.changed() {
        debug!(
            count = outcome.rewrites.len(),
            "proxy_secrets: rewrote outgoing headers"
        );
    }

    Ok(outcome)
}

// =============================================================================
// Pattern matching
// =============================================================================

/// Internal representation of a recognised alias inside a header.
struct ParsedAlias {
    path: String,
    scheme: Option<String>,
}

fn parse_aliased_value(raw: &str) -> Option<ParsedAlias> {
    let trimmed = raw.trim();

    // Whole-value alias: `@secret:team/x/y`.
    if let Some(path) = parse_alias(trimmed) {
        return Some(ParsedAlias {
            path: path.to_owned(),
            scheme: None,
        });
    }

    // Scheme-prefixed alias: `Bearer @secret:...` /
    // `Token @secret:...`.
    let (head, tail) = trimmed.split_once(' ')?;
    let scheme = RECOGNISED_SCHEMES
        .iter()
        .find(|s| s.eq_ignore_ascii_case(head))?;
    let path = parse_alias(tail.trim_start())?;
    Some(ParsedAlias {
        path: path.to_owned(),
        scheme: Some((*scheme).to_owned()),
    })
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use devboy_core::alias::AliasResolverError as AErr;
    use secrecy::SecretString;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Map-backed resolver for tests.
    struct MapResolver {
        entries: Mutex<HashMap<String, String>>,
    }

    impl MapResolver {
        fn new(pairs: impl IntoIterator<Item = (&'static str, &'static str)>) -> Self {
            Self {
                entries: Mutex::new(
                    pairs
                        .into_iter()
                        .map(|(k, v)| (k.to_owned(), v.to_owned()))
                        .collect(),
                ),
            }
        }
    }

    /// Test-only policy lookup that maps every path to
    /// `Never`, so the gated wrapper passes through to the
    /// inner resolver. Used by the existing test surface that
    /// asserts the rewrite outcome under the no-policy default.
    fn always_never(_: &str) -> devboy_core::secret_approval::ApproveOnUsePolicy {
        devboy_core::secret_approval::ApproveOnUsePolicy::Never
    }

    /// Wrap a [`MapResolver`] in an [`ApprovalGatedResolver`]
    /// with the test-only `Never`-policy lookup. The function-
    /// pointer shape (`fn`, not a closure) makes the return
    /// type spellable for tests that need to pin it.
    fn wrap_resolver_never(
        r: MapResolver,
    ) -> ApprovalGatedResolver<
        MapResolver,
        fn(&str) -> devboy_core::secret_approval::ApproveOnUsePolicy,
    > {
        ApprovalGatedResolver::new(
            r,
            std::sync::Arc::new(devboy_core::secret_approval::SessionApprovalCache::new()),
            always_never,
        )
    }

    impl SecretResolver for MapResolver {
        fn resolve(&self, path: &str) -> Result<SecretString, AErr> {
            let map = self.entries.lock().unwrap();
            match map.get(path) {
                Some(v) => Ok(SecretString::from(v.clone())),
                None => Err(AErr::NotFound {
                    path: path.to_owned(),
                }),
            }
        }
    }

    // -- parse_aliased_value ------------------------------------

    #[test]
    fn parse_aliased_value_recognises_bearer_alias() {
        let p = parse_aliased_value("Bearer @secret:team/x/y").unwrap();
        assert_eq!(p.path, "team/x/y");
        assert_eq!(p.scheme.as_deref(), Some("Bearer"));
    }

    #[test]
    fn parse_aliased_value_recognises_token_alias() {
        let p = parse_aliased_value("Token @secret:foo/bar/baz").unwrap();
        assert_eq!(p.scheme.as_deref(), Some("Token"));
    }

    #[test]
    fn parse_aliased_value_recognises_whole_value_alias() {
        let p = parse_aliased_value("@secret:team/x/y").unwrap();
        assert_eq!(p.path, "team/x/y");
        assert!(p.scheme.is_none());
    }

    #[test]
    fn parse_aliased_value_recognises_bearer_case_insensitive() {
        // RFC 7235 says auth-scheme is case-insensitive.
        let p = parse_aliased_value("BEARER @secret:a/b/c").unwrap();
        assert_eq!(p.scheme.as_deref(), Some("Bearer"));
    }

    #[test]
    fn parse_aliased_value_rejects_unknown_scheme() {
        // `Basic` isn't in our recognised list — the rest of the
        // value isn't an alias either, so we get None.
        assert!(parse_aliased_value("Basic dXNlcjpwYXNz").is_none());
    }

    #[test]
    fn parse_aliased_value_rejects_plain_token() {
        assert!(parse_aliased_value("ghp_realtoken").is_none());
        assert!(parse_aliased_value("Bearer ghp_realtoken").is_none());
    }

    #[test]
    fn parse_aliased_value_rejects_partial_alias() {
        // Per ADR-020 §5 the alias must be the whole value (or
        // come right after a recognised scheme).
        assert!(parse_aliased_value("xxx@secret:a/b/c").is_none());
    }

    // -- rewrite_outgoing_headers happy paths -------------------

    fn headers_with(name: &str, value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
        h
    }

    #[test]
    fn bearer_alias_in_authorization_is_rewritten() {
        let mut h = headers_with("authorization", "Bearer @secret:team/gitlab/token");
        let resolver = MapResolver::new([("team/gitlab/token", "glpat-fixture")]);
        let outcome = rewrite_outgoing_headers(&mut h, &wrap_resolver_never(resolver)).unwrap();
        assert_eq!(outcome.rewrites.len(), 1);
        assert_eq!(
            h.get("authorization").unwrap().to_str().unwrap(),
            "Bearer glpat-fixture"
        );
        assert_eq!(outcome.rewrites[0].header_name, "authorization");
        assert_eq!(outcome.rewrites[0].scheme.as_deref(), Some("Bearer"));
        assert_eq!(outcome.rewrites[0].original_alias, "team/gitlab/token");
    }

    #[test]
    fn whole_value_alias_in_x_api_key_is_rewritten() {
        let mut h = headers_with("x-api-key", "@secret:personal/openai/key");
        let resolver = MapResolver::new([("personal/openai/key", "sk-fixture")]);
        let outcome = rewrite_outgoing_headers(&mut h, &wrap_resolver_never(resolver)).unwrap();
        assert_eq!(h.get("x-api-key").unwrap().to_str().unwrap(), "sk-fixture");
        assert!(outcome.rewrites[0].scheme.is_none());
    }

    #[test]
    fn private_token_with_token_scheme_is_rewritten() {
        let mut h = headers_with("private-token", "Token @secret:team/gitlab/token");
        let resolver = MapResolver::new([("team/gitlab/token", "glpat-fixture")]);
        rewrite_outgoing_headers(&mut h, &wrap_resolver_never(resolver)).unwrap();
        assert_eq!(
            h.get("private-token").unwrap().to_str().unwrap(),
            "Token glpat-fixture"
        );
    }

    #[test]
    fn pass_through_when_no_alias_present() {
        let mut h = headers_with("authorization", "Bearer real-static-token");
        let resolver = MapResolver::new([]);
        let outcome = rewrite_outgoing_headers(&mut h, &wrap_resolver_never(resolver)).unwrap();
        assert!(outcome.rewrites.is_empty());
        assert!(!outcome.changed());
        assert_eq!(
            h.get("authorization").unwrap().to_str().unwrap(),
            "Bearer real-static-token"
        );
    }

    #[test]
    fn non_whitelisted_header_is_not_rewritten_even_with_alias() {
        // The whitelist is the security boundary; arbitrary
        // headers do NOT trigger rewriting even if they contain a
        // valid-looking alias.
        let mut h = headers_with("x-custom-header", "@secret:team/x/y");
        let resolver = MapResolver::new([("team/x/y", "should-not-be-used")]);
        let outcome = rewrite_outgoing_headers(&mut h, &wrap_resolver_never(resolver)).unwrap();
        assert!(outcome.rewrites.is_empty());
        assert_eq!(
            h.get("x-custom-header").unwrap().to_str().unwrap(),
            "@secret:team/x/y"
        );
    }

    #[test]
    fn multiple_aliased_headers_each_rewritten() {
        let mut h = HeaderMap::new();
        h.insert(
            "authorization",
            HeaderValue::from_static("Bearer @secret:team/auth/token"),
        );
        h.insert(
            "x-api-key",
            HeaderValue::from_static("@secret:team/api/key"),
        );
        let resolver = MapResolver::new([
            ("team/auth/token", "auth-fixture"),
            ("team/api/key", "api-fixture"),
        ]);
        let outcome = rewrite_outgoing_headers(&mut h, &wrap_resolver_never(resolver)).unwrap();
        assert_eq!(outcome.rewrites.len(), 2);
        assert_eq!(
            h.get("authorization").unwrap().to_str().unwrap(),
            "Bearer auth-fixture"
        );
        assert_eq!(h.get("x-api-key").unwrap().to_str().unwrap(), "api-fixture");
    }

    // -- Error propagation -------------------------------------

    #[test]
    fn missing_resolver_entry_propagates_error() {
        let mut h = headers_with("authorization", "Bearer @secret:nope/nope/nope");
        let resolver = MapResolver::new([]);
        let err = rewrite_outgoing_headers(&mut h, &wrap_resolver_never(resolver)).unwrap_err();
        match err {
            ProxyHeaderError::Resolve { header, path, .. } => {
                assert_eq!(header, "authorization");
                assert_eq!(path, "nope/nope/nope");
            }
            other => panic!("expected Resolve, got {other:?}"),
        }
    }

    // -- Transcript value --------------------------------------

    #[test]
    fn transcript_value_reproduces_original_alias_with_scheme() {
        let r = HeaderRewrite {
            header_name: "authorization".into(),
            original_alias: "team/gitlab/token-deploy".into(),
            scheme: Some("Bearer".into()),
        };
        assert_eq!(
            r.transcript_value(),
            "Bearer @secret:team/gitlab/token-deploy"
        );
    }

    #[test]
    fn transcript_value_reproduces_whole_value_alias() {
        let r = HeaderRewrite {
            header_name: "x-api-key".into(),
            original_alias: "personal/openai/key".into(),
            scheme: None,
        };
        assert_eq!(r.transcript_value(), "@secret:personal/openai/key");
    }

    // -- F3: approve-on-use gating fires before resolve -----------

    #[test]
    fn gated_resolver_refuses_session_policy_without_cache_hit() {
        use devboy_core::secret_approval::{
            ApprovalGatedResolver, ApproveOnUsePolicy, SessionApprovalCache,
        };
        let mut h = headers_with("authorization", "Bearer @secret:team/prod-db/password");
        let inner = MapResolver::new([("team/prod-db/password", "should-not-leak")]);
        let cache = std::sync::Arc::new(SessionApprovalCache::new());
        let gated = ApprovalGatedResolver::new(inner, cache, |_| ApproveOnUsePolicy::PerCall);
        let err = rewrite_outgoing_headers(&mut h, &gated).unwrap_err();
        match err {
            ProxyHeaderError::Resolve { path, .. } => {
                assert_eq!(path, "team/prod-db/password");
            }
            other => panic!("expected Resolve error from gate, got {other:?}"),
        }
        // Original header is left untouched — no plaintext leak.
        assert_eq!(
            h.get("authorization").unwrap().to_str().unwrap(),
            "Bearer @secret:team/prod-db/password",
            "the alias must NOT have been replaced when the gate refuses"
        );
    }

    #[test]
    fn gated_resolver_allows_session_policy_after_cache_record() {
        use devboy_core::secret_approval::{
            ApprovalGatedResolver, ApproveOnUsePolicy, SessionApprovalCache,
        };
        use std::time::Duration;
        let mut h = headers_with("authorization", "Bearer @secret:team/jira/api-key");
        let inner = MapResolver::new([("team/jira/api-key", "jira-fixture")]);
        let cache = std::sync::Arc::new(SessionApprovalCache::new());
        cache.record_session("team/jira/api-key", Duration::from_secs(300));
        let gated = ApprovalGatedResolver::new(inner, cache, |_| ApproveOnUsePolicy::Session);
        let outcome = rewrite_outgoing_headers(&mut h, &gated).unwrap();
        assert_eq!(outcome.rewrites.len(), 1);
        assert_eq!(
            h.get("authorization").unwrap().to_str().unwrap(),
            "Bearer jira-fixture"
        );
    }

    // -- End-to-end with mock upstream ---------------------------

    #[tokio::test]
    async fn end_to_end_rewriting_with_mock_upstream_via_reqwest() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/")
                    .header("Authorization", "Bearer ghp-end-to-end-fixture");
                then.status(204);
            })
            .await;

        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_static("Bearer @secret:team/github/pat"),
        );

        let resolver = MapResolver::new([("team/github/pat", "ghp-end-to-end-fixture")]);
        let outcome =
            rewrite_outgoing_headers(&mut headers, &wrap_resolver_never(resolver)).unwrap();
        assert_eq!(outcome.rewrites.len(), 1);

        let client = reqwest::Client::new();
        let resp = client
            .get(server.base_url())
            .headers(headers)
            .send()
            .await
            .unwrap();
        // The mock matched (returned 204) only because it saw the
        // resolved header. If rewriting were broken, httpmock
        // would 404 (no matching mock) and this assert would
        // fail.
        assert_eq!(resp.status().as_u16(), 204);

        // The transcript value still shows the alias the agent
        // would have constructed — this is the property ADR-020
        // §5 third bullet promises.
        assert_eq!(
            outcome.rewrites[0].transcript_value(),
            "Bearer @secret:team/github/pat"
        );
    }
}
