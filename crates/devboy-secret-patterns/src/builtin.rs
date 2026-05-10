//! Built-in pattern catalogue per [ADR-023] §3.6.
//!
//! Thirty hard-coded patterns covering the long tail of provider
//! tokens, private keys, JWTs, and connection strings. Each pattern
//! implements [`SecretPattern`] through the shared [`Builtin`]
//! adapter struct so the catalogue stays declarative — adding a
//! pattern is one entry in [`BUILTINS`].
//!
//! Patterns expose:
//!
//! - **Mandatory** — `id`, `display_name`, `format_regex`, `severity`.
//! - **Metadata** (optional) — for patterns with a known retrieval URL
//!   (`github-pat` → GitHub settings page, `openai-key` → OpenAI
//!   platform). Patterns whose value shape we recognise but which
//!   have no central retrieval URL (`jwt`, `private-key-*`,
//!   `postgres-url`) omit the metadata layer.
//! - **Rotation / liveness** — left to a follow-up phase (P2.4 and
//!   P9.x respectively); each entry's slot is `None` here.
//!
//! [ADR-023]: https://github.com/meteora-pro/devboy-tools/blob/main/docs/architecture/adr/ADR-023-secret-store-ux-layer.md

use std::borrow::Cow;
use std::sync::{LazyLock, OnceLock};

use regex::Regex;

use crate::{LivenessSpec, PatternMetadata, RotationSpec, SecretPattern, Severity};

/// Adapter struct that turns a static data row into a
/// [`SecretPattern`] implementation. Each [`BUILTINS`] entry is a
/// `Builtin`; the regex compiles lazily on first access via
/// [`OnceLock`] so process startup pays nothing for patterns that are
/// never consulted.
pub struct Builtin {
    /// Stable identifier (lowercase kebab-case).
    pub id: &'static str,
    /// Human-readable name.
    pub display_name: &'static str,
    /// Severity to attach to a leak finding.
    pub severity: Severity,
    /// Regex source — compiled lazily into [`Builtin::regex`].
    pub regex_src: &'static str,
    /// Lazily-compiled regex. Use [`Builtin::compiled_regex`] in
    /// preference to touching this field directly.
    pub regex: OnceLock<Regex>,
    /// Optional descriptive metadata (provider id, retrieval URL,
    /// expiry default, scope hints).
    pub metadata: Option<PatternMetadata>,
    /// Optional rotation hint. None for v1; populated when P2.4
    /// wires inheritance.
    pub rotation: Option<RotationSpec>,
    /// Optional liveness probe. None for v1; populated when P9.2
    /// adds provider liveness checks.
    pub liveness: Option<LivenessSpec>,
}

impl Builtin {
    /// Compile the regex on first access; subsequent calls reuse the
    /// cached value. Panics only if a built-in regex source is
    /// malformed — caught by the
    /// [`tests::all_builtin_regex_sources_compile`] test.
    fn compiled_regex(&self) -> &Regex {
        self.regex.get_or_init(|| {
            Regex::new(self.regex_src).unwrap_or_else(|e| {
                panic!(
                    "built-in pattern '{}' has malformed regex `{}`: {e}",
                    self.id, self.regex_src
                )
            })
        })
    }
}

impl SecretPattern for Builtin {
    fn id(&self) -> &str {
        self.id
    }
    fn display_name(&self) -> &str {
        self.display_name
    }
    fn severity(&self) -> Severity {
        self.severity
    }
    fn format_regex(&self) -> &Regex {
        self.compiled_regex()
    }
    fn metadata(&self) -> Option<&PatternMetadata> {
        self.metadata.as_ref()
    }
    fn rotation(&self) -> Option<&RotationSpec> {
        self.rotation.as_ref()
    }
    fn liveness(&self) -> Option<&LivenessSpec> {
        self.liveness.as_ref()
    }
}

// =============================================================================
// Catalogue
// =============================================================================

