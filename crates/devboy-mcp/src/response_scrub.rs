//! Redacting secrets out of upstream tool results before they reach
//! the agent (ADR-024 §4, Ф15).
//!
//! # The route this closes
//!
//! An agent's session transcript is a JSONL file on disk that any
//! process running as the user can read. Everything a tool returns is
//! written to it verbatim and kept. So the question is not whether
//! devboy *stores* a secret, but whether a secret can pass *through*
//! devboy into that file.
//!
//! Our own tools were audited and return no values. The route left
//! open is the one we do not control: a proxied upstream echoing a
//! credential back at us. The commonest shape is banal —
//!
//! ```text
//! 401 Unauthorized: token glpat-xxxxxxxxxxxxxxxxxxxx is expired
//! ```
//!
//! — an error message quoting the very token devboy just sent. The
//! proxy returned that text to the agent unchanged, and it landed in
//! the transcript.
//!
//! # What is matched, and why nothing new is loaded to do it
//!
//! Two passes, from [`devboy_secret_patterns::scrubber`]:
//!
//! - **Credentials this process already sent upstream** — the bearer
//!   token or API key the proxy connected with, and the current OAuth
//!   access token. These are already in this process's memory; the
//!   registry only labels what is there. Nothing is fetched from the
//!   vault or the daemon to build it, because pulling every secret
//!   into the MCP server so it could recognise them would create a
//!   larger exposure than the one being fixed.
//! - **Anything secret-shaped** — the catalogue, which catches an
//!   upstream leaking a credential of its own that devboy has never
//!   seen. Built-ins plus whatever the user declared in
//!   `<config>/secrets/patterns.d/`, which is how an internal token
//!   format nobody upstream has heard of gets caught here too.
//!
//! # Deliberately unconditional
//!
//! There is no opt-out and no allow-list of upstreams. A tool that
//! genuinely means to return a token — an auth debugger, say — will
//! see `[REDACTED:jwt]` instead. That is the intended trade and it is
//! consistent with the rest of the framework: agents work with
//! aliases, never values (ADR-020). The redaction is also visible
//! rather than silent, so the one user it inconveniences can see
//! exactly what happened.
//!
//! # What this does not do
//!
//! It does not write to the audit log. The log lives in the daemon
//! and this runs in the MCP server; routing every proxied response
//! through an RPC would put the daemon on the hot path of every tool
//! call. A leak is reported through `tracing` instead, naming the
//! secret and never the value.

use std::sync::RwLock;

use devboy_secret_patterns::scrubber::{Replacement, Scrubber};
use tracing::warn;

use crate::protocol::{ToolCallResult, ToolResultContent};

/// Label used for the credential a proxy connected with.
pub fn static_token_label(upstream: &str) -> String {
    format!("proxy/{upstream}/token")
}

/// Label used for an OAuth access token.
///
/// Distinct from the static one so a leak report says which of the
/// two appeared, and so a refreshed token replaces its predecessor
/// rather than accumulating.
pub fn oauth_token_label(upstream: &str) -> String {
    format!("proxy/{upstream}/oauth-access-token")
}

/// The credentials one proxy client has sent upstream.
///
/// Small on purpose: at most two entries, both of which the client
/// already holds. This is a *labelling* of existing material, not a
/// new place secrets are collected.
#[derive(Debug, Default)]
pub struct CredentialRegistry {
    entries: RwLock<Vec<(String, String)>>,
}

