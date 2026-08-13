//! Exchanging a short-lived onboarding token for a durable one.
//!
//! # The problem
//!
//! Onboarding hands a person a command to paste, and that command
//! carries a token. Whatever is in it lands in a shell history, a
//! chat log, a screen share, a support ticket. If that token is the
//! long-lived one, every copy of the command is a live credential
//! for as long as it stays valid.
//!
//! The fix is to make what gets pasted worthless within minutes: the
//! command carries a **bootstrap** token, and the first thing
//! `devboy init` does with it is trade it for a durable one that was
//! never written down anywhere.
//!
//! # The protocol
//!
//! Deliberately small, and deliberately not about any particular
//! server. A config server opts in by adding one field to the TOML it
//! already serves:
//!
//! ```toml
//! [remote_config]
//! token_exchange_url = "https://config.example.com/api/config/exchange"
//! ```
//!
//! The client then makes one request:
//!
//! ```text
//! POST <token_exchange_url>
//! Authorization: Bearer <bootstrap token>
//! Accept: application/json
//!
//! → 200 { "token": "<durable token>", "expires_at": "<RFC 3339>"? }
//! ```
//!
//! `expires_at` is optional and advisory — it is shown to the user,
//! not enforced. A server that omits it is saying "no expiry I want
//! to promise", which is a legitimate answer.
//!
//! A server that declares no `token_exchange_url` gets exactly the
//! old behaviour: the token supplied on the command line is stored
//! as-is. Nothing about this is required to use devboy.
//!
//! # Why the exchange URL must share the config URL's origin
//!
//! The URL arrives *in a response*, and acting on it means posting a
//! live credential to wherever it points. A config server that is
//! compromised, misconfigured, or merely careless with a template
//! could otherwise redirect every new install's token to a third
//! party, and the user would see nothing unusual.
//!
//! Restricting it to the origin the client already chose to trust
//! removes that: the server can name a path on itself and nothing
//! else. It costs a deployment nothing — a config server that cannot
//! host its own exchange endpoint can decline to declare one.
//!
//! # Single use is the server's business, not the client's
//!
//! The client makes the exchange exactly once and does not retry: a
//! bootstrap token is expected to be consumed by the first successful
//! exchange, so a retry would be a second failure with a worse error
//! message. Whether it is truly single-use, and what a replay is
//! recorded as, is for the server to decide and to audit.

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

/// How long the client waits for the exchange.
///
/// Shorter than the config fetch's ten seconds on purpose: this is a
/// single small POST, and a user is watching an install hang.
const EXCHANGE_TIMEOUT_SECS: u64 = 10;

/// The durable token an exchange produced.
///
/// `token` is a [`SecretString`] rather than a `String`: this is the
/// credential the whole exchange exists to produce, and the repo's
/// secrets-discipline gate is right that a plain `String` here would
/// be one accidental `{:?}` away from the log line the scheme is
/// meant to prevent.
#[derive(Debug, Clone, Deserialize)]
pub struct ExchangedToken {
    /// The token to store.
    pub token: SecretString,
    /// Advisory expiry, RFC 3339, when the server chose to state one.
    #[serde(default)]
    pub expires_at: Option<String>,
}

impl ExchangedToken {
    /// A one-line summary safe to print.
    ///
    /// Never includes the token. The length is given instead, because
    /// "did I get something plausible?" is the question a user has,
    /// and answering it with the value would put the durable token in
    /// the terminal scrollback the whole scheme exists to keep it out
    /// of.
    pub fn describe(&self) -> String {
        match &self.expires_at {
            Some(exp) => format!(
                "received a durable token ({} chars), valid until {exp}",
                self.token.expose_secret().len()
            ),
            None => format!(
                "received a durable token ({} chars); the server stated no expiry",
                self.token.expose_secret().len()
            ),
        }
    }
}

/// Why an exchange did not happen.
#[derive(Debug, thiserror::Error)]
pub enum ExchangeError {
    /// The declared exchange URL is not on the config URL's origin.
    #[error(
        "the config server asked to exchange the token at `{declared}`, which is not on the same \
         host as the config URL `{config_origin}`. Refusing: that request would send a live \
         credential to a host you did not configure"
    )]
    ForeignOrigin {
        /// Origin of the declared exchange URL, as far as it parsed.
        declared: String,
        /// Origin of the configured config URL.
        config_origin: String,
    },

    /// One of the two URLs could not be parsed into an origin.
    #[error("could not compare origins: {0}")]
    Unparseable(String),

    /// The request itself failed.
    #[error("token exchange request failed: {0}")]
    Transport(String),

    /// The server answered, but not with success.
    #[error("token exchange returned HTTP {status}")]
    Status {
        /// Status code returned.
        status: u16,
    },

    /// The body did not parse, or carried an empty token.
    #[error("token exchange returned a response this client cannot use: {0}")]
    BadResponse(String),
}

/// Scheme, host and port of a URL, for an origin comparison.
///
/// A deliberately small parser rather than a `url` dependency in a
/// crate that has managed without one. It answers one question and
/// refuses anything it does not recognise, which is the right
/// direction to fail for a security check.
pub fn origin_of(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let (scheme, rest) = raw.split_once("://")?;
    if scheme.is_empty() || rest.is_empty() {
        return None;
    }

    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|a| !a.is_empty())?;

    // Userinfo is not part of an origin, and comparing it would make
    // two URLs to the same host look different.
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if host_port.is_empty() {
        return None;
    }

    Some(format!(
        "{}://{}",
        scheme.to_ascii_lowercase(),
        host_port.to_ascii_lowercase()
    ))
}

