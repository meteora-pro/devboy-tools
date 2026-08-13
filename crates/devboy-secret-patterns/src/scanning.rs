//! Turning a *validation* pattern into one that can scan free text.
//!
//! # Why the catalogue cannot scan as it stands
//!
//! Every catalogue pattern is anchored — `^glpat-[A-Za-z0-9_-]{20,}$`
//! — because its first job is to answer "is this string a GitLab
//! token?" when a value is provisioned. Anchors are exactly right for
//! that.
//!
//! They are exactly wrong for the other job. Redacting a token that
//! appears *inside* a log line or a tool response needs a match in the
//! middle of a larger string, and an anchored regex can never produce
//! one. Before this module, [`crate::scrubber::Scrubber`]'s shape-only
//! fallback fed it the anchored regex and therefore fired only when the
//! text was, in its entirety, a single token — never in the cases the
//! fallback exists for.
//!
//! # Why not simply strip the anchors
//!
//! Because one pattern must not be unanchored, and it is the one that
//! would do the most damage. The generic catch-all
//! `^[A-Za-z0-9._-]{40,}$` is a reasonable last-resort validator, and
//! a catastrophic scanner: unanchored it matches a git SHA, a base64
//! blob, a long path segment, a content hash — most of a normal tool
//! response.
//!
//! So promotion is conditional, and the two conditions are what make
//! the result safe to run over text nobody vetted:
//!
//! - **A literal prefix.** At least [`MIN_LITERAL_PREFIX`] literal
//!   characters before any character class or group. `glpat-`, `AKIA`,
//!   `eyJ`, `-----BEGIN` all qualify; `[A-Za-z0-9._-]{40,}` has no
//!   literal prefix at all and is refused. This is the whole defence
//!   against false positives: a scanner has to start with something
//!   distinctive.
//! - **No unbounded wildcard.** A trailing `/.+` is bounded by the end
//!   of the string when anchored, and by nothing at all when it is not
//!   — on a minified JSON response it would swallow every remaining
//!   byte on the line. The four connection-string patterns are refused
//!   for this reason. They keep whole-string validation; scanning them
//!   safely needs a bounded tail written by hand, which is a change to
//!   those patterns rather than to this rule.
//!
//! A pattern that fails either condition is not broken and is not
//! silently degraded — it keeps validating whole strings exactly as
//! before. It simply does not scan.
//!
//! # Over-redaction is the safe direction, but it is not free
//!
//! Both conditions above err toward *not* scanning rather than toward
//! scanning aggressively. That ordering is deliberate: a missed
//! redaction is one leak, while a scanner that mangles ordinary tool
//! output breaks every response it touches and gets the whole feature
//! turned off. `tests::realistic_non_secret_text_is_never_matched`
//! holds that line for patterns added later.

use regex::Regex;

/// Literal characters required at the start of a pattern before it may
/// scan free text.
///
/// Two is enough to disqualify the generic catch-all (which has none)
/// while admitting `gh[pousr]_…`, `AC[a-f0-9]{32}` and `SG\.…`. The
/// corpus test is what actually keeps the bar honest — this constant
/// only decides which patterns get to face it.
pub const MIN_LITERAL_PREFIX: usize = 2;

/// Derive a regex that can find this pattern inside a larger string.
///
/// Returns `None` when the pattern is unfit to scan — see the module
/// docs for the two conditions and why each one exists.
pub fn scanning_regex(source: &str) -> Option<Regex> {
    let body = strip_anchors(source);

    if literal_prefix_len(body) < MIN_LITERAL_PREFIX {
        return None;
    }
    if has_unbounded_wildcard(body) {
        return None;
    }

    Regex::new(body).ok()
}

/// Remove a leading `^` and a trailing unescaped `$`.
///
/// Both are removed only in the anchor position; a `$` in the middle
/// of a pattern is a literal there and is left alone.
fn strip_anchors(source: &str) -> &str {
    let body = source.strip_prefix('^').unwrap_or(source);

    // `\$` is an escaped dollar, not an anchor. Count the backslashes
    // before it: an even number means the `$` itself is unescaped.
    if let Some(without) = body.strip_suffix('$') {
        let escapes = without.chars().rev().take_while(|c| *c == '\\').count();
        if escapes % 2 == 0 {
            return without;
        }
    }

    body
}

/// How many literal characters the pattern starts with.
///
/// Counting stops at the first character that could begin a class, a
/// group, a quantifier or an escape — anything that is not plainly
/// itself. Space is not counted either, so `-----BEGIN RSA…` is
/// measured on `-----BEGIN` alone.
fn literal_prefix_len(body: &str) -> usize {
    body.chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | ':'))
        .count()
}