impl CredentialRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a credential under `label`, replacing any previous
    /// value for that label.
    ///
    /// Replacing rather than appending is what keeps a refreshing
    /// OAuth token from growing the list without bound. It also means
    /// a *previous* access token stops being recognised — an upstream
    /// echoing a token it was given a minute ago would not be caught
    /// by the first pass. The shape pass still catches it, which is
    /// the reason that pass exists.
    pub fn remember(&self, label: impl Into<String>, value: &str) {
        if value.is_empty() {
            return;
        }
        let label = label.into();
        let Ok(mut entries) = self.entries.write() else {
            // A poisoned lock here would mean a panic while holding
            // it. Failing to register is degraded but safe: the shape
            // pass still runs.
            return;
        };
        match entries.iter_mut().find(|(l, _)| *l == label) {
            Some(entry) => entry.1 = value.to_owned(),
            None => entries.push((label, value.to_owned())),
        }
    }

    /// Current `(label, value)` pairs.
    fn snapshot(&self) -> Vec<(String, String)> {
        self.entries
            .read()
            .map(|entries| entries.clone())
            .unwrap_or_default()
    }

    /// How many credentials are registered. For tests and
    /// diagnostics; never renders a value.
    pub fn len(&self) -> usize {
        self.entries.read().map(|e| e.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Build the scrubber for one pass.
///
/// Rebuilt per call rather than cached. It is built over at most two
/// needles and 31 already-compiled regexes, which is microseconds next
/// to the HTTP round trip that produced the text — and a cached
/// scrubber would need invalidating every time an OAuth token
/// refreshed, which is exactly the kind of staleness that turns into a
/// silent hole.
fn scrubber_for(registry: &CredentialRegistry) -> Scrubber {
    Scrubber::new(registry.snapshot()).with_patterns(devboy_secret_patterns::resolved::patterns())
}

/// Redact secrets from a tool result on its way to the agent.
///
/// `upstream` names the proxied server, for the leak warning.
pub fn scrub_tool_result(
    upstream: &str,
    registry: &CredentialRegistry,
    result: ToolCallResult,
) -> ToolCallResult {
    let scrubber = scrubber_for(registry);

    let mut replacements: Vec<Replacement> = Vec::new();
    let content = result
        .content
        .into_iter()
        .map(|block| match block {
            ToolResultContent::Text { text } => {
                let out = scrubber.scrub(&text);
                replacements.extend(out.replacements);
                ToolResultContent::Text { text: out.text }
            }
        })
        .collect();

    if !replacements.is_empty() {
        report_leak(upstream, &replacements);
    }

    ToolCallResult {
        content,
        is_error: result.is_error,
    }
}

/// Redact secrets from a bare string that came from an upstream.
///
/// Used for error messages, which carry response bodies verbatim and
/// reach the agent by a different route than results do.
pub fn scrub_text(upstream: &str, registry: &CredentialRegistry, text: &str) -> String {
    let out = scrubber_for(registry).scrub(text);
    if !out.replacements.is_empty() {
        report_leak(upstream, &out.replacements);
    }
    out.text
}

/// Warn that an upstream returned something that had to be redacted.
///
/// Carries the label and the count, never any part of the value.
/// Separated from the scrub so the wording can be tested without
/// building a proxy client.
fn report_leak(upstream: &str, replacements: &[Replacement]) {
    warn!(
        upstream = %upstream,
        detail = %leak_summary(replacements),
        "an upstream MCP server returned a secret in its response; it was redacted before \
         reaching the agent. The value would otherwise have been written to the agent's \
         session transcript."
    );
}

/// One-line description of what was redacted.
pub fn leak_summary(replacements: &[Replacement]) -> String {
    replacements
        .iter()
        .map(|r| {
            let kind = if r.known { "credential" } else { "pattern" };
            format!("{kind} {} ×{}", r.label, r.count)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_result(text: &str) -> ToolCallResult {
        ToolCallResult::text(text.to_owned())
    }

    fn text_of(result: &ToolCallResult) -> String {
        result
            .content
            .iter()
            .map(|c| match c {
                ToolResultContent::Text { text } => text.clone(),
            })
            .collect()
    }

    /// The exact scenario this module exists for: an upstream quotes
    /// the token it was given back in an error message.
    #[test]
    fn an_upstream_echoing_our_token_does_not_reach_the_agent() {
        let registry = CredentialRegistry::new();
        registry.remember(static_token_label("devboy"), "glpat-ABCDEFGHIJKLMNOPQRSTU");

        let out = scrub_tool_result(
            "devboy",
            &registry,
            text_result("401 Unauthorized: token glpat-ABCDEFGHIJKLMNOPQRSTU is expired"),
        );

        let text = text_of(&out);
        assert!(!text.contains("glpat-ABCDEFGHIJKLMNOPQRSTU"), "{text}");
        assert!(text.contains("@secret:proxy/devboy/token"), "{text}");
    }

    /// A credential devboy has never held is caught by shape. Without
    /// this pass the registry could only recognise what we sent, and
    /// an upstream leaking its own credentials would sail through.
    #[test]
    fn a_credential_devboy_never_sent_is_still_redacted() {
        let registry = CredentialRegistry::new();

        let out = scrub_tool_result(
            "devboy",
            &registry,
            text_result("config dump: aws_key=AKIAIOSFODNN7EXAMPLE region=eu-central-1"),
        );

        let text = text_of(&out);
        assert!(!text.contains("AKIAIOSFODNN7EXAMPLE"), "{text}");
        assert!(text.contains("[REDACTED:aws-access-key]"), "{text}");
        assert!(
            text.contains("eu-central-1"),
            "the rest of the response must survive: {text}"
        );
    }

    /// The overwhelmingly common case. If ordinary results came back
    /// altered, this feature would be turned off within a day.
    #[test]
    fn an_ordinary_result_is_returned_byte_for_byte() {
        let registry = CredentialRegistry::new();
        registry.remember(static_token_label("devboy"), "glpat-ABCDEFGHIJKLMNOPQRSTU");

        let original = "Issue DEV-1234 updated: status in progress, assignee andrey, \
                        commit 08a2981b047b0f8ffa464e80d5486e04ecaee460";
        let out = scrub_tool_result("devboy", &registry, text_result(original));

        assert_eq!(text_of(&out), original);
    }

    /// `is_error` has to survive the rewrite, or a failed upstream
    /// call starts reading as a successful one.
    #[test]
    fn the_error_flag_survives_scrubbing() {
        let registry = CredentialRegistry::new();
        registry.remember(static_token_label("up"), "glpat-ABCDEFGHIJKLMNOPQRSTU");

        let out = scrub_tool_result(
            "up",
            &registry,
            ToolCallResult::error("failed with glpat-ABCDEFGHIJKLMNOPQRSTU".to_owned()),
        );

        assert_eq!(out.is_error, Some(true));
        assert!(!text_of(&out).contains("glpat-"));
    }

    /// A refreshed OAuth token replaces its predecessor rather than
    /// piling up — the registry must stay at two entries however long
    /// the process runs.
    #[test]
    fn a_refreshed_token_replaces_rather_than_accumulates() {
        let registry = CredentialRegistry::new();
        registry.remember(oauth_token_label("up"), "first-access-token-value");
        registry.remember(oauth_token_label("up"), "second-access-token-value");
        registry.remember(static_token_label("up"), "static-token-value");

        assert_eq!(registry.len(), 2);

        let out = scrub_tool_result(
            "up",
            &registry,
            text_result("rejected second-access-token-value"),
        );
        assert!(!text_of(&out).contains("second-access-token-value"));
    }

    /// An empty credential must not be registered: the scrubber skips
    /// short values anyway, but an empty needle would be a bug worth
    /// refusing at the door.
    #[test]
    fn an_empty_credential_is_not_registered() {
        let registry = CredentialRegistry::new();
        registry.remember(static_token_label("up"), "");

        assert!(registry.is_empty());
    }

    /// The warning is the operator's only signal that an upstream is
    /// leaking, so it has to name the secret — and must never carry
    /// the value.
    #[test]
    fn the_leak_summary_names_the_secret_but_not_the_value() {
        let summary = leak_summary(&[
            Replacement {
                label: "proxy/devboy/token".into(),
                known: true,
                count: 2,
            },
            Replacement {
                label: "aws-access-key".into(),
                known: false,
                count: 1,
            },
        ]);

        assert!(summary.contains("proxy/devboy/token"), "{summary}");
        assert!(summary.contains("credential"), "{summary}");
        assert!(summary.contains("aws-access-key"), "{summary}");
        assert!(summary.contains("pattern"), "{summary}");
        assert!(summary.contains("×2"), "{summary}");
    }
}