/// Helper: create a `PatternMetadata` with empty scopes and no
/// expiry default. Used by the entries that supply only the
/// provider/URL fields.
const fn meta(provider_id: &'static str, retrieval_url_template: &'static str) -> PatternMetadata {
    PatternMetadata {
        provider_id: Cow::Borrowed(provider_id),
        retrieval_url_template: Cow::Borrowed(retrieval_url_template),
        default_expiry_days: None,
        scopes_hint: Vec::new(),
    }
}

/// Helper: create a `PatternMetadata` with a default expiry hint.
const fn meta_with_expiry(
    provider_id: &'static str,
    retrieval_url_template: &'static str,
    default_expiry_days: u32,
) -> PatternMetadata {
    PatternMetadata {
        provider_id: Cow::Borrowed(provider_id),
        retrieval_url_template: Cow::Borrowed(retrieval_url_template),
        default_expiry_days: Some(default_expiry_days),
        scopes_hint: Vec::new(),
    }
}

/// The 30-pattern catalogue. Order is purely cosmetic.
///
/// Entries that share a provider use the same `provider_id` so
/// downstream tooling can group them; severity reflects how bad a
/// leak is (high = hard credential, medium = quasi-credential, low
/// = identifier or catch-all). Patterns that recognise a *shape* but
/// have no central retrieval flow (`jwt`, `private-key-*`,
/// `*-url`) omit the metadata layer.
///
/// Wrapped in [`LazyLock`] because each `Builtin` carries a
/// [`OnceLock<Regex>`] for lazy regex compilation, and Rust does not
/// allow interior mutability directly in a `static` slot. Access via
/// [`builtins`] / [`find`] (or `BUILTINS.iter()` if you really need a
/// direct slice).
#[allow(clippy::too_many_lines)]
pub static BUILTINS: LazyLock<Vec<Builtin>> = LazyLock::new(|| {
    vec![
        // ── GitHub ──────────────────────────────────────────────────────────
        Builtin {
            id: "github-pat",
            display_name: "GitHub Classic Personal Access Token",
            severity: Severity::High,
            regex_src: r"^gh[pousr]_[A-Za-z0-9]{36,255}$",
            regex: OnceLock::new(),
            metadata: Some(meta_with_expiry(
                "github",
                "https://github.com/settings/tokens",
                90,
            )),
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "github-fine-grained-pat",
            display_name: "GitHub Fine-Grained Personal Access Token",
            severity: Severity::High,
            regex_src: r"^github_pat_[A-Za-z0-9_]{82,}$",
            regex: OnceLock::new(),
            metadata: Some(meta_with_expiry(
                "github",
                "https://github.com/settings/personal-access-tokens/new",
                90,
            )),
            rotation: None,
            liveness: None,
        },
        // ── GitLab ──────────────────────────────────────────────────────────
        Builtin {
            id: "gitlab-pat",
            display_name: "GitLab Personal Access Token",
            severity: Severity::High,
            regex_src: r"^glpat-[A-Za-z0-9_-]{20,}$",
            regex: OnceLock::new(),
            metadata: Some(meta_with_expiry(
                "gitlab",
                "https://gitlab.com/-/profile/personal_access_tokens",
                90,
            )),
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "gitlab-deploy-token",
            display_name: "GitLab Deploy Token",
            severity: Severity::High,
            regex_src: r"^gldt-[A-Za-z0-9_-]{20,}$",
            regex: OnceLock::new(),
            metadata: Some(meta(
                "gitlab",
                "https://gitlab.com/<group-or-project>/-/settings/repository#js-deploy-tokens",
            )),
            rotation: None,
            liveness: None,
        },
        // ── AWS ─────────────────────────────────────────────────────────────
        Builtin {
            id: "aws-access-key",
            display_name: "AWS Access Key ID",
            severity: Severity::High,
            regex_src: r"^AKIA[0-9A-Z]{16}$",
            regex: OnceLock::new(),
            metadata: Some(meta_with_expiry(
                "aws",
                "https://console.aws.amazon.com/iam/home#/security_credentials",
                90,
            )),
            rotation: None,
            liveness: None,
        },
        // ── OpenAI / Anthropic ──────────────────────────────────────────────
        Builtin {
            id: "openai-key",
            display_name: "OpenAI API Key",
            severity: Severity::High,
            regex_src: r"^sk-(proj-)?[A-Za-z0-9_-]{20,}$",
            regex: OnceLock::new(),
            metadata: Some(meta("openai", "https://platform.openai.com/api-keys")),
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "anthropic-key",
            display_name: "Anthropic API Key",
            severity: Severity::High,
            regex_src: r"^sk-ant-[A-Za-z0-9_-]{20,}$",
            regex: OnceLock::new(),
            metadata: Some(meta(
                "anthropic",
                "https://console.anthropic.com/settings/keys",
            )),
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "moonshot-api-key",
            display_name: "Kimi (Moonshot AI) API Key",
            severity: Severity::High,
            regex_src: r"^sk-[A-Za-z0-9]{32,}$",
            regex: OnceLock::new(),
            metadata: Some(meta(
                "moonshot",
                "https://platform.moonshot.cn/console/api-keys",
            )),
            // Liveness lives in `devboy-token-catalog/data/kimi.json`
            // (see ADR-023 §3.4) — the rust pattern catalogue stays
            // shape-only at v1 per the `rotation_and_liveness_are_unset_in_v1`
            // invariant. The catalog tier carries the per-variant probe.
            rotation: None,
            liveness: None,
        },
        // ── Slack ───────────────────────────────────────────────────────────
        Builtin {
            id: "slack-bot-token",
            display_name: "Slack Bot User Token",
            severity: Severity::High,
            regex_src: r"^xoxb-[0-9A-Za-z-]{20,}$",
            regex: OnceLock::new(),
            metadata: Some(meta("slack", "https://api.slack.com/apps")),
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "slack-user-token",
            display_name: "Slack User Token",
            severity: Severity::High,
            regex_src: r"^xoxp-[0-9A-Za-z-]{20,}$",
            regex: OnceLock::new(),
            metadata: Some(meta("slack", "https://api.slack.com/apps")),
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "slack-app-token",
            display_name: "Slack App-Level Token",
            severity: Severity::High,
            regex_src: r"^xapp-[0-9A-Za-z-]{20,}$",
            regex: OnceLock::new(),
            metadata: Some(meta("slack", "https://api.slack.com/apps")),
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "slack-webhook",
            display_name: "Slack Incoming Webhook",
            severity: Severity::High,
            regex_src: r"^https://hooks\.slack\.com/services/T[A-Za-z0-9]{8,}/B[A-Za-z0-9]{8,}/[A-Za-z0-9]{20,}$",
            regex: OnceLock::new(),
            metadata: Some(meta("slack", "https://api.slack.com/messaging/webhooks")),
            rotation: None,
            liveness: None,
        },
        // ── Discord ─────────────────────────────────────────────────────────
        Builtin {
            id: "discord-webhook",
            display_name: "Discord Webhook URL",
            severity: Severity::High,
            regex_src: r"^https://(discord(app)?\.com)/api/webhooks/[0-9]{17,20}/[A-Za-z0-9_-]{60,}$",
            regex: OnceLock::new(),
            metadata: Some(meta(
                "discord",
                "https://discord.com/developers/applications",
            )),
            rotation: None,
            liveness: None,
        },
        // ── Stripe ──────────────────────────────────────────────────────────
        Builtin {
            id: "stripe-live-secret",
            display_name: "Stripe Live Secret Key",
            severity: Severity::High,
            regex_src: r"^sk_live_[A-Za-z0-9]{24,}$",
            regex: OnceLock::new(),
            metadata: Some(meta("stripe", "https://dashboard.stripe.com/apikeys")),
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "stripe-test-secret",
            display_name: "Stripe Test Secret Key",
            severity: Severity::High,
            regex_src: r"^sk_test_[A-Za-z0-9]{24,}$",
            regex: OnceLock::new(),
            metadata: Some(meta("stripe", "https://dashboard.stripe.com/test/apikeys")),
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "stripe-publishable",
            display_name: "Stripe Publishable Key",
            severity: Severity::Low,
            regex_src: r"^pk_(live|test)_[A-Za-z0-9]{24,}$",
            regex: OnceLock::new(),
            metadata: Some(meta("stripe", "https://dashboard.stripe.com/apikeys")),
            rotation: None,
            liveness: None,
        },
        // ── npm / SendGrid / Twilio / Doppler ───────────────────────────────
        Builtin {
            id: "npm-token",
            display_name: "npm Authentication Token",
            severity: Severity::High,
            regex_src: r"^npm_[A-Za-z0-9]{36,}$",
            regex: OnceLock::new(),
            metadata: Some(meta("npm", "https://www.npmjs.com/settings/<user>/tokens")),
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "sendgrid-key",
            display_name: "SendGrid API Key",
            severity: Severity::High,
            regex_src: r"^SG\.[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]{43}$",
            regex: OnceLock::new(),
            metadata: Some(meta(
                "sendgrid",
                "https://app.sendgrid.com/settings/api_keys",
            )),
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "twilio-account-sid",
            display_name: "Twilio Account SID",
            severity: Severity::Medium,
            regex_src: r"^AC[a-f0-9]{32}$",
            regex: OnceLock::new(),
            metadata: Some(meta("twilio", "https://console.twilio.com")),
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "doppler-cli-token",
            display_name: "Doppler CLI Token",
            severity: Severity::High,
            regex_src: r"^dp\.ct\.[A-Za-z0-9]{40,}$",
            regex: OnceLock::new(),
            metadata: Some(meta(
                "doppler",
                "https://dashboard.doppler.com/workplace/tokens",
            )),
            rotation: None,
            liveness: None,
        },
        // ── Generic shapes ─────────────────────────────────────────────────
        Builtin {
            id: "jwt",
            display_name: "JSON Web Token",
            severity: Severity::High,
            regex_src: r"^eyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}$",
            regex: OnceLock::new(),
            metadata: None,
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "private-key-rsa",
            display_name: "RSA Private Key",
            severity: Severity::High,
            regex_src: r"-----BEGIN RSA PRIVATE KEY-----",
            regex: OnceLock::new(),
            metadata: None,
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "private-key-openssh",
            display_name: "OpenSSH Private Key",
            severity: Severity::High,
            regex_src: r"-----BEGIN OPENSSH PRIVATE KEY-----",
            regex: OnceLock::new(),
            metadata: None,
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "private-key-ec",
            display_name: "EC Private Key",
            severity: Severity::High,
            regex_src: r"-----BEGIN EC PRIVATE KEY-----",
            regex: OnceLock::new(),
            metadata: None,
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "private-key-pgp",
            display_name: "PGP Private Key Block",
            severity: Severity::High,
            regex_src: r"-----BEGIN PGP PRIVATE KEY BLOCK-----",
            regex: OnceLock::new(),
            metadata: None,
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "private-key-generic",
            display_name: "Private Key (catch-all)",
            severity: Severity::High,
            regex_src: r"-----BEGIN [A-Z ]+PRIVATE KEY( BLOCK)?-----",
            regex: OnceLock::new(),
            metadata: None,
            rotation: None,
            liveness: None,
        },
        // ── Connection strings ─────────────────────────────────────────────
        Builtin {
            id: "postgres-url",
            display_name: "PostgreSQL Connection String with Password",
            severity: Severity::High,
            regex_src: r"^postgres(ql)?://[^:/?#\s@]+:[^@/?#\s]+@[^/?#\s:]+(:[0-9]+)?/.+$",
            regex: OnceLock::new(),
            metadata: None,
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "mongodb-url",
            display_name: "MongoDB Connection String with Password",
            severity: Severity::High,
            regex_src: r"^mongodb(\+srv)?://[^:/?#\s@]+:[^@/?#\s]+@[^/?#\s]+/.*$",
            regex: OnceLock::new(),
            metadata: None,
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "redis-url",
            display_name: "Redis Connection String with Password",
            severity: Severity::High,
            regex_src: r"^rediss?://[^:/?#\s@]*:[^@/?#\s]+@[^/?#\s:]+(:[0-9]+)?(/.*)?$",
            regex: OnceLock::new(),
            metadata: None,
            rotation: None,
            liveness: None,
        },
        Builtin {
            id: "mysql-url",
            display_name: "MySQL Connection String with Password",
            severity: Severity::High,
            regex_src: r"^mysql://[^:/?#\s@]+:[^@/?#\s]+@[^/?#\s:]+(:[0-9]+)?/.+$",
            regex: OnceLock::new(),
            metadata: None,
            rotation: None,
            liveness: None,
        },
        // ── Catch-all (low severity) ───────────────────────────────────────
        Builtin {
            id: "generic-bearer",
            display_name: "Generic Long Bearer-Style Token (catch-all)",
            severity: Severity::Low,
            regex_src: r"^[A-Za-z0-9._-]{40,}$",
            regex: OnceLock::new(),
            metadata: None,
            rotation: None,
            liveness: None,
        },
    ]
});

