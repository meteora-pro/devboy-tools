//! Liveness probes for provider plugins per [ADR-021] §6 ("the
//! validation framework") and ADR-021 §9 (`doctor` integration —
//! "current status: provisioned / expiring / missing /
//! format-invalid").
//!
//! [ADR-020]'s format validator (epic phase P9.1) tells you a
//! candidate value *looks* right. Liveness asks the upstream the
//! harder question: does it actually work? Plugins implement
//! [`LivenessProbe`] against their native auth-introspection
//! endpoint (GitHub `/user`, GitLab `/personal_access_tokens/self`,
//! …) and report a typed [`LivenessResult`]. The wider validation
//! flow (P9.4 `devboy secrets validate`, P9.3 expiry tracking)
//! consumes the result.
//!
//! This module is the *contract*; concrete impls live in
//! `crates/plugins/api/{github,gitlab,…}`.
//!
//! ## Token argument
//!
//! `test(&self, token: &SecretString)` deliberately takes the
//! token as a parameter rather than reading from `self`. The
//! validate flow may want to test a *candidate* token before
//! storing it — see also the format validator's same
//! pattern (it takes the value, not the path).
//!
//! ## Variants
//!
//! - [`LivenessStatus::Live`]: token authenticated and the
//!   upstream returned account info.
//! - [`LivenessStatus::Revoked`]: token was rejected as
//!   permanently invalid (401 with revoke semantics, or 403 on a
//!   probe endpoint that the token previously had access to).
//! - [`LivenessStatus::Expired`]: token was rejected because it
//!   had a hard expiration (mostly GitLab personal-access-tokens
//!   with `expires_at` < today).
//! - [`LivenessStatus::Throttled`]: probe hit a 429. Caller may
//!   retry later; the result is inconclusive.
//! - [`LivenessStatus::NotImplemented`]: the provider plugin
//!   has no native introspection (or hasn't been wired yet).
//!   The shared default in this module returns this; concrete
//!   plugins override.
//! - [`LivenessStatus::Error`]: anything else (network failure,
//!   non-JSON body, unexpected 5xx). The `detail` field carries
//!   the message.
//!
//! [ADR-020]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-020-secret-manifest-and-alias-resolution.md
//! [ADR-021]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-021-external-secret-sources.md

use async_trait::async_trait;
use secrecy::SecretString;

use crate::Result;

/// What the upstream said about a probed token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LivenessStatus {
    /// Token authenticated.
    Live,
    /// Token was rejected as permanently invalid.
    Revoked,
    /// Token had a hard expiration that has now passed.
    Expired,
    /// Probe was rate-limited; result inconclusive.
    Throttled,
    /// Provider plugin has no native introspection.
    NotImplemented,
    /// Unexpected error (network, non-JSON, 5xx, …). The
    /// `detail` field carries the message.
    Error,
}

/// Outcome of [`LivenessProbe::test`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivenessResult {
    /// Classified outcome.
    pub status: LivenessStatus,
    /// Human-readable detail — account login, scope summary,
    /// upstream error message, …. Always optional; doctor / the
    /// validate CLI render it verbatim.
    pub detail: Option<String>,
    /// Upstream-reported expiry, when the API exposes one (GitHub
    /// `github-authentication-token-expiration` header, GitLab
    /// `expires_at` JSON field). P9.3 reads this back into the
    /// global index.
    pub expires_at: Option<String>,
}

impl LivenessResult {
    /// Convenience — `Live` with a detail string.
    pub fn live(detail: impl Into<String>) -> Self {
        Self {
            status: LivenessStatus::Live,
            detail: Some(detail.into()),
            expires_at: None,
        }
    }

    /// Convenience — `Revoked` with a detail string.
    pub fn revoked(detail: impl Into<String>) -> Self {
        Self {
            status: LivenessStatus::Revoked,
            detail: Some(detail.into()),
            expires_at: None,
        }
    }

    /// Convenience — `Expired` carrying the upstream-reported
    /// expiry timestamp.
    pub fn expired(expires_at: impl Into<String>) -> Self {
        Self {
            status: LivenessStatus::Expired,
            detail: None,
            expires_at: Some(expires_at.into()),
        }
    }

    /// Convenience — `Throttled` with a detail string (e.g. the
    /// `Retry-After` header value).
    pub fn throttled(detail: impl Into<String>) -> Self {
        Self {
            status: LivenessStatus::Throttled,
            detail: Some(detail.into()),
            expires_at: None,
        }
    }

