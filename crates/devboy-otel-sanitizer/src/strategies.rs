//! Application of redaction strategies to a matched value.
//!
//! Given a [`Strategy`] and the original input + match span, produce the
//! redacted output. Pure functions, no I/O.

use sha2::{Digest, Sha256};

use crate::strategy::Strategy;
use crate::validity::{shannon_entropy, Encoding};

/// Apply a strategy to the entire input string.
///
/// `match_range` describes the byte span (start, end-exclusive) inside
/// `input` where the rule matched. For `Mask` / `RegexRedact` only the
/// matched bytes are replaced; for `Drop` the result is `None` (caller
/// should drop the whole field); for other strategies see per-variant
/// docs in [`Strategy`].
///
/// Returns:
/// - `Some(redacted)` — the value to substitute back into the OTLP tree
/// - `None` — caller should drop the field (used by `Drop` and by
///   `EntropyFilter` when threshold is met).
pub fn apply(strategy: &Strategy, input: &str, match_range: (usize, usize)) -> Option<String> {
    let (start, end) = match_range;
    let matched = &input[start..end];

    match strategy {
        Strategy::Allow => Some(input.to_string()),
        Strategy::Drop => None,
        Strategy::Hash => {
            let mut h = Sha256::new();
            h.update(matched.as_bytes());
            let digest = h.finalize();
            let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
            Some(splice(input, start, end, &format!("[HASH:{hex}]")))
        }
        Strategy::Mask { replacement } => {
            let r = replacement.as_deref().unwrap_or("[REDACTED]");
            Some(splice(input, start, end, r))
        }
        Strategy::RegexRedact { replacement } => {
            // For non-capture-group case behaves like Mask. Capture-group
            // handling is regex-specific and applied by the engine before
            // calling here; this fallback masks the full match.
            let r = replacement.as_deref().unwrap_or("[REDACTED]");
            Some(splice(input, start, end, r))
        }
        Strategy::Truncate { max_chars } => {
            if input.chars().count() <= *max_chars {
                return Some(input.to_string());
            }
            let truncated: String = input.chars().take(*max_chars).collect();
            Some(format!("{truncated}…"))
        }
        Strategy::ScopeToWorkspace => Some(scope_path(input)),
        Strategy::EntropyFilter { min_entropy } => {
            let encoding = Encoding::detect(matched);
            let entropy = shannon_entropy(matched);
            let threshold = if *min_entropy > 0.0 {
                *min_entropy
            } else {
                encoding.entropy_threshold()
            };
            if entropy >= threshold {
                None
            } else {
                Some(input.to_string())
            }
        }
    }
}

/// Replace `input[start..end]` with `replacement`.
fn splice(input: &str, start: usize, end: usize, replacement: &str) -> String {
    let mut out = String::with_capacity(input.len() - (end - start) + replacement.len());
    out.push_str(&input[..start]);
    out.push_str(replacement);
    out.push_str(&input[end..]);
    out
}

/// Replace common user home prefixes with `<workspace>` placeholder.
/// Examples:
///   `/Users/alice/projects/foo/bar.rs` → `<workspace>/foo/bar.rs`
///   `/home/bob/.cache/x` → `<workspace>/.cache/x`
fn scope_path(input: &str) -> String {
    // Drop two leading components after /Users or /home.
    let trimmed = input
        .strip_prefix("/Users/")
        .or_else(|| input.strip_prefix("/home/"));
    if let Some(rest) = trimmed {
        if let Some(after_user) = rest.split_once('/') {
            return format!("<workspace>/{}", after_user.1);
        }
        return "<workspace>/".to_string();
    }
    input.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_default_replacement() {
        let s = Strategy::Mask { replacement: None };
        // input len=16; range (4,16) covers "AKIA1234ABCD" (12 chars) →
        // splice replaces the suffix with "[REDACTED]".
        let out = apply(&s, "key=AKIA1234ABCD", (4, 16));
        assert_eq!(out, Some("key=[REDACTED]".to_string()));
    }

    #[test]
    fn mask_custom_replacement() {
        let s = Strategy::Mask {
            replacement: Some("***".to_string()),
        };
        let out = apply(&s, "abcXYZdef", (3, 6));
        assert_eq!(out, Some("abc***def".to_string()));
    }

    #[test]
    fn drop_returns_none() {
        let out = apply(&Strategy::Drop, "anything", (0, 8));
        assert!(out.is_none());
    }

    #[test]
    fn hash_replaces_with_short_digest() {
        let out = apply(&Strategy::Hash, "secret=value", (7, 12));
        let v = out.unwrap();
        assert!(v.starts_with("secret=[HASH:"));
        assert!(v.ends_with(']'));
        assert_eq!(v.len(), "secret=".len() + "[HASH:".len() + 16 + "]".len());
    }

    #[test]
    fn truncate_short_passthrough() {
        let s = Strategy::Truncate { max_chars: 100 };
        let out = apply(&s, "short", (0, 5));
        assert_eq!(out, Some("short".to_string()));
    }

    #[test]
    fn truncate_long_appends_ellipsis() {
        let s = Strategy::Truncate { max_chars: 5 };
        let out = apply(&s, "0123456789", (0, 10)).unwrap();
        assert_eq!(out, "01234…");
    }

    #[test]
    fn scope_to_workspace_users() {
        let out = apply(
            &Strategy::ScopeToWorkspace,
            "/Users/alice/projects/foo/bar.rs",
            (0, 32),
        );
        assert_eq!(out, Some("<workspace>/projects/foo/bar.rs".to_string()));
    }

    #[test]
    fn scope_to_workspace_home() {
        let out = apply(&Strategy::ScopeToWorkspace, "/home/bob/.cache/x", (0, 18));
        assert_eq!(out, Some("<workspace>/.cache/x".to_string()));
    }

    #[test]
    fn scope_to_workspace_no_prefix() {
        // Non-home paths pass through.
        let out = apply(&Strategy::ScopeToWorkspace, "/etc/hosts", (0, 10));
        assert_eq!(out, Some("/etc/hosts".to_string()));
    }

    #[test]
    fn entropy_filter_drops_high_entropy() {
        let s = Strategy::EntropyFilter { min_entropy: 0.0 }; // use default per encoding
        // Random-looking 32-char base64-shape value: high entropy > 4.5
        let value = "kJq7mZx9P2vN5yT8wE3rB6aH4dF1sLcQ";
        let out = apply(&s, value, (0, value.len()));
        assert!(out.is_none(), "high-entropy match should be dropped");
    }

    #[test]
    fn entropy_filter_keeps_low_entropy() {
        let s = Strategy::EntropyFilter { min_entropy: 0.0 };
        let out = apply(&s, "aaaaaaaaaa", (0, 10));
        assert_eq!(out, Some("aaaaaaaaaa".to_string()));
    }
}