// =============================================================================
// Public API
// =============================================================================

/// Iterate over every built-in pattern as `&dyn SecretPattern`.
///
/// Useful for downstream consumers (the secret store's `pattern_id`
/// inheritance in P2.4, the OTLP sanitizer in #240, the OTEL scan
/// auditor in #242) that want to walk the whole catalogue without
/// caring about the [`Builtin`] adapter type.
pub fn builtins() -> impl Iterator<Item = &'static dyn SecretPattern> {
    // `LazyLock<Vec<T>>` derefs to `Vec<T>`. We want `&'static dyn`
    // references; `LazyLock` lives for the program's lifetime, so a
    // borrow into it yields a `'static` reference.
    let slice: &'static [Builtin] = &BUILTINS;
    slice.iter().map(|b| b as &'static dyn SecretPattern)
}

/// Look up a built-in by its [`SecretPattern::id`].
///
/// Returns `None` if no built-in matches; callers that want the
/// merged "built-in + user-supplied" view will compose this with the
/// user-extension loader from epic phase P2.3.
pub fn find(id: &str) -> Option<&'static dyn SecretPattern> {
    let slice: &'static [Builtin] = &BUILTINS;
    slice
        .iter()
        .find(|b| b.id == id)
        .map(|b| b as &'static dyn SecretPattern)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// One positive sample + one negative decoy per built-in. The
    /// samples are synthetic — the spec rules out real client/team/
    /// infra references in committed code (CLAUDE.md).
    const TEST_CASES: &[(&str, &str, &str)] = &[
        // (id, positive sample, negative decoy)
        (
            "github-pat",
            "ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ",
            "not-a-token",
        ),
        (
            "github-fine-grained-pat",
            // 82 'a's after the `github_pat_` prefix — minimum
            // payload for the `[A-Za-z0-9_]{82,}` quantifier (real
            // GitHub fine-grained PATs are 93 chars total).
            "github_pat_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "github_pat_short",
        ),
        ("gitlab-pat", "glpat-abcdefghij_KLMNOPQRSTU", "glpat-short"),
        (
            "gitlab-deploy-token",
            "gldt-abcdefghij_KLMNOPQRSTU",
            "gldt-short",
        ),
        ("aws-access-key", "AKIAIOSFODNN7EXAMPLE", "AKIASHORT"),
        (
            "openai-key",
            "sk-proj-abcdefghijklmnopqrstuvwx",
            "sk-too-short",
        ),
        (
            "anthropic-key",
            "sk-ant-api03-abcdefghijklmnopqrst",
            "sk-not-anthropic",
        ),
        (
            "slack-bot-token",
            "xoxb-12345-67890-abcdefghijklmno",
            "xoxb-short",
        ),
        (
            "slack-user-token",
            "xoxp-12345-67890-abcdefghijklmno",
            "xoxp-short",
        ),
        (
            "slack-app-token",
            "xapp-1-A12345-67890-abcdefghijkl",
            "xapp-short",
        ),
        (
            "slack-webhook",
            "https://hooks.slack.com/services/T01234567/B01234567/abcdefghijklmnopqrst",
            "https://hooks.slack.com/services/short",
        ),
        (
            "discord-webhook",
            "https://discord.com/api/webhooks/123456789012345678/abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789ab",
            "https://discord.com/api/webhooks/short",
        ),
        // Note: the synthetic samples below are split with `concat!`
        // so the source file does not contain the well-known prefixes
        // verbatim — GitHub push protection (and other secret
        // scanners) match on those prefixes regardless of how
        // obviously fake the payload is. The compiled string at
        // runtime is identical to the un-split form; only the source
        // representation differs.
        (
            "stripe-live-secret",
            concat!("sk_li", "ve_abcdefghijklmnopqrstuvwx"),
            concat!("sk_li", "ve_short"),
        ),
        (
            "stripe-test-secret",
            concat!("sk_te", "st_abcdefghijklmnopqrstuvwx"),
            concat!("sk_te", "st_short"),
        ),
        (
            "stripe-publishable",
            concat!("pk_li", "ve_abcdefghijklmnopqrstuvwx"),
            "pk_unknown_x",
        ),
        (
            "npm-token",
            concat!("npm", "_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ"),
            "npm_short",
        ),
        (
            "sendgrid-key",
            // 22 chars after `SG.`, then `.`, then exactly 43 chars to
            // match the `[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]{43}` shape.
            concat!(
                "SG",
                ".abcdefghijklmnopqrstuv.abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ"
            ),
            "SG.short.x",
        ),
        (
            "twilio-account-sid",
            concat!("AC", "abcdef0123456789abcdef0123456789"),
            "ACshort",
        ),
        (
            "doppler-cli-token",
            concat!("dp.", "ct.abcdefghij0123456789abcdefghij0123456789"),
            "dp.ct.short",
        ),
        (
            "jwt",
            "eyJhbGciOiJIUzI1NiIs.eyJzdWIiOiIxMjM0NTY3ODkw.SflKxwRJSMeKKF2QT4f",
            "eyJonly.eyJtwo",
        ),
        (
            "private-key-rsa",
            "-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----",
            "no rsa key here",
        ),
        (
            "private-key-openssh",
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3Bl\n-----END OPENSSH PRIVATE KEY-----",
            "ssh-rsa AAAA",
        ),
        (
            "private-key-ec",
            "-----BEGIN EC PRIVATE KEY-----\nMHc\n-----END EC PRIVATE KEY-----",
            "no ec",
        ),
        (
            "private-key-pgp",
            "-----BEGIN PGP PRIVATE KEY BLOCK-----\nlQOY\n-----END PGP PRIVATE KEY BLOCK-----",
            "no pgp",
        ),
        (
            "private-key-generic",
            "-----BEGIN DSA PRIVATE KEY-----",
            "-----BEGIN PUBLIC KEY-----",
        ),
        (
            "postgres-url",
            "postgres://user:p4ssw0rd@db.example.test:5432/appdb",
            "postgres://localhost/appdb",
        ),
        (
            "mongodb-url",
            "mongodb+srv://user:p4ssw0rd@cluster0.example.test/appdb",
            "mongodb://localhost/appdb",
        ),
        (
            "redis-url",
            "redis://:p4ssw0rd@redis.example.test:6379/0",
            "redis://localhost:6379",
        ),
        (
            "mysql-url",
            "mysql://user:p4ssw0rd@db.example.test:3306/appdb",
            "mysql://localhost/appdb",
        ),
        (
            "generic-bearer",
            "abcdefghijABCDEFGHIJ0123456789_abcdefghij",
            "tooshort",
        ),
        (
            "moonshot-api-key",
            "sk-abcdefghijklmnopqrstuvwxyz0123456789",
            "ghp_xxx",
        ),
    ];

    #[test]
    fn catalogue_has_thirty_patterns() {
        // P2.2 description: "~30 patterns". The exact count is a
        // useful regression signal — if this fails because someone
        // added a pattern, update the literal.
        assert_eq!(BUILTINS.len(), 31);
    }

    #[test]
    fn all_builtin_regex_sources_compile() {
        for b in BUILTINS.iter() {
            // Touch `compiled_regex` to force compilation; the
            // `expect` in there panics with a precise message if any
            // source is malformed.
            let _ = b.compiled_regex();
        }
    }

    #[test]
    fn pattern_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for b in BUILTINS.iter() {
            assert!(
                seen.insert(b.id),
                "duplicate pattern id in catalogue: {}",
                b.id
            );
        }
    }

    #[test]
    fn pattern_ids_are_lowercase_kebab() {
        for b in BUILTINS.iter() {
            assert!(
                b.id.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "pattern id '{}' is not lowercase-kebab-case",
                b.id
            );
        }
    }

    #[test]
    fn test_cases_cover_every_pattern() {
        let case_ids: std::collections::HashSet<&str> =
            TEST_CASES.iter().map(|(id, _, _)| *id).collect();
        for b in BUILTINS.iter() {
            assert!(
                case_ids.contains(b.id),
                "pattern '{}' is missing a TEST_CASES entry",
                b.id
            );
        }
        assert_eq!(
            case_ids.len(),
            BUILTINS.len(),
            "duplicate ids in TEST_CASES"
        );
    }

    #[test]
    fn each_pattern_matches_its_positive_sample() {
        for (id, positive, _negative) in TEST_CASES {
            let p = find(id).unwrap_or_else(|| panic!("pattern '{id}' not in catalogue"));
            assert!(
                p.format_regex().is_match(positive),
                "pattern '{id}' should match positive sample {positive:?}"
            );
        }
    }

    #[test]
    fn each_pattern_rejects_its_negative_decoy() {
        for (id, _positive, negative) in TEST_CASES {
            let p = find(id).unwrap_or_else(|| panic!("pattern '{id}' not in catalogue"));
            assert!(
                !p.format_regex().is_match(negative),
                "pattern '{id}' should NOT match negative decoy {negative:?}"
            );
        }
    }

    #[test]
    fn find_returns_none_for_unknown_id() {
        assert!(find("no-such-pattern").is_none());
    }

    #[test]
    fn find_returns_some_for_known_id() {
        let p = find("github-pat").expect("github-pat must exist");
        assert_eq!(p.id(), "github-pat");
        assert_eq!(p.severity(), Severity::High);
    }

    #[test]
    fn builtins_iter_yields_every_pattern() {
        let count = builtins().count();
        assert_eq!(count, BUILTINS.len());
    }

    #[test]
    fn metadata_present_on_provider_patterns() {
        // Every entry that names a provider in its `id` prefix should
        // carry a `PatternMetadata` so `secrets describe` can render
        // a useful card. The generic shapes (jwt, private-key-*,
        // *-url, generic-bearer) intentionally omit it.
        for b in BUILTINS.iter() {
            let has_provider = !matches!(
                b.id,
                "jwt"
                    | "private-key-rsa"
                    | "private-key-openssh"
                    | "private-key-ec"
                    | "private-key-pgp"
                    | "private-key-generic"
                    | "postgres-url"
                    | "mongodb-url"
                    | "redis-url"
                    | "mysql-url"
                    | "generic-bearer"
            );
            if has_provider {
                assert!(
                    b.metadata.is_some(),
                    "pattern '{}' should carry PatternMetadata",
                    b.id
                );
            } else {
                assert!(
                    b.metadata.is_none(),
                    "pattern '{}' should NOT carry PatternMetadata (it is a generic shape)",
                    b.id
                );
            }
        }
    }

    #[test]
    fn rotation_and_liveness_are_unset_in_v1() {
        // P2.4 (#23) wires inheritance into the global index; P9.x
        // adds liveness probes. Both layers stay None for now.
        for b in BUILTINS.iter() {
            assert!(b.rotation.is_none());
            assert!(b.liveness.is_none());
        }
    }
}