    /// Convenience — `NotImplemented` carrying the provider
    /// name so `doctor` can show "this provider has no native
    /// introspection".
    pub fn not_implemented(provider: impl Into<String>) -> Self {
        Self {
            status: LivenessStatus::NotImplemented,
            detail: Some(format!(
                "{} liveness probe is not implemented; format-only validation only",
                provider.into()
            )),
            expires_at: None,
        }
    }

    /// Convenience — `Error` with a detail string.
    pub fn error(detail: impl Into<String>) -> Self {
        Self {
            status: LivenessStatus::Error,
            detail: Some(detail.into()),
            expires_at: None,
        }
    }
}

/// Liveness probe a provider plugin implements against its
/// native introspection endpoint. The `test` method is async
/// because it does network I/O.
#[async_trait]
pub trait LivenessProbe: Send + Sync {
    /// Probe the upstream with `token`. The default impl returns
    /// `NotImplemented` so providers without a native
    /// introspection endpoint can opt-in trivially.
    async fn test(&self, _token: &SecretString) -> Result<LivenessResult> {
        Ok(LivenessResult::not_implemented(self.provider_name()))
    }

    /// Lower-cased provider name surfaced in the `NotImplemented`
    /// default and in `doctor` output. Concrete impls override
    /// to return e.g. `"github"`, `"gitlab"`.
    fn provider_name(&self) -> &str;
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Stub provider that uses the trait's default `test` impl.
    /// Verifies the default returns `NotImplemented` carrying the
    /// `provider_name`.
    struct StubProvider;

    #[async_trait]
    impl LivenessProbe for StubProvider {
        fn provider_name(&self) -> &str {
            "stub"
        }
    }

    // -- Result helpers -----------------------------------------

    #[test]
    fn live_helper_sets_status_and_detail() {
        let r = LivenessResult::live("alice");
        assert_eq!(r.status, LivenessStatus::Live);
        assert_eq!(r.detail.as_deref(), Some("alice"));
        assert!(r.expires_at.is_none());
    }

    #[test]
    fn revoked_helper_sets_status_and_detail() {
        let r = LivenessResult::revoked("token revoked by user");
        assert_eq!(r.status, LivenessStatus::Revoked);
        assert_eq!(r.detail.as_deref(), Some("token revoked by user"));
    }

    #[test]
    fn expired_helper_carries_expiry_timestamp() {
        let r = LivenessResult::expired("2025-12-31");
        assert_eq!(r.status, LivenessStatus::Expired);
        assert_eq!(r.expires_at.as_deref(), Some("2025-12-31"));
    }

    #[test]
    fn throttled_helper_holds_retry_after_detail() {
        let r = LivenessResult::throttled("retry after 60s");
        assert_eq!(r.status, LivenessStatus::Throttled);
        assert!(r.detail.unwrap().contains("retry"));
    }

    #[test]
    fn not_implemented_helper_includes_provider_name_in_detail() {
        let r = LivenessResult::not_implemented("clickup");
        assert_eq!(r.status, LivenessStatus::NotImplemented);
        let detail = r.detail.unwrap();
        assert!(detail.contains("clickup"));
        assert!(detail.contains("not implemented"));
    }

    #[test]
    fn error_helper_sets_status_and_detail() {
        let r = LivenessResult::error("connection refused");
        assert_eq!(r.status, LivenessStatus::Error);
        assert_eq!(r.detail.as_deref(), Some("connection refused"));
    }

    // -- Trait default ------------------------------------------

    #[tokio::test]
    async fn default_test_impl_returns_not_implemented_with_provider_name() {
        let p = StubProvider;
        let r = p
            .test(&SecretString::from("any-token".to_owned()))
            .await
            .unwrap();
        assert_eq!(r.status, LivenessStatus::NotImplemented);
        let detail = r.detail.unwrap();
        assert!(detail.contains("stub"));
    }

    #[tokio::test]
    async fn default_impl_works_through_dyn_trait_object() {
        // The validate flow (P9.4) will store probes in a
        // `HashMap<String, Box<dyn LivenessProbe>>`. Confirm dyn
        // compatibility for the default impl.
        let p: Box<dyn LivenessProbe> = Box::new(StubProvider);
        assert_eq!(p.provider_name(), "stub");
        let r = p.test(&SecretString::from("x".to_owned())).await.unwrap();
        assert_eq!(r.status, LivenessStatus::NotImplemented);
    }
}