/// Whether the pattern contains `.` followed by a quantifier.
///
/// A bare `.` matches exactly one character and is harmless. `.+`,
/// `.*` and `.{n,}` are what run away once the trailing anchor is
/// gone. A `.` inside a character class is a literal dot and does not
/// count.
fn has_unbounded_wildcard(body: &str) -> bool {
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;
    let mut in_class = false;

    while i < chars.len() {
        let c = chars[i];

        if c == '\\' {
            i += 2;
            continue;
        }
        if in_class {
            if c == ']' {
                in_class = false;
            }
            i += 1;
            continue;
        }
        if c == '[' {
            in_class = true;
            i += 1;
            continue;
        }
        if c == '.'
            && let Some(next) = chars.get(i + 1)
            && matches!(next, '+' | '*' | '{')
        {
            return true;
        }

        i += 1;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtins;

    /// The property the module exists for: a token embedded in a
    /// sentence is found.
    #[test]
    fn a_promoted_pattern_matches_inside_a_larger_string() {
        let re =
            scanning_regex(r"^glpat-[A-Za-z0-9_-]{20,}$").expect("literal prefix, no wildcard");

        assert!(re.is_match("upstream said: invalid token glpat-ABCDEFGHIJKLMNOPQRSTU here"));
    }

    /// The refusal that matters most. Unanchored, this pattern eats
    /// hashes, paths and base64 — the reason promotion is conditional
    /// at all.
    #[test]
    fn the_generic_catch_all_is_refused() {
        assert!(scanning_regex(r"^[A-Za-z0-9._-]{40,}$").is_none());
    }

    /// A trailing `.+` is bounded by the anchor and by nothing else.
    /// On one-line JSON it would swallow the rest of the response.
    #[test]
    fn an_unbounded_tail_is_refused() {
        assert!(
            scanning_regex(r"^postgres(ql)?://[^:/?#\s@]+:[^@/?#\s]+@[^/?#\s:]+(:[0-9]+)?/.+$")
                .is_none()
        );
    }

    #[test]
    fn a_dot_inside_a_class_is_not_a_wildcard() {
        assert!(!has_unbounded_wildcard(r"[a-z.]+more"));
    }

    #[test]
    fn an_escaped_dot_is_not_a_wildcard() {
        assert!(scanning_regex(r"^SG\.[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]{43}$").is_some());
    }

    /// Already-unanchored patterns — the private-key headers — must
    /// come through unchanged rather than being mangled by the
    /// anchor stripper.
    #[test]
    fn an_unanchored_source_survives_promotion() {
        let re = scanning_regex(r"-----BEGIN RSA PRIVATE KEY-----").expect("literal prefix");
        assert!(re.is_match("blah\n-----BEGIN RSA PRIVATE KEY-----\nblah"));
    }

    #[test]
    fn a_literal_dollar_is_not_treated_as_an_anchor() {
        assert_eq!(strip_anchors(r"^ab\$"), r"ab\$");
        assert_eq!(strip_anchors(r"^ab$"), "ab");
    }

    /// Which catalogue patterns may scan is a security-relevant fact,
    /// so a change to it should show up in a diff rather than in
    /// behaviour. If this count moves, the corpus test below is the
    /// one that decides whether the move was safe.
    #[test]
    fn the_scannable_share_of_the_catalogue_is_pinned() {
        let total = builtins().count();
        let scannable = builtins().filter(|p| p.scan_regex().is_some()).count();

        assert_eq!(total, 31, "catalogue size changed");
        assert_eq!(
            scannable, 26,
            "the set of patterns allowed to scan free text changed — five are refused on \
             purpose: the generic catch-all and the four connection strings"
        );
    }

    /// The guard that keeps a future pattern from turning every tool
    /// response into confetti. Every string here is ordinary output a
    /// user would be furious to see redacted.
    #[test]
    fn realistic_non_secret_text_is_never_matched() {
        let corpus = [
            // Git object names, short and full.
            "commit 08a2981b047b0f8ffa464e80d5486e04ecaee460 by Andrey",
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            // A UUID and a ULID.
            "request_id=21c73f48-9f84-4a39-92fb-7be85ba15718",
            "01KZASPFHXXF31KAK1P3NJNVA2",
            // Paths, module names and URLs without credentials.
            "/home/titan/projects/meteora/devboy-tools/crates/devboy-secret-patterns/src/lib.rs",
            "https://github.com/meteora-pro/devboy-tools/blob/main/README.md",
            "crates::devboy_storage::plugin_client::tests::negotiated_capabilities",
            // Base64 that is not a token.
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk",
            // Prose that happens to contain provider names.
            "The GitHub API returned 404 for that repository, check the org name.",
            "Slack webhook delivery is disabled for this workspace.",
            // Semver, timestamps, hex dumps.
            "devboy-core 0.34.0 built 2026-08-13T10:22:31Z",
            "00 1f 8b 08 00 00 00 00 00 00 03 ed 5d 6b 73 db 38",
        ];

        for text in corpus {
            for pattern in builtins() {
                let Some(re) = pattern.scan_regex() else {
                    continue;
                };
                assert!(
                    !re.is_match(text),
                    "pattern `{}` matched ordinary text — scanning it would corrupt tool \
                     output:\n  text: {text}\n  match: {:?}",
                    pattern.id(),
                    re.find(text).map(|m| m.as_str()),
                );
            }
        }
    }

    /// The other half of the guard: real tokens must still be caught
    /// once they are embedded in exactly the kind of line an upstream
    /// error produces.
    #[test]
    fn real_tokens_embedded_in_upstream_errors_are_matched() {
        let cases = [
            (
                "gitlab-pat",
                "401: token glpat-ABCDEFGHIJKLMNOPQRSTU rejected",
            ),
            (
                "github-pat",
                "bad credentials for ghp_0123456789012345678901234567890123456 (401)",
            ),
            ("aws-access-key", "using AKIAIOSFODNN7EXAMPLE in region"),
            (
                "jwt",
                "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
            ),
        ];

        for (id, text) in cases {
            let pattern = crate::find(id).unwrap_or_else(|| panic!("no pattern `{id}`"));
            let re = pattern
                .scan_regex()
                .unwrap_or_else(|| panic!("pattern `{id}` should be scannable"));
            assert!(re.is_match(text), "`{id}` missed: {text}");
        }
    }
}