/// Check that `exchange_url` may be used, given where the config came
/// from.
pub fn check_same_origin(config_url: &str, exchange_url: &str) -> Result<(), ExchangeError> {
    let config_origin = origin_of(config_url)
        .ok_or_else(|| ExchangeError::Unparseable(format!("config URL `{config_url}`")))?;
    let declared = origin_of(exchange_url)
        .ok_or_else(|| ExchangeError::Unparseable(format!("exchange URL `{exchange_url}`")))?;

    if declared == config_origin {
        Ok(())
    } else {
        Err(ExchangeError::ForeignOrigin {
            declared,
            config_origin,
        })
    }
}

/// Trade `bootstrap` for a durable token.
///
/// Verifies the origin first, so a refusal costs no request and — more
/// to the point — does not hand the credential over before deciding
/// whether it should have.
pub async fn exchange(
    config_url: &str,
    exchange_url: &str,
    bootstrap: &str,
) -> Result<ExchangedToken, ExchangeError> {
    check_same_origin(config_url, exchange_url)?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(EXCHANGE_TIMEOUT_SECS))
        .build()
        .map_err(|e| ExchangeError::Transport(e.to_string()))?;

    let response = client
        .post(exchange_url)
        .bearer_auth(bootstrap)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| ExchangeError::Transport(e.to_string()))?;

    let status = response.status();
    if !status.is_success() {
        return Err(ExchangeError::Status {
            status: status.as_u16(),
        });
    }

    let body = response
        .text()
        .await
        .map_err(|e| ExchangeError::Transport(e.to_string()))?;

    parse_response(&body)
}

/// Parse an exchange response body.
///
/// Split out from the request so the shape can be tested without a
/// server, and so a malformed body produces a message naming what was
/// wrong rather than a serde path.
pub fn parse_response(body: &str) -> Result<ExchangedToken, ExchangeError> {
    let parsed: ExchangedToken = serde_json::from_str(body)
        .map_err(|e| ExchangeError::BadResponse(format!("not the expected JSON ({e})")))?;

    if parsed.token.expose_secret().trim().is_empty() {
        return Err(ExchangeError::BadResponse(
            "the `token` field was empty".to_string(),
        ));
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exchange_on_the_same_host_is_allowed() {
        assert!(
            check_same_origin(
                "https://cfg.example.com/api/config/mcp",
                "https://cfg.example.com/api/config/exchange",
            )
            .is_ok()
        );
    }

    /// The check this module exists for. A response naming another
    /// host would otherwise walk a live credential off-site.
    #[test]
    fn an_exchange_on_another_host_is_refused() {
        let err = check_same_origin(
            "https://cfg.example.com/api/config/mcp",
            "https://attacker.example.net/collect",
        )
        .expect_err("must refuse");

        assert!(matches!(err, ExchangeError::ForeignOrigin { .. }));
        assert!(
            err.to_string().contains("did not configure"),
            "the refusal must explain the risk: {err}"
        );
    }

    /// Same host, different scheme or port is a different origin.
    /// Downgrading to plaintext would be the interesting attack.
    #[test]
    fn scheme_and_port_are_part_of_the_origin() {
        assert!(
            check_same_origin("https://cfg.example.com/c", "http://cfg.example.com/e").is_err()
        );
        assert!(
            check_same_origin(
                "https://cfg.example.com/c",
                "https://cfg.example.com:8443/e"
            )
            .is_err()
        );
    }

    #[test]
    fn host_comparison_ignores_case_and_userinfo() {
        assert!(
            check_same_origin(
                "https://user:pw@CFG.Example.com/api/config",
                "https://cfg.example.com/api/exchange",
            )
            .is_ok()
        );
    }

    /// Failing to parse must refuse, not wave through.
    #[test]
    fn an_unparseable_url_is_refused_rather_than_trusted() {
        assert!(matches!(
            check_same_origin("not a url", "https://cfg.example.com/e"),
            Err(ExchangeError::Unparseable(_))
        ));
        assert!(matches!(
            check_same_origin("https://cfg.example.com/c", "also-not-a-url"),
            Err(ExchangeError::Unparseable(_))
        ));
    }

    #[test]
    fn a_well_formed_response_parses() {
        let parsed =
            parse_response(r#"{"token":"durable-abc","expires_at":"2027-01-01T00:00:00Z"}"#)
                .expect("parse");

        assert_eq!(parsed.token.expose_secret(), "durable-abc");
        assert_eq!(parsed.expires_at.as_deref(), Some("2027-01-01T00:00:00Z"));
    }

    /// A server that will not promise an expiry is giving a valid
    /// answer, not a broken one.
    #[test]
    fn an_absent_expiry_is_accepted() {
        let parsed = parse_response(r#"{"token":"durable-abc"}"#).expect("parse");
        assert!(parsed.expires_at.is_none());
        assert!(
            parsed.describe().contains("no expiry"),
            "{}",
            parsed.describe()
        );
    }

    /// An empty token would be stored and then fail every request
    /// afterwards, with nothing pointing back at this moment.
    #[test]
    fn an_empty_token_is_rejected_at_the_door() {
        let err = parse_response(r#"{"token":"   "}"#).expect_err("must reject");
        assert!(err.to_string().contains("empty"), "{err}");
    }

    #[test]
    fn a_non_json_body_is_reported_as_such() {
        let err = parse_response("<html>login</html>").expect_err("must reject");
        assert!(err.to_string().contains("expected JSON"), "{err}");
    }

    /// The description is printed to a terminal, so it must never
    /// carry the token it is describing.
    #[test]
    fn the_description_never_contains_the_token() {
        let t = ExchangedToken {
            token: SecretString::from("durable-supersecret-value".to_owned()),
            expires_at: None,
        };
        assert!(!t.describe().contains("durable-supersecret-value"));
        assert!(t.describe().contains("25 chars"), "{}", t.describe());
    }
}
